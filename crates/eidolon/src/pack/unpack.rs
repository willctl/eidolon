//! The folder is built beside the real one (`<into>.eidolon-staging-<rand>`)
//! and renamed into place at the end, so a failure or a kill never leaves a
//! half folder under the real name. Nothing is written outside the staging
//! folder: every path is checked before any file is made, every parent is
//! looked at with `symlink_metadata` (a symlink made earlier in the same
//! unpack is never walked through), and files are created with `O_EXCL` and
//! `O_NOFOLLOW`.

use crate::error::{Error, Result};
use crate::pack::manifest::{FacetManifest, Node};
use crate::vault::{Keys, Vault};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use xenonet_id::codec::{hex, random};

fn components(path: &str) -> Result<Vec<&str>> {
    let bad = path.is_empty() || path.starts_with('/') || path.contains('\0') || (cfg!(windows) && (path.contains('\\') || path.contains(':')));
    let parts: Vec<&str> = path.split('/').collect();
    if bad || parts.iter().any(|p| p.is_empty() || *p == "." || *p == "..") {
        return Err(Error::UnsafePath(path.to_owned()));
    }
    Ok(parts)
}

pub fn unpack_facet(fm: &FacetManifest, keys: &Keys, vault: &Vault, into: &Path) -> Result<()> {
    for entry in &fm.entries {
        components(&entry.path)?;
    }
    match fs::symlink_metadata(into) {
        Ok(meta) => {
            let empty = meta.is_dir() && fs::read_dir(into).map_err(Error::io("read the folder"))?.next().is_none();
            if !empty {
                return Err(Error::NotEmpty(into.display().to_string()));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(Error::Io { what: "look at the folder", source: e }),
    }
    let staging = staging_path(into)?;
    if let Some(parent) = staging.parent() {
        fs::create_dir_all(parent).map_err(Error::io("make the parent folder"))?;
    }
    fs::create_dir(&staging).map_err(Error::io("make the staging folder"))?;
    let built = build(fm, keys, vault, &staging).and_then(|()| put_in_place(&staging, into));
    if built.is_err() {
        // Our own folder, made a moment ago; a failure to remove it is
        // reported by the original error.
        let _ = fs::remove_dir_all(&staging);
    }
    built
}

fn staging_path(into: &Path) -> Result<PathBuf> {
    let tag = hex(&random::<8>().map_err(|_| Error::random())?);
    let mut name = into.file_name().map(std::ffi::OsString::from).ok_or_else(|| Error::NotEmpty(into.display().to_string()))?;
    name.push(format!(".eidolon-staging-{tag}"));
    Ok(into.with_file_name(name))
}

fn put_in_place(staging: &Path, into: &Path) -> Result<()> {
    if into.exists() {
        // Empty (checked above); Windows cannot rename over it.
        fs::remove_dir(into).map_err(Error::io("replace the empty folder"))?;
    }
    fs::rename(staging, into).map_err(Error::io("move the folder into place"))
}

fn build(fm: &FacetManifest, keys: &Keys, vault: &Vault, root: &Path) -> Result<()> {
    let mut dir_modes: Vec<(PathBuf, u32)> = Vec::new();
    for entry in &fm.entries {
        let parts = components(&entry.path)?;
        let (last, parents) = parts.split_last().ok_or_else(|| Error::UnsafePath(entry.path.clone()))?;
        let mut here = root.to_path_buf();
        for part in parents {
            here.push(part);
            match fs::symlink_metadata(&here) {
                Ok(m) if m.is_dir() => {}
                Ok(_) => return Err(Error::UnsafePath(entry.path.clone())),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => fs::create_dir(&here).map_err(Error::io("make a folder"))?,
                Err(e) => return Err(Error::Io { what: "look at a folder", source: e }),
            }
        }
        let target = here.join(last);
        match &entry.node {
            Node::Dir { mode } => {
                match fs::symlink_metadata(&target) {
                    Ok(m) if m.is_dir() => {}
                    Ok(_) => return Err(Error::UnsafePath(entry.path.clone())),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => fs::create_dir(&target).map_err(Error::io("make a folder"))?,
                    Err(e) => return Err(Error::Io { what: "look at a folder", source: e }),
                }
                dir_modes.push((target, *mode));
            }
            Node::File { mode, size, chunks } => {
                let mut file = create_file(&target)?;
                let mut written = 0u64;
                for name in chunks {
                    let plain = keys.open(name, &vault.get(name)?)?;
                    file.write_all(&plain).map_err(Error::io("write a file"))?;
                    written += plain.len() as u64;
                }
                if written != *size {
                    return Err(Error::Damaged { what: "manifest", why: format!("{:?} has {written} bytes, not the {size} the manifest says", entry.path) });
                }
                file.flush().map_err(Error::io("write a file"))?;
                set_mode_file(&file, *mode)?;
            }
            Node::Symlink { target: to } => make_symlink(to, &target)?,
        }
    }
    // Folders last and deepest first, so a read-only folder never blocks
    // its own contents.
    dir_modes.sort_by_key(|(p, _)| std::cmp::Reverse(p.components().count()));
    for (path, mode) in dir_modes {
        set_mode_path(&path, mode)?;
    }
    Ok(())
}

#[cfg(unix)]
fn create_file(path: &Path) -> Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new().write(true).create_new(true).mode(0o600).custom_flags(libc::O_NOFOLLOW).open(path).map_err(Error::io("make a file"))
}

#[cfg(not(unix))]
fn create_file(path: &Path) -> Result<fs::File> {
    fs::OpenOptions::new().write(true).create_new(true).open(path).map_err(Error::io("make a file"))
}

#[cfg(unix)]
fn set_mode_file(file: &fs::File, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(mode & 0o777)).map_err(Error::io("set a file's mode"))
}

#[cfg(not(unix))]
fn set_mode_file(_file: &fs::File, _mode: u32) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_mode_path(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o777)).map_err(Error::io("set a folder's mode"))
}

#[cfg(not(unix))]
fn set_mode_path(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn make_symlink(to: &str, at: &Path) -> Result<()> {
    std::os::unix::fs::symlink(to, at).map_err(Error::io("make a symlink"))
}

#[cfg(not(unix))]
fn make_symlink(_to: &str, _at: &Path) -> Result<()> {
    Err(Error::Io { what: "make a symlink", source: std::io::Error::new(std::io::ErrorKind::Unsupported, "symlinks are not supported on this system") })
}
