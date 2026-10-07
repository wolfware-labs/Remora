//! `<data_dir>/remorad.log`: owner-only, rotated to `remorad.log.1` when it grows too big.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use crate::clock::{Clock, SystemClock};

/// The log is rotated when a line would take it past this size.
pub const LOG_ROTATE_BYTES: u64 = 10 * 1024 * 1024;

/// An append-only log file with one previous generation.
#[derive(Debug)]
pub struct LogFile {
    path: PathBuf,
    max_bytes: u64,
    file: Mutex<File>,
}

impl LogFile {
    /// Open `<dir>/remorad.log`, creating `dir` owner-only if needed.
    pub fn open(dir: &Path, max_bytes: u64) -> io::Result<Self> {
        Self::open_named(dir, "remorad.log", max_bytes)
    }

    /// Open `<dir>/<name>` (rotated to `<name>.1`), creating `dir` owner-only if needed.
    pub fn open_named(dir: &Path, name: &str, max_bytes: u64) -> io::Result<Self> {
        private_dir(dir)?;
        let path = dir.join(name);
        let file = open_append(&path)?;
        Ok(Self {
            path,
            max_bytes,
            file: Mutex::new(file),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append `line` and a newline, rotating first if it would not fit.
    pub fn write_line(&self, line: &str) -> io::Result<()> {
        let mut file = self.file.lock().unwrap_or_else(PoisonError::into_inner);
        let len = file.metadata()?.len();
        if len > 0 && len + line.len() as u64 + 1 > self.max_bytes {
            fs::rename(&self.path, self.path.with_extension("log.1"))?;
            *file = open_append(&self.path)?;
        }
        writeln!(file, "{line}")
    }

    /// Append `message` with the current UTC time and `level`, as the daemon's log does.
    pub fn write_entry(&self, level: &str, message: &str) -> io::Result<()> {
        self.write_line(&format!("{} {level:<5} {message}", utc(SystemClock.now_ms())))
    }
}

fn open_append(path: &Path) -> io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

/// Sends the `log` facade (the daemon's and the engine's messages) to a [`LogFile`].
struct FileLogger {
    file: LogFile,
}

impl log::Log for FileLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            let _ = self
                .file
                .write_entry(record.level().as_str(), &record.args().to_string());
        }
    }

    fn flush(&self) {}
}

/// Route this process's log messages to `<data_dir>/remorad.log`. Call once, at start-up.
pub fn install_logger(data_dir: &Path) -> io::Result<()> {
    let file = LogFile::open(data_dir, LOG_ROTATE_BYTES)?;
    log::set_boxed_logger(Box::new(FileLogger { file })).map_err(io::Error::other)?;
    log::set_max_level(log::LevelFilter::Info);
    Ok(())
}

/// `ms` since the Unix epoch as `YYYY-MM-DDTHH:MM:SS.mmmZ`.
fn utc(ms: i64) -> String {
    let (days, ms_of_day) = (ms.div_euclid(86_400_000), ms.rem_euclid(86_400_000));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let (h, m, s, milli) = (
        ms_of_day / 3_600_000,
        ms_of_day / 60_000 % 60,
        ms_of_day / 1_000 % 60,
        ms_of_day % 1_000,
    );
    format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}.{milli:03}Z")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn timestamps_are_utc() {
        assert_eq!(utc(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(utc(1_700_000_000_123), "2023-11-14T22:13:20.123Z");
        assert_eq!(utc(951_782_400_000), "2000-02-29T00:00:00.000Z");
    }

    #[test]
    fn the_log_rotates_and_keeps_one_previous_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let log = LogFile::open(dir.path(), 20).unwrap();

        for line in ["first line", "second line", "third line"] {
            log.write_line(line).unwrap();
        }

        let read = |name: &str| fs::read_to_string(dir.path().join(name)).unwrap();
        assert_eq!(read("remorad.log"), "third line\n");
        assert_eq!(
            read("remorad.log.1"),
            "second line\n",
            "only one generation is kept"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_log_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let log = LogFile::open(&dir.path().join("data"), LOG_ROTATE_BYTES).unwrap();
        log.write_line("hello").unwrap();

        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(log.path()), 0o600);
        assert_eq!(mode(&dir.path().join("data")), 0o700);
    }
}
