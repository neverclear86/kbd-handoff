//! A size-bounded log file shared by several receiver processes.
//!
//! Each write opens the file by path in append mode, writes one record and
//! closes it again, so concurrent processes (for example a `toggle` and the
//! `session` it started) never hold a handle that another one has rotated
//! away. Before a write, a file that has grown beyond the limit is renamed to
//! `<name>.old` (replacing an older one), so at most about twice the limit is
//! kept on disk.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Default size at which the log file is rotated.
pub const DEFAULT_MAX_LEN: u64 = 1024 * 1024;

/// A log file path with a rotation limit. Implements
/// [`tracing_subscriber::fmt::MakeWriter`].
#[derive(Debug, Clone)]
pub struct LogFile {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    path: PathBuf,
    old: PathBuf,
    max_len: u64,
}

impl LogFile {
    /// Use `path`, creating its parent directory if needed.
    pub fn new(path: impl Into<PathBuf>, max_len: u64) -> io::Result<Self> {
        let path = path.into();
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let mut old = path.clone().into_os_string();
        old.push(".old");
        Ok(Self {
            inner: Arc::new(Inner {
                path,
                old: old.into(),
                max_len,
            }),
        })
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Append `buf` as one write, rotating first if the file is too large.
    pub fn append(&self, buf: &[u8]) -> io::Result<()> {
        let inner = &*self.inner;
        if let Ok(meta) = std::fs::metadata(&inner.path)
            && meta.len() >= inner.max_len
        {
            // Best effort: another process may have rotated it already.
            let _ = std::fs::rename(&inner.path, &inner.old);
        }
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&inner.path)?;
        f.write_all(buf)
    }
}

/// Writer for one log record; see [`LogFile`].
#[derive(Debug)]
pub struct LogFileWriter<'a>(&'a LogFile);

impl Write for LogFileWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.append(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogFile {
    type Writer = LogFileWriter<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        LogFileWriter(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("kbd-handoff-logfile-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn appends_and_rotates() {
        let dir = temp_dir("rotate");
        let path = dir.join("sub").join("receiver.log");
        let log = LogFile::new(&path, 10).unwrap();
        log.append(b"12345").unwrap();
        log.append(b"6789").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"123456789");
        // 9 bytes < 10: no rotation yet.
        log.append(b"ab").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"123456789ab");
        // 11 bytes >= 10: rotated before this write.
        log.append(b"cd").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"cd");
        let old = dir.join("sub").join("receiver.log.old");
        assert_eq!(std::fs::read(&old).unwrap(), b"123456789ab");
        // A second rotation replaces the old file.
        log.append(b"0123456789").unwrap();
        log.append(b"x").unwrap();
        assert_eq!(std::fs::read(&old).unwrap(), b"cd0123456789");
        assert_eq!(std::fs::read(&path).unwrap(), b"x");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn works_as_tracing_writer() {
        use tracing_subscriber::fmt::MakeWriter;
        let dir = temp_dir("writer");
        let path = dir.join("receiver.log");
        let log = LogFile::new(&path, DEFAULT_MAX_LEN).unwrap();
        write!(log.make_writer(), "hello ").unwrap();
        write!(log.make_writer(), "world").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello world");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
