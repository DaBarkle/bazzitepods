//! The app's log also goes to a file, because an autostarted Kase has no terminal and its
//! output reaches no journal: without the file, "why didn't my AirPods switch?" has no
//! answer. `~/.local/state/kase/kase.log` (inside the Flatpak:
//! `~/.var/app/io.github.dabarkle.Kase/.local/state/kase/kase.log`), with the previous
//! run's log kept beside it as `kase.log.1` once it grows past a few megabytes.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// A log over this size is moved aside when the app starts.
const ROTATE_AT: u64 = 5 * 1024 * 1024;

pub fn path() -> PathBuf {
    crate::utils::state_home().join("kase").join("kase.log")
}

/// Opens the log for appending, moving a large one aside first. `None` when the folder
/// can't be written; the app then logs to stderr only.
pub fn open(path: &Path) -> Option<File> {
    std::fs::create_dir_all(path.parent()?).ok()?;
    if std::fs::metadata(path).is_ok_and(|m| m.len() > ROTATE_AT) {
        let _ = std::fs::rename(path, path.with_extension("log.1"));
    }
    OpenOptions::new().create(true).append(true).open(path).ok()
}

/// Writes everything to stderr and, when it opened, the log file.
pub struct Tee(pub Option<File>);

impl Write for Tee {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Some(file) = &mut self.0 {
            // A full disk must not stop the app: lose the file copy, keep stderr.
            let _ = file.write_all(buf);
        }
        io::stderr().write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(file) = &mut self.0 {
            let _ = file.flush();
        }
        io::stderr().flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_and_moves_a_large_log_aside() {
        let dir = std::env::temp_dir().join(format!("kase-log-test-{}", std::process::id()));
        let path = dir.join("kase.log");
        let mut tee = Tee(open(&path));
        tee.write_all(b"first\n").unwrap();
        tee.flush().unwrap();
        drop(tee);
        let mut tee = Tee(open(&path));
        tee.write_all(b"second\n").unwrap();
        drop(tee);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first\nsecond\n");

        std::fs::write(&path, vec![b'x'; ROTATE_AT as usize + 1]).unwrap();
        let mut tee = Tee(open(&path));
        tee.write_all(b"new run\n").unwrap();
        drop(tee);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new run\n");
        assert!(path.with_extension("log.1").exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
