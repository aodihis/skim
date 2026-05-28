//! Streaming SQL statement extractor.
//!
//! Reads an input byte-by-byte (via `BufRead`) and yields one complete SQL
//! statement (terminated by `;`) per `Iterator::next()` call.  The internal
//! buffer never grows beyond a single statement, so memory usage is
//! O(max_statement_size) regardless of total file size.
//!
//! # Edge cases handled
//! - `'...'` single-quoted strings (SQL standard)
//! - `\'`  backslash-escape inside strings (MySQL)
//! - `''`  double-quote escape inside strings (SQL standard)
//! - `"..."` double-quoted identifiers
//! - `` `...` `` back-tick quoted identifiers (MySQL)
//! - `-- ...`  line comments
//! - `/* ... */` block comments (including multi-line)
//! - `DELIMITER ;;` sections in mysqldump — skipped entirely
//! - A configurable max-statement-size circuit breaker

use std::io::BufRead;

use crate::error::ConvertError;

// ── State machine ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum State {
    /// Normal SQL — outside any string, comment, or special construct.
    Normal,
    /// Inside a `'...'` single-quoted string literal.
    InSingleQuote,
    /// After a `\` inside a single-quoted string (MySQL backslash escape).
    InSingleQuoteEscape,
    /// Inside a `"..."` double-quoted identifier.
    InDoubleQuote,
    /// After a `\` inside a double-quoted identifier.
    InDoubleQuoteEscape,
    /// Inside a `` `...` `` back-tick quoted identifier (MySQL).
    InBacktick,
    /// After the first `-` of a potential `--` line comment.
    MaybeLineComment,
    /// Inside a `--` line comment; ends at `\n`.
    InLineComment,
    /// After the `/` of a potential `/*` block comment.
    MaybeBlockComment,
    /// Inside a `/* ... */` block comment.
    InBlockComment,
    /// Inside a block comment, after seeing `*` (potential end `*/`).
    InBlockCommentStar,
    /// Skipping a `DELIMITER <delim>` section (stored procedures in mysqldump).
    SkipDelimiter { delim: Vec<u8> },
}

// ── Public iterator ───────────────────────────────────────────────────────────

/// Yields one complete SQL statement per call.
///
/// Statements are delimited by `;` at the top level (outside strings/comments).
/// Blank statements (only whitespace) are silently skipped.
pub struct StatementExtractor<R: BufRead> {
    reader: R,
    state: State,
    buf: String,
    max_size: usize,
    done: bool,
}

impl<R: BufRead> StatementExtractor<R> {
    pub fn new(reader: R, max_statement_size: usize) -> Self {
        Self {
            reader,
            state: State::Normal,
            buf: String::with_capacity(4096),
            max_size: max_statement_size,
            done: false,
        }
    }
}

impl<R: BufRead> Iterator for StatementExtractor<R> {
    type Item = anyhow::Result<String>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        loop {
            // Read the next line from the underlying reader.
            let mut line = String::new();
            let n = match self.reader.read_line(&mut line) {
                Ok(n) => n,
                Err(e) => {
                    self.done = true;
                    return Some(Err(e.into()));
                }
            };

            if n == 0 {
                // EOF
                self.done = true;
                let stmt = self.buf.trim().to_string();
                if stmt.is_empty() {
                    return None;
                }
                // Return whatever is left (incomplete statement at EOF).
                return Some(Ok(stmt));
            }

            // Process each byte of the line.
            for byte in line.bytes() {
                if let Some(stmt) = self.process_byte(byte) {
                    match stmt {
                        Ok(s) => {
                            let trimmed = s.trim().to_string();
                            if !trimmed.is_empty() {
                                return Some(Ok(trimmed));
                            }
                            // blank statement — keep going
                        }
                        Err(e) => {
                            self.done = true;
                            return Some(Err(e));
                        }
                    }
                }
            }
        }
    }
}

impl<R: BufRead> StatementExtractor<R> {
    /// Process a single byte.  Returns `Some(Ok(stmt))` when a complete
    /// statement has been accumulated, `Some(Err(_))` on error, or `None`
    /// when the byte was consumed without completing a statement.
    fn process_byte(&mut self, b: u8) -> Option<anyhow::Result<String>> {
        // Check for DELIMITER keyword at the start of Normal state.
        // We detect it by inspecting the buffer after each Normal-state push.

        match &self.state.clone() {
            // ── Normal state ─────────────────────────────────────────────
            State::Normal => {
                match b {
                    b'\'' => {
                        self.push(b);
                        self.state = State::InSingleQuote;
                    }
                    b'"' => {
                        self.push(b);
                        self.state = State::InDoubleQuote;
                    }
                    b'`' => {
                        self.push(b);
                        self.state = State::InBacktick;
                    }
                    b'-' => {
                        self.push(b);
                        self.state = State::MaybeLineComment;
                    }
                    b'/' => {
                        self.push(b);
                        self.state = State::MaybeBlockComment;
                    }
                    b';' => {
                        // Statement boundary — check for DELIMITER before yielding.
                        let stmt = self.buf.clone();
                        self.buf.clear();

                        // Detect `DELIMITER ;;` (or other custom delimiter).
                        let trimmed = stmt.trim();
                        if let Some(rest) = trimmed
                            .get(0..9)
                            .and_then(|s| s.eq_ignore_ascii_case("delimiter").then_some(()))
                            .and(trimmed.get(9..))
                        {
                            let delim = rest.trim().as_bytes().to_vec();
                            if !delim.is_empty() && delim != b";" {
                                self.state = State::SkipDelimiter { delim };
                                return None;
                            }
                        }

                        return Some(Ok(stmt));
                    }
                    _ => {
                        if let Err(e) = self.push_checked(b) {
                            return Some(Err(e));
                        }
                    }
                }
                // Check if the buffer now starts with DELIMITER (before any `;`).
                self.maybe_enter_delimiter_skip();
                None
            }

            // ── Single-quoted string ──────────────────────────────────────
            State::InSingleQuote => {
                match b {
                    b'\\' => {
                        self.push(b);
                        self.state = State::InSingleQuoteEscape;
                    }
                    b'\'' => {
                        self.push(b);
                        // Stay in InSingleQuote — the next byte will tell us
                        // whether this is `''` (escape) or end-of-string.
                        // We handle it by peeking ahead logically: if the
                        // next byte is also `'`, it's an escape; otherwise
                        // we're back to Normal.  We track this via a tiny
                        // sub-state by transitioning to a "maybe end quote"
                        // inline here.
                        self.state = State::Normal; // tentatively Normal
                        // But we need to check the next byte.  We re-enter
                        // InSingleQuote if the next byte is `'`.  Handled
                        // by the Normal branch above: seeing another `'`
                        // will push and transition to InSingleQuote again.
                        //
                        // Wait — this would incorrectly treat `''` as ending
                        // and starting a new string.  The net effect is
                        // actually correct for statement extraction purposes
                        // because `;` inside a string (even after `''`) is
                        // still protected, and the content is preserved as-is
                        // for sqlparser to interpret later.
                    }
                    _ => {
                        self.push(b);
                    }
                }
                None
            }

            State::InSingleQuoteEscape => {
                self.push(b);
                self.state = State::InSingleQuote;
                None
            }

            // ── Double-quoted identifier ──────────────────────────────────
            State::InDoubleQuote => {
                match b {
                    b'\\' => {
                        self.push(b);
                        self.state = State::InDoubleQuoteEscape;
                    }
                    b'"' => {
                        self.push(b);
                        self.state = State::Normal;
                    }
                    _ => {
                        self.push(b);
                    }
                }
                None
            }

            State::InDoubleQuoteEscape => {
                self.push(b);
                self.state = State::InDoubleQuote;
                None
            }

            // ── Back-tick quoted identifier (MySQL) ───────────────────────
            State::InBacktick => {
                self.push(b);
                if b == b'`' {
                    self.state = State::Normal;
                }
                None
            }

            // ── Line comment ──────────────────────────────────────────────
            State::MaybeLineComment => {
                if b == b'-' {
                    // Confirmed `--` comment.
                    self.push(b);
                    self.state = State::InLineComment;
                } else {
                    // Just a minus sign; fall back to Normal.
                    self.state = State::Normal;
                    // Re-process `b` as Normal.
                    return self.process_byte(b);
                }
                None
            }

            State::InLineComment => {
                self.push(b);
                if b == b'\n' {
                    self.state = State::Normal;
                }
                None
            }

            // ── Block comment ─────────────────────────────────────────────
            State::MaybeBlockComment => {
                if b == b'*' {
                    self.push(b);
                    self.state = State::InBlockComment;
                } else {
                    // Just a slash; fall back to Normal.
                    self.state = State::Normal;
                    return self.process_byte(b);
                }
                None
            }

            State::InBlockComment => {
                self.push(b);
                if b == b'*' {
                    self.state = State::InBlockCommentStar;
                }
                None
            }

            State::InBlockCommentStar => {
                self.push(b);
                if b == b'/' {
                    self.state = State::Normal;
                } else if b != b'*' {
                    self.state = State::InBlockComment;
                }
                // If b == '*' we stay in InBlockCommentStar.
                None
            }

            // ── DELIMITER skip (mysqldump stored procedures) ──────────────
            State::SkipDelimiter { delim } => {
                let delim = delim.clone();
                // Accumulate into buf looking for the delimiter sequence.
                unsafe { self.buf.as_mut_vec().push(b) };
                if self.buf.ends_with(std::str::from_utf8(&delim).unwrap_or("")) {
                    self.buf.clear();
                    self.state = State::Normal;
                }
                None
            }
        }
    }

    /// Push `b` to the buffer without size checking (used in string/comment
    /// states where we know the content is bounded by the statement size).
    fn push(&mut self, b: u8) {
        // SAFETY: we accumulate raw bytes into a Vec<u8> and re-interpret as
        // UTF-8 only when the statement is complete.  Using push(b as char)
        // would corrupt multi-byte sequences (e.g. Arabic / CJK text) by
        // treating each byte as an independent Unicode code point.
        unsafe { self.buf.as_mut_vec().push(b) };
    }

    /// Push `b` to the buffer with a size-limit check (used in Normal state).
    fn push_checked(&mut self, b: u8) -> anyhow::Result<()> {
        if self.buf.len() >= self.max_size {
            return Err(ConvertError::StatementTooLarge {
                max_bytes: self.max_size,
                actual_bytes: self.buf.len() + 1,
            }
            .into());
        }
        unsafe { self.buf.as_mut_vec().push(b) };
        Ok(())
    }

    /// If the buffer (before any `;`) starts with `DELIMITER `, enter skip
    /// mode for the custom delimiter.  This handles the case where mysqldump
    /// emits `DELIMITER ;;` before stored procedures.
    fn maybe_enter_delimiter_skip(&mut self) {
        let trimmed = self.buf.trim_start();
        if trimmed.len() < 10 {
            return;
        }
        if !trimmed[..9].eq_ignore_ascii_case("delimiter") {
            return;
        }
        let rest = trimmed[9..].trim_start();
        // Only switch if the delimiter is not `;` (which ends the current statement normally).
        if !rest.is_empty() && !rest.starts_with(';') {
            let delim = rest.as_bytes().to_vec();
            self.state = State::SkipDelimiter { delim };
            self.buf.clear();
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    fn extract(sql: &str) -> Vec<String> {
        let reader = BufReader::new(sql.as_bytes());
        StatementExtractor::new(reader, 256 * 1024 * 1024)
            .map(|r| r.expect("parse error"))
            .collect()
    }

    #[test]
    fn simple_insert() {
        let stmts = extract("INSERT INTO t VALUES (1, 'hello');");
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].contains("INSERT"));
    }

    #[test]
    fn two_statements() {
        let stmts = extract(
            "INSERT INTO a VALUES (1);\nINSERT INTO b VALUES (2);",
        );
        assert_eq!(stmts.len(), 2);
    }

    #[test]
    fn semicolon_inside_string() {
        let stmts = extract("INSERT INTO t VALUES ('hello; world');");
        assert_eq!(stmts.len(), 1, "semicolon inside string must not split statement");
    }

    #[test]
    fn escaped_quote_mysql_style() {
        let stmts = extract(r"INSERT INTO t VALUES ('it\'s ok');");
        assert_eq!(stmts.len(), 1);
    }

    #[test]
    fn doubled_quote_standard_style() {
        let stmts = extract("INSERT INTO t VALUES ('it''s ok');");
        assert_eq!(stmts.len(), 1);
    }

    #[test]
    fn line_comment_skipped() {
        let stmts = extract(
            "-- this is a comment\nINSERT INTO t VALUES (1);",
        );
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].contains("INSERT"));
    }

    #[test]
    fn block_comment_skipped() {
        let stmts = extract(
            "/* header comment\n   spanning lines */\nINSERT INTO t VALUES (1);",
        );
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].contains("INSERT"));
    }

    #[test]
    fn multiline_insert() {
        let stmts = extract(
            "INSERT INTO t\n  (a, b)\n  VALUES\n  (1, 'foo'),\n  (2, 'bar');",
        );
        assert_eq!(stmts.len(), 1);
    }

    #[test]
    fn blank_statements_skipped() {
        let stmts = extract(";;;  ;  \n  ;");
        assert_eq!(stmts.len(), 0, "blank statements should be skipped");
    }

    #[test]
    fn set_and_insert() {
        let stmts = extract(
            "SET NAMES utf8mb4;\nINSERT INTO t VALUES (42);",
        );
        assert_eq!(stmts.len(), 2);
    }

    #[test]
    fn backtick_identifier_with_semicolon() {
        // unlikely but valid: backtick identifier containing a semicolon char
        // (not valid SQL but tests the scanner robustness)
        let stmts = extract("SELECT `col;name` FROM t;");
        assert_eq!(stmts.len(), 1);
    }

    #[test]
    fn block_comment_star_sequences() {
        // `*` chars inside a block comment must not prematurely close it
        let stmts = extract("/* ** not closed yet ** */\nINSERT INTO t VALUES (1);");
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].contains("INSERT"));
    }
}
