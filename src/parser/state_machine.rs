//! Streaming SQL statement extractor.
//!
//! Reads input through `BufRead` and yields one complete SQL statement
//! (terminated by `;`) per `Iterator::next()` call.  The internal
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
    buf: Vec<u8>,
    max_size: usize,
    done: bool,
}

impl<R: BufRead> StatementExtractor<R> {
    pub fn new(reader: R, max_statement_size: usize) -> Self {
        Self {
            reader,
            state: State::Normal,
            buf: Vec::with_capacity(4096),
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
                let stmt = String::from_utf8_lossy(&self.buf).trim().to_string();
                if stmt.is_empty() {
                    return None;
                }
                // Return whatever is left (incomplete statement at EOF).
                return Some(Ok(stmt));
            }

            if self.try_consume_delimiter_directive(&line) {
                continue;
            }

            if let Some(stmt) = self.process_bytes(line.as_bytes()) {
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

impl<R: BufRead> StatementExtractor<R> {
    /// Process a line/slice in chunks.  The state machine still transitions on
    /// individual SQL syntax bytes, but long runs of ordinary SQL text and
    /// quoted string content are copied in one go.
    fn process_bytes(&mut self, bytes: &[u8]) -> Option<anyhow::Result<String>> {
        let mut i = 0;
        while i < bytes.len() {
            match self.state {
                State::Normal => {
                    let rest = &bytes[i..];
                    let special = rest.iter().position(|b| is_normal_special(*b));
                    match special {
                        Some(0) => {
                            i += 1;
                            if let Some(stmt) = self.process_byte(rest[0]) {
                                return Some(stmt);
                            }
                        }
                        Some(pos) => {
                            if let Err(e) = self.push_slice_checked(&rest[..pos]) {
                                return Some(Err(e));
                            }
                            self.maybe_enter_delimiter_skip();
                            i += pos;
                        }
                        None => {
                            if let Err(e) = self.push_slice_checked(rest) {
                                return Some(Err(e));
                            }
                            self.maybe_enter_delimiter_skip();
                            return None;
                        }
                    }
                }

                State::InSingleQuote => {
                    let rest = &bytes[i..];
                    let special = rest.iter().position(|b| *b == b'\\' || *b == b'\'');
                    match special {
                        Some(0) => {
                            let b = rest[0];
                            self.push(b);
                            i += 1;
                            if b == b'\\' {
                                if i < bytes.len() {
                                    self.push(bytes[i]);
                                    i += 1;
                                } else {
                                    self.state = State::InSingleQuoteEscape;
                                }
                            } else {
                                self.state = State::Normal;
                            }
                        }
                        Some(pos) => {
                            self.push_slice(&rest[..pos]);
                            i += pos;
                        }
                        None => {
                            self.push_slice(rest);
                            return None;
                        }
                    }
                }

                State::InDoubleQuote => {
                    let rest = &bytes[i..];
                    let special = rest.iter().position(|b| *b == b'\\' || *b == b'"');
                    match special {
                        Some(0) => {
                            let b = rest[0];
                            self.push(b);
                            i += 1;
                            if b == b'\\' {
                                if i < bytes.len() {
                                    self.push(bytes[i]);
                                    i += 1;
                                } else {
                                    self.state = State::InDoubleQuoteEscape;
                                }
                            } else {
                                self.state = State::Normal;
                            }
                        }
                        Some(pos) => {
                            self.push_slice(&rest[..pos]);
                            i += pos;
                        }
                        None => {
                            self.push_slice(rest);
                            return None;
                        }
                    }
                }

                State::InBacktick => {
                    let rest = &bytes[i..];
                    match rest.iter().position(|b| *b == b'`') {
                        Some(pos) => {
                            self.push_slice(&rest[..=pos]);
                            self.state = State::Normal;
                            i += pos + 1;
                        }
                        None => {
                            self.push_slice(rest);
                            return None;
                        }
                    }
                }

                State::InLineComment => {
                    let rest = &bytes[i..];
                    match rest.iter().position(|b| *b == b'\n') {
                        Some(pos) => {
                            self.push_slice(&rest[..=pos]);
                            self.state = State::Normal;
                            i += pos + 1;
                        }
                        None => {
                            self.push_slice(rest);
                            return None;
                        }
                    }
                }

                _ => {
                    let b = bytes[i];
                    i += 1;
                    if let Some(stmt) = self.process_byte(b) {
                        return Some(stmt);
                    }
                }
            }
        }
        None
    }

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
                        let bytes = std::mem::take(&mut self.buf);
                        let stmt = String::from_utf8_lossy(&bytes).into_owned();

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
                        //
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
                        self.state = State::Normal; // tentatively Normal
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
                // `delim` borrows from the cloned state temporary, not from self,
                // so mutating self.buf below is safe without an extra clone.
                self.buf.push(b);
                if self.buf.ends_with(delim.as_slice()) {
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
        self.buf.push(b);
    }

    fn push_slice(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
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
        self.buf.push(b);
        Ok(())
    }

    fn push_slice_checked(&mut self, bytes: &[u8]) -> anyhow::Result<()> {
        if self.buf.len() + bytes.len() > self.max_size {
            return Err(ConvertError::StatementTooLarge {
                max_bytes: self.max_size,
                actual_bytes: self.buf.len() + bytes.len(),
            }
            .into());
        }
        self.buf.extend_from_slice(bytes);
        Ok(())
    }

    /// If the buffer (before any `;`) starts with `DELIMITER `, enter skip
    /// mode for the custom delimiter.  This handles the case where mysqldump
    /// emits `DELIMITER ;;` before stored procedures.
    fn maybe_enter_delimiter_skip(&mut self) {
        // DELIMITER directives are short standalone commands.  Once a normal
        // statement has grown past this small prefix window, repeatedly
        // decoding the full buffer turns large INSERT lines into O(n^2) work.
        if self.buf.len() > 128 {
            return;
        }
        // DELIMITER is always ASCII and only appears before any string content,
        // so from_utf8 will succeed here; fall back to ignoring on error.
        let Ok(s) = std::str::from_utf8(&self.buf) else {
            return;
        };
        let trimmed = s.trim_start();
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

    fn try_consume_delimiter_directive(&mut self, line: &str) -> bool {
        if self.state != State::Normal {
            return false;
        }
        if !self.buf.iter().all(|b| b.is_ascii_whitespace()) {
            return false;
        }

        let trimmed = line.trim_start();
        if trimmed.len() < 9 || !trimmed[..9].eq_ignore_ascii_case("delimiter") {
            return false;
        }
        let rest = trimmed[9..].trim();
        if rest.is_empty() {
            return false;
        }

        self.buf.clear();
        if rest != ";" {
            self.state = State::SkipDelimiter {
                delim: rest.as_bytes().to_vec(),
            };
        }
        true
    }
}

fn is_normal_special(b: u8) -> bool {
    matches!(b, b'\'' | b'"' | b'`' | b'-' | b'/' | b';')
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, BufReader, Read};

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
        let stmts = extract("INSERT INTO a VALUES (1);\nINSERT INTO b VALUES (2);");
        assert_eq!(stmts.len(), 2);
    }

    #[test]
    fn semicolon_inside_string() {
        let stmts = extract("INSERT INTO t VALUES ('hello; world');");
        assert_eq!(
            stmts.len(),
            1,
            "semicolon inside string must not split statement"
        );
    }

    #[test]
    fn arabic_text_with_semicolon_inside_string() {
        let stmts = extract("INSERT INTO t VALUES ('باب العلم؛ ثم المزيد; داخل النص');");
        assert_eq!(stmts.len(), 1, "Arabic text inside string must stay intact");
        assert!(stmts[0].contains("باب العلم"));
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
        let stmts = extract("-- this is a comment\nINSERT INTO t VALUES (1);");
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].contains("INSERT"));
    }

    #[test]
    fn block_comment_skipped() {
        let stmts = extract("/* header comment\n   spanning lines */\nINSERT INTO t VALUES (1);");
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].contains("INSERT"));
    }

    #[test]
    fn multiline_insert() {
        let stmts = extract("INSERT INTO t\n  (a, b)\n  VALUES\n  (1, 'foo'),\n  (2, 'bar');");
        assert_eq!(stmts.len(), 1);
    }

    #[test]
    fn blank_statements_skipped() {
        let stmts = extract(";;;  ;  \n  ;");
        assert_eq!(stmts.len(), 0, "blank statements should be skipped");
    }

    #[test]
    fn set_and_insert() {
        let stmts = extract("SET NAMES utf8mb4;\nINSERT INTO t VALUES (42);");
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

    #[test]
    fn incomplete_statement_at_eof_is_returned_once() {
        let reader = BufReader::new("INSERT INTO t VALUES (1)".as_bytes());
        let mut extractor = StatementExtractor::new(reader, 1024);

        assert_eq!(
            extractor.next().unwrap().unwrap(),
            "INSERT INTO t VALUES (1)",
        );
        assert!(extractor.next().is_none());
    }

    #[test]
    fn done_iterator_stays_done() {
        let reader = BufReader::new("".as_bytes());
        let mut extractor = StatementExtractor::new(reader, 1024);

        assert!(extractor.next().is_none());
        assert!(extractor.next().is_none());
    }

    #[test]
    fn reader_error_is_returned_and_stops_iteration() {
        struct FailingReader;

        impl Read for FailingReader {
            fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("boom"))
            }
        }

        impl BufRead for FailingReader {
            fn fill_buf(&mut self) -> io::Result<&[u8]> {
                Err(io::Error::other("boom"))
            }

            fn consume(&mut self, _amt: usize) {}
        }

        let mut extractor = StatementExtractor::new(FailingReader, 1024);

        let err = extractor.next().unwrap().unwrap_err();
        assert!(err.to_string().contains("boom"));
        assert!(extractor.next().is_none());
    }

    #[test]
    fn statement_size_limit_errors_for_long_normal_text() {
        let reader = BufReader::new("INSERT INTO t VALUES (12345);".as_bytes());
        let mut extractor = StatementExtractor::new(reader, 10);

        let err = extractor.next().unwrap().unwrap_err();
        assert!(
            err.to_string().contains("exceeds maximum size"),
            "unexpected error: {err}",
        );
        assert!(extractor.next().is_none());
    }

    #[test]
    fn double_quoted_identifier_with_escaped_quote_across_line_boundary() {
        let stmts = extract("SELECT \"a\\\n;still identifier\" FROM t;\nSELECT 2;");

        assert_eq!(stmts.len(), 2);
        assert!(stmts[0].contains(";still identifier"));
    }

    #[test]
    fn minus_and_slash_not_starting_comments_are_normal_sql() {
        let stmts = extract("SELECT 4-2;\nSELECT 8/4;");

        assert_eq!(stmts, ["SELECT 4-2", "SELECT 8/4"]);
    }

    #[test]
    fn mysql_custom_delimiter_section_is_skipped() {
        let stmts = extract(
            "DELIMITER //\n\
             CREATE PROCEDURE p()\n\
             BEGIN\n\
               SELECT 1;\n\
             END//\n\
             DELIMITER ;\n\
             INSERT INTO t VALUES (1);",
        );

        assert_eq!(stmts, ["INSERT INTO t VALUES (1)"]);
    }
}
