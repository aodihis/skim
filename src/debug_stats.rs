#[cfg(debug_assertions)]
pub const DEBUG_ENV: &str = "SKIM_DEBUG";

#[cfg(debug_assertions)]
mod imp {
    use std::{
        env,
        time::{Duration, Instant},
    };

    use super::DEBUG_ENV;

    pub struct DebugStats {
        enabled: bool,
        started_at: Instant,
        file_len: Option<u64>,
        statements: u64,
        create_statements: u64,
        insert_statements: u64,
        skipped_statements: u64,
        rows: u64,
        statement_bytes: u64,
        max_statement_bytes: usize,
        schema_parse: Duration,
        table_parse: Duration,
        row_parse: Duration,
        row_write: Duration,
        final_write: Duration,
    }

    pub struct DebugTimer {
        started_at: Instant,
    }

    impl DebugStats {
        pub fn new(file_len: Option<u64>) -> Self {
            Self {
                enabled: debug_enabled(),
                started_at: Instant::now(),
                file_len,
                statements: 0,
                create_statements: 0,
                insert_statements: 0,
                skipped_statements: 0,
                rows: 0,
                statement_bytes: 0,
                max_statement_bytes: 0,
                schema_parse: Duration::ZERO,
                table_parse: Duration::ZERO,
                row_parse: Duration::ZERO,
                row_write: Duration::ZERO,
                final_write: Duration::ZERO,
            }
        }

        pub fn timer(&self) -> DebugTimer {
            DebugTimer {
                started_at: Instant::now(),
            }
        }

        pub fn record_statement(&mut self, bytes: usize) {
            if !self.enabled {
                return;
            }
            self.statements += 1;
            self.statement_bytes += bytes as u64;
            self.max_statement_bytes = self.max_statement_bytes.max(bytes);
        }

        pub fn record_create_statement(&mut self) {
            if self.enabled {
                self.create_statements += 1;
            }
        }

        pub fn record_insert_statement(&mut self) {
            if self.enabled {
                self.insert_statements += 1;
            }
        }

        pub fn record_skipped_statement(&mut self) {
            if self.enabled {
                self.skipped_statements += 1;
            }
        }

        pub fn record_rows(&mut self, count: usize) {
            if self.enabled {
                self.rows += count as u64;
            }
        }

        pub fn add_schema_parse(&mut self, elapsed: Duration) {
            if self.enabled {
                self.schema_parse += elapsed;
            }
        }

        pub fn add_table_parse(&mut self, elapsed: Duration) {
            if self.enabled {
                self.table_parse += elapsed;
            }
        }

        pub fn add_row_parse(&mut self, elapsed: Duration) {
            if self.enabled {
                self.row_parse += elapsed;
            }
        }

        pub fn add_row_write(&mut self, elapsed: Duration) {
            if self.enabled {
                self.row_write += elapsed;
            }
        }

        pub fn add_final_write(&mut self, elapsed: Duration) {
            if self.enabled {
                self.final_write += elapsed;
            }
        }

        pub fn print_summary(&self) {
            if !self.enabled {
                return;
            }

            let total = self.started_at.elapsed();
            let measured = self.schema_parse
                + self.table_parse
                + self.row_parse
                + self.row_write
                + self.final_write;
            let other = total.saturating_sub(measured);

            eprintln!("[skim debug] enabled by {DEBUG_ENV}=1 (debug build only)");
            eprintln!(
                "[skim debug] total={} rows={} rows/s={:.2}",
                fmt_duration(total),
                self.rows,
                rate(self.rows, total),
            );
            if let Some(file_len) = self.file_len {
                eprintln!(
                    "[skim debug] input={} MiB throughput={:.2} MiB/s",
                    fmt_mib(file_len),
                    mib_rate(file_len, total),
                );
            }
            eprintln!(
                "[skim debug] statements total={} create={} insert={} skipped={} bytes={} MiB max_statement={} MiB",
                self.statements,
                self.create_statements,
                self.insert_statements,
                self.skipped_statements,
                fmt_mib(self.statement_bytes),
                fmt_mib(self.max_statement_bytes as u64),
            );
            eprintln!(
                "[skim debug] timings schema_parse={} table_parse={} row_parse={} row_write={} final_write={} other={}",
                fmt_duration(self.schema_parse),
                fmt_duration(self.table_parse),
                fmt_duration(self.row_parse),
                fmt_duration(self.row_write),
                fmt_duration(self.final_write),
                fmt_duration(other),
            );
        }
    }

    impl DebugTimer {
        pub fn elapsed(&self) -> Duration {
            self.started_at.elapsed()
        }
    }

    fn debug_enabled() -> bool {
        env::var(DEBUG_ENV)
            .map(|value| {
                let value = value.trim();
                !value.is_empty()
                    && !value.eq_ignore_ascii_case("0")
                    && !value.eq_ignore_ascii_case("false")
                    && !value.eq_ignore_ascii_case("no")
                    && !value.eq_ignore_ascii_case("off")
            })
            .unwrap_or(false)
    }

    fn rate(count: u64, elapsed: Duration) -> f64 {
        let secs = elapsed.as_secs_f64();
        if secs > 0.0 {
            count as f64 / secs
        } else {
            0.0
        }
    }

    fn mib_rate(bytes: u64, elapsed: Duration) -> f64 {
        let secs = elapsed.as_secs_f64();
        if secs > 0.0 {
            bytes as f64 / 1024.0 / 1024.0 / secs
        } else {
            0.0
        }
    }

    fn fmt_duration(duration: Duration) -> String {
        format!("{:.3}s", duration.as_secs_f64())
    }

    fn fmt_mib(bytes: u64) -> String {
        format!("{:.2}", bytes as f64 / 1024.0 / 1024.0)
    }
}

#[cfg(not(debug_assertions))]
mod imp {
    use std::time::Duration;

    pub struct DebugStats;

    pub struct DebugTimer;

    impl DebugStats {
        pub fn new(_file_len: Option<u64>) -> Self {
            Self
        }

        pub fn timer(&self) -> DebugTimer {
            DebugTimer
        }

        pub fn record_statement(&mut self, _bytes: usize) {}
        pub fn record_create_statement(&mut self) {}
        pub fn record_insert_statement(&mut self) {}
        pub fn record_skipped_statement(&mut self) {}
        pub fn record_rows(&mut self, _count: usize) {}
        pub fn add_schema_parse(&mut self, _elapsed: Duration) {}
        pub fn add_table_parse(&mut self, _elapsed: Duration) {}
        pub fn add_row_parse(&mut self, _elapsed: Duration) {}
        pub fn add_row_write(&mut self, _elapsed: Duration) {}
        pub fn add_final_write(&mut self, _elapsed: Duration) {}
        pub fn print_summary(&self) {}
    }

    impl DebugTimer {
        pub fn elapsed(&self) -> Duration {
            Duration::ZERO
        }
    }
}

pub use imp::DebugStats;
