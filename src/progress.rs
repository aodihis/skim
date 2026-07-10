use std::io::{self, BufRead, Read};

use indicatif::{ProgressBar, ProgressStyle};

/// Wraps any `BufRead` and increments an indicatif `ProgressBar` with every
/// byte consumed. Byte tracking happens in `consume` so `read_line` / `read_until`
/// (used by `StatementExtractor`) is covered without double-counting.
pub struct ProgressReader<R: BufRead> {
    inner: R,
    bar: ProgressBar,
}

impl<R: BufRead> ProgressReader<R> {
    pub fn new(inner: R, bar: ProgressBar) -> Self {
        Self { inner, bar }
    }
}

impl<R: BufRead> Read for ProgressReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // Copy from the inner buffer, then consume via our own `consume` so
        // that the progress bar is updated exactly once per byte.
        let n = {
            let available = BufRead::fill_buf(self)?;
            let n = available.len().min(buf.len());
            buf[..n].copy_from_slice(&available[..n]);
            n
        };
        BufRead::consume(self, n);
        Ok(n)
    }
}

impl<R: BufRead> BufRead for ProgressReader<R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.inner.fill_buf()
    }

    fn consume(&mut self, amt: usize) {
        self.inner.consume(amt);
        if amt > 0 {
            self.bar.inc(amt as u64);
        }
    }
}

/// Build a progress bar suited to the input source.
/// - Seekable file (known length): byte-progress bar with ETA.
/// - Stdin (unknown length): spinner that shows elapsed time.
pub fn make_bar(file_len: Option<u64>) -> anyhow::Result<ProgressBar> {
    match file_len {
        Some(len) => {
            let bar = ProgressBar::new(len);
            bar.set_style(
                ProgressStyle::with_template(
                    "[{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})",
                )?
                .progress_chars("=>-"),
            );
            Ok(bar)
        }
        None => {
            let bar = ProgressBar::new_spinner();
            bar.set_style(ProgressStyle::with_template(
                "{spinner:.green} [{elapsed_precise}] {msg}",
            )?);
            Ok(bar)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufReader, Read};

    #[test]
    fn progress_reader_read_copies_bytes_and_advances_bar() {
        let bar = ProgressBar::hidden();
        let mut reader = ProgressReader::new(BufReader::new(&b"abcdef"[..]), bar.clone());
        let mut buf = [0_u8; 3];

        let n = reader.read(&mut buf).unwrap();

        assert_eq!(n, 3);
        assert_eq!(&buf, b"abc");
        assert_eq!(bar.position(), 3);
    }

    #[test]
    fn progress_reader_consume_zero_does_not_advance_bar() {
        let bar = ProgressBar::hidden();
        let mut reader = ProgressReader::new(BufReader::new(&b"abc"[..]), bar.clone());

        BufRead::consume(&mut reader, 0);

        assert_eq!(bar.position(), 0);
    }

    #[test]
    fn make_bar_builds_sized_bar_and_spinner() {
        let sized = make_bar(Some(128)).unwrap();
        assert_eq!(sized.length(), Some(128));

        let spinner = make_bar(None).unwrap();
        assert_eq!(spinner.length(), None);
    }
}
