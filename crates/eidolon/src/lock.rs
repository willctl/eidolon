//! The app's own lock, taken the way the app takes it, and held while
//! Eidolon reads or replaces the folder. Firefox holds an fcntl write lock
//! on `.parentlock` on Unix, and keeps `parent.lock` open without sharing on
//! Windows; while Eidolon holds either, Firefox does not start on the profile.

use crate::error::{io_err, Code, Error, Result};
use crate::target::Closed;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

/// Released when dropped.
#[derive(Debug)]
pub struct AppLock {
    _file: File,
}

/// Takes the app's lock in `dir`, or refuses with `app_running`. A folder
/// that does not exist yet needs no lock.
pub fn hold(dir: &Path, closed: &Closed) -> Result<Option<AppLock>> {
    let Some(name) = closed.lock_here() else { return Ok(None) };
    if !dir.is_dir() {
        return Ok(None);
    }
    let path = dir.join(name);
    match take(&path) {
        Ok(file) => Ok(Some(AppLock { _file: file })),
        Err(e) if held_by_another(&e) => {
            Err(Error::refused(Code::AppRunning, format!("{} is running on {}.", closed.app, dir.display()), format!("Quit {} and run the command again.", closed.app)))
        }
        Err(e) => Err(io_err(format!("take the lock {}", path.display()))(e)),
    }
}

#[cfg(unix)]
fn take(path: &Path) -> io::Result<File> {
    use rustix::fs::{fcntl_lock, FlockOperation, OFlags};
    use std::os::unix::fs::OpenOptionsExt;
    let file = OpenOptions::new().read(true).write(true).create(true).truncate(false).mode(0o644).custom_flags(OFlags::NOFOLLOW.bits().cast_signed()).open(path)?;
    fcntl_lock(&file, FlockOperation::NonBlockingLockExclusive)?;
    Ok(file)
}

#[cfg(unix)]
fn held_by_another(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::WouldBlock || e.raw_os_error() == Some(rustix::io::Errno::ACCESS.raw_os_error())
}

#[cfg(windows)]
fn take(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    OpenOptions::new().read(true).write(true).create(true).truncate(false).share_mode(0).open(path)
}

#[cfg(windows)]
fn held_by_another(e: &io::Error) -> bool {
    const SHARING_VIOLATION: i32 = 32;
    const LOCK_VIOLATION: i32 = 33;
    matches!(e.raw_os_error(), Some(SHARING_VIOLATION | LOCK_VIOLATION))
}
