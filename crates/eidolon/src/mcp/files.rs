use crate::error::{Error, Result};
use crate::model::RefusalCode;
use std::path::{Component, Path, PathBuf};

pub const DOWNLOADS_DIR: &str = "downloads";

pub const MAX_UPLOAD_BYTES: u64 = 512 * 1024 * 1024;

fn root(files: &Path) -> Result<PathBuf> {
    files.canonicalize().map_err(Error::io("use the files folder"))
}

pub fn upload_source(files: &Path, requested: &str) -> Result<PathBuf> {
    let root = root(files)?;
    let asked = Path::new(requested);
    let resolved = if asked.is_absolute() { asked.to_path_buf() } else { root.join(asked) }.canonicalize().map_err(Error::io("read the file to upload"))?;
    if !resolved.starts_with(&root) {
        return Err(Error::refused(
            RefusalCode::OutsideFiles,
            format!("The file {requested} is outside the files folder {}.", root.display()),
            "Put it in that folder first; only files there can be uploaded.",
        ));
    }
    let meta = resolved.metadata().map_err(Error::io("read the file to upload"))?;
    if !meta.is_file() {
        return Err(Error::refused(RefusalCode::NotAFile, format!("{requested} is not a regular file."), "Upload one file at a time."));
    }
    if meta.len() > MAX_UPLOAD_BYTES {
        return Err(Error::refused(RefusalCode::NotAFile, format!("{requested} is larger than 512 MiB."), "Upload a smaller file."));
    }
    Ok(resolved)
}

pub fn upload_name(resolved: &Path) -> Result<String> {
    let name =
        resolved.file_name().and_then(|n| n.to_str()).ok_or_else(|| Error::refused(RefusalCode::BadFileName, "The file's name is not plain text.", "Rename it and try again."))?;
    check_name(name)?;
    Ok(name.to_owned())
}

pub fn check_name(name: &str) -> Result<()> {
    let bad = name.is_empty() || name.contains(['/', '\\', '\0']) || Path::new(name).components().any(|c| !matches!(c, Component::Normal(_)));
    if bad {
        return Err(Error::refused(RefusalCode::BadFileName, format!("{name:?} is not a plain file name."), "Give a name from list_downloads, without a folder or `..`."));
    }
    Ok(())
}

pub fn downloads_dir(files: &Path) -> Result<PathBuf> {
    let root = root(files)?;
    let dir = root.join(DOWNLOADS_DIR);
    std::fs::create_dir_all(&dir).map_err(Error::io("make the downloads folder"))?;
    let resolved = dir.canonicalize().map_err(Error::io("use the downloads folder"))?;
    if !resolved.starts_with(&root) {
        return Err(Error::refused(RefusalCode::OutsideFiles, format!("{} leads outside the files folder.", dir.display()), "Remove it and try again."));
    }
    Ok(resolved)
}

/// A path in `dir` for `name` that does not exist yet: `name`, then
/// `stem-1.ext`, `stem-2.ext`, so a fetch never overwrites an earlier file.
pub fn free_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if first.symlink_metadata().is_err() {
        return first;
    }
    let (stem, ext) = match name.rfind('.').filter(|&i| i > 0) {
        Some(i) => (&name[..i], &name[i..]),
        None => (name, ""),
    };
    (1..=u32::MAX).map(|n| dir.join(format!("{stem}-{n}{ext}"))).find(|p| p.symlink_metadata().is_err()).unwrap_or(first)
}
