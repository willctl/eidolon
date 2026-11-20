//! Replacing a folder whole. The new copy is filled beside it, the old one is
//! renamed aside, the new one renamed in, and only then is the old one
//! removed. A run killed at any step leaves the old folder or the new one
//! (or the old one aside), and `recover` finishes or undoes it.

use crate::error::{io_err, Code, Error, Result};
use crate::files::remove_dir_if_present;
use crate::lock::AppLock;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

fn beside(dest: &Path, suffix: &str) -> Result<PathBuf> {
    let name = dest.file_name().ok_or_else(|| Error::refused(Code::NoPath, format!("{} names no folder.", dest.display()), "Give a folder's path."))?;
    let mut side = OsString::from(".");
    side.push(name);
    side.push(suffix);
    Ok(dest.with_file_name(side))
}

fn fresh(dest: &Path) -> Result<PathBuf> {
    beside(dest, ".eidolon-new")
}

fn old(dest: &Path) -> Result<PathBuf> {
    beside(dest, ".eidolon-old")
}

/// Finishes or undoes a replace that was cut off.
pub(crate) fn recover(dest: &Path) -> Result<()> {
    let (fresh, old) = (fresh(dest)?, old(dest)?);
    remove_dir_if_present(&fresh).map_err(io_err(format!("remove {}", fresh.display())))?;
    if old.exists() {
        if dest.exists() {
            remove_dir_if_present(&old).map_err(io_err(format!("remove {}", old.display())))?;
        } else {
            fs::rename(&old, dest).map_err(io_err(format!("put {} back", dest.display())))?;
        }
    }
    Ok(())
}

/// Fills a fresh folder with `fill`, then puts it at `dest` in place of what
/// was there. The app's lock is held to the end; on Windows, where a folder
/// with an open file cannot be renamed, until just before the swap.
pub(crate) fn replace<T>(dest: &Path, lock: Option<AppLock>, fill: impl FnOnce(&Path) -> Result<T>) -> Result<T> {
    recover(dest)?;
    let (fresh, old) = (fresh(dest)?, old(dest)?);
    if let Some(parent) = dest.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(io_err(format!("make {}", parent.display())))?;
    }
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&fresh).map_err(io_err(format!("make {}", fresh.display())))?;
    let filled = match fill(&fresh) {
        Ok(v) => v,
        Err(e) => {
            let _ = remove_dir_if_present(&fresh);
            return Err(e);
        }
    };
    #[cfg(windows)]
    drop(lock);
    let had_old = dest.exists();
    if had_old {
        fs::rename(dest, &old).map_err(io_err(format!("move {} aside", dest.display())))?;
    }
    fs::rename(&fresh, dest).map_err(io_err(format!("move the new copy to {}", dest.display())))?;
    if had_old {
        remove_dir_if_present(&old).map_err(io_err(format!("remove {}", old.display())))?;
    }
    #[cfg(not(windows))]
    drop(lock);
    Ok(filled)
}

/// Removes `dest` the same way: renamed aside first, so a cut run leaves it whole or gone.
pub(crate) fn remove(dest: &Path, lock: Option<AppLock>) -> Result<bool> {
    recover(dest)?;
    if !dest.exists() {
        return Ok(false);
    }
    let old = old(dest)?;
    #[cfg(windows)]
    drop(lock);
    fs::rename(dest, &old).map_err(io_err(format!("move {} aside", dest.display())))?;
    remove_dir_if_present(&old).map_err(io_err(format!("remove {}", old.display())))?;
    #[cfg(not(windows))]
    drop(lock);
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cut_replace_is_undone_or_finished() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("profile");
        fs::create_dir(&dest).unwrap();
        fs::write(dest.join("a"), "old").unwrap();

        fs::rename(&dest, old(&dest).unwrap()).unwrap();
        fs::create_dir(fresh(&dest).unwrap()).unwrap();
        recover(&dest).unwrap();
        assert_eq!(fs::read_to_string(dest.join("a")).unwrap(), "old", "cut between the renames: the old copy comes back");
        assert!(!fresh(&dest).unwrap().exists());

        fs::create_dir(old(&dest).unwrap()).unwrap();
        recover(&dest).unwrap();
        assert!(!old(&dest).unwrap().exists(), "cut after the swap: the old copy goes");

        let failed: Result<()> = replace(&dest, None, |_| Err(Error::refused(Code::TooLarge, "no", "no")));
        assert!(failed.is_err());
        assert_eq!(fs::read_to_string(dest.join("a")).unwrap(), "old", "a failed fill changes nothing");

        replace(&dest, None, |fresh| fs::write(fresh.join("b"), "new").map_err(io_err("write"))).unwrap();
        assert!(!dest.join("a").exists() && dest.join("b").exists());
        assert!(remove(&dest, None).unwrap() && !dest.exists());
    }
}
