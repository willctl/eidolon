//! The tar inside a bundle. Capture writes it from a folder or from another
//! tar, filtered by a target's rules. Restore reads it entry by entry and
//! refuses anything that could write outside the folder it fills.

use crate::error::{io_err, Code, Error, Result};
use crate::target::Rules;
use serde::Serialize;
use std::cell::Cell;
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tar::{EntryType, Header};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub files: u64,
    pub dirs: u64,
    pub links: u64,
    pub bytes: u64,
    /// Sockets, devices, FIFOs and hard links in a capture's source, which a bundle never holds.
    pub skipped: u64,
}

/// How much a restore unpacks before it refuses with `too_large`.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_bytes: u64,
    pub max_entries: u64,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits { max_bytes: 16 << 30, max_entries: 1_000_000 }
    }
}

fn unsafe_entry(rel: &str, why: &str) -> Error {
    Error::refused(Code::UnsafeEntry, format!("The archive's entry {rel:?} {why}."), "Do not open it; seal the target again on a machine you trust.")
}

fn too_large(limits: &Limits) -> Error {
    Error::refused(Code::TooLarge, format!("The archive holds more than {} bytes or {} entries.", limits.max_bytes, limits.max_entries), "Raise --max-size if the target is really that big.")
}

/// Writes clean entries: relative paths, permission bits only, no owners.
pub(crate) struct TarOut<W: Write> {
    builder: tar::Builder<W>,
    pub counts: Counts,
}

impl<W: Write> TarOut<W> {
    pub fn new(out: W) -> TarOut<W> {
        TarOut { builder: tar::Builder::new(out), counts: Counts::default() }
    }

    fn header(kind: EntryType, mode: u32, mtime: u64, size: u64) -> Header {
        let mut h = Header::new_gnu();
        h.set_entry_type(kind);
        h.set_mode(mode & 0o777);
        h.set_mtime(mtime);
        h.set_size(size);
        h.set_uid(0);
        h.set_gid(0);
        h
    }

    pub fn dir(&mut self, rel: &str, mode: u32, mtime: u64) -> io::Result<()> {
        let mut h = Self::header(EntryType::Directory, mode, mtime, 0);
        self.builder.append_data(&mut h, rel, io::empty())?;
        self.counts.dirs += 1;
        Ok(())
    }

    pub fn file(&mut self, rel: &str, mode: u32, mtime: u64, size: u64, data: &mut dyn Read) -> io::Result<()> {
        let mut h = Self::header(EntryType::Regular, mode, mtime, size);
        self.builder.append_data(&mut h, rel, Exact { inner: data, left: size })?;
        self.counts.files += 1;
        self.counts.bytes += size;
        Ok(())
    }

    pub fn symlink(&mut self, rel: &str, target: &str, mtime: u64) -> io::Result<()> {
        let mut h = Self::header(EntryType::Symlink, 0o777, mtime, 0);
        self.builder.append_link(&mut h, rel, target)?;
        self.counts.links += 1;
        Ok(())
    }

    pub fn finish(self) -> io::Result<(W, Counts)> {
        let counts = self.counts;
        Ok((self.builder.into_inner()?, counts))
    }
}

/// Reads exactly `left` bytes, or fails: a file that shrinks while it is
/// read would otherwise leave a broken archive.
struct Exact<'a> {
    inner: &'a mut dyn Read,
    left: u64,
}

impl Read for Exact<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.left == 0 {
            return Ok(0);
        }
        let want = buf.len().min(usize::try_from(self.left).unwrap_or(usize::MAX));
        let n = self.inner.read(&mut buf[..want])?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "a file changed size while it was read"));
        }
        self.left -= n as u64;
        Ok(n)
    }
}

fn secs(t: io::Result<SystemTime>) -> u64 {
    t.ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs())
}

#[cfg(unix)]
fn mode_of(meta: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode()
}

#[cfg(not(unix))]
fn mode_of(meta: &fs::Metadata) -> u32 {
    match (meta.is_dir(), meta.permissions().readonly()) {
        (true, _) => 0o700,
        (false, true) => 0o400,
        (false, false) => 0o600,
    }
}

/// Captures the folder `root` by `rules`, in name order, without following links.
pub(crate) fn capture_folder<W: Write>(root: &Path, rules: &Rules, out: &mut TarOut<W>) -> Result<()> {
    if !root.is_dir() {
        return Err(Error::refused(Code::NoPath, format!("{} is not a folder.", root.display()), "Give the target's folder with `eidolon target add <name> --path` or --from."));
    }
    walk(root, "", rules, out)
}

fn walk<W: Write>(dir: &Path, prefix: &str, rules: &Rules, out: &mut TarOut<W>) -> Result<()> {
    let mut names: Vec<_> = fs::read_dir(dir).map_err(io_err(format!("list {}", dir.display())))?.map(|e| e.map(|e| e.file_name())).collect::<io::Result<_>>().map_err(io_err(format!("list {}", dir.display())))?;
    names.sort();
    for name in names {
        let path = dir.join(&name);
        let name = name.to_str().ok_or_else(|| unsafe_entry(&path.display().to_string(), "has a name that is not UTF-8"))?;
        let rel = if prefix.is_empty() { name.to_owned() } else { format!("{prefix}/{name}") };
        if rules.excludes(&rel) {
            continue;
        }
        let meta = fs::symlink_metadata(&path).map_err(io_err(format!("read {}", path.display())))?;
        let kind = meta.file_type();
        let writing = || io_err(format!("archive {rel}"));
        if kind.is_dir() {
            if rules.keeps(&rel) {
                out.dir(&rel, mode_of(&meta), secs(meta.modified())).map_err(writing())?;
            }
            walk(&path, &rel, rules, out)?;
        } else if !rules.keeps(&rel) {
            continue;
        } else if kind.is_file() {
            let mut file = fs::File::open(&path).map_err(io_err(format!("open {}", path.display())))?;
            out.file(&rel, mode_of(&meta), secs(meta.modified()), meta.len(), &mut file).map_err(writing())?;
        } else if kind.is_symlink() {
            let target = fs::read_link(&path).map_err(io_err(format!("read the link {}", path.display())))?;
            let target = target.to_str().ok_or_else(|| unsafe_entry(&rel, "links to a name that is not UTF-8"))?;
            out.symlink(&rel, target, secs(meta.modified())).map_err(writing())?;
        } else {
            out.counts.skipped += 1;
        }
    }
    Ok(())
}

/// Captures from another tar, such as one a cell streams out, by `rules`.
/// Only files, folders and links cross; their paths must stay relative.
pub(crate) fn capture_tar<W: Write>(input: impl Read, rules: &Rules, out: &mut TarOut<W>) -> Result<()> {
    let mut archive = tar::Archive::new(input);
    let reading = || io_err("read the tar given to capture");
    for entry in archive.entries().map_err(reading())? {
        let mut entry = entry.map_err(reading())?;
        let Some(rel) = relative(&entry.path_bytes())? else { continue };
        if !rules.keeps(&rel) {
            continue;
        }
        let h = entry.header();
        let (mode, mtime) = (h.mode().unwrap_or(0o600), h.mtime().unwrap_or(0));
        let writing = || io_err(format!("archive {rel}"));
        match h.entry_type() {
            EntryType::Regular | EntryType::Continuous => {
                let size = entry.size();
                out.file(&rel, mode, mtime, size, &mut entry).map_err(writing())?;
            }
            EntryType::Directory => out.dir(&rel, mode, mtime).map_err(writing())?,
            EntryType::Symlink => {
                let target = entry.link_name_bytes().ok_or_else(|| unsafe_entry(&rel, "is a link without a target"))?;
                let target = std::str::from_utf8(&target).map_err(|_| unsafe_entry(&rel, "links to a name that is not UTF-8"))?.to_owned();
                out.symlink(&rel, &target, mtime).map_err(writing())?;
            }
            _ => out.counts.skipped += 1,
        }
    }
    Ok(())
}

/// An entry's path as `a/b/c`, or none for the archive's root itself.
/// Absolute paths, `..`, backslashes and names that are not UTF-8 are refused.
fn relative(raw: &[u8]) -> Result<Option<String>> {
    let shown = String::from_utf8_lossy(raw);
    let text = std::str::from_utf8(raw).map_err(|_| unsafe_entry(&shown, "has a name that is not UTF-8"))?;
    if text.starts_with('/') {
        return Err(unsafe_entry(text, "is an absolute path"));
    }
    let mut parts = Vec::new();
    for part in text.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err(unsafe_entry(text, "climbs out with `..`")),
            p if p.contains(['\\', '\0']) || (cfg!(windows) && p.contains(':')) => return Err(unsafe_entry(text, "has a character this system reads as a path")),
            p => parts.push(p),
        }
    }
    Ok((!parts.is_empty()).then(|| parts.join("/")))
}

/// Whether a link at `rel` to `target` resolves inside the folder, read
/// without touching the disk.
fn link_stays_inside(rel: &str, target: &str) -> bool {
    if target.is_empty() || target.starts_with(['/', '\\']) || target.contains(['\\', '\0', ':']) {
        return false;
    }
    let mut depth = rel.matches('/').count();
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => match depth.checked_sub(1) {
                Some(d) => depth = d,
                None => return false,
            },
            _ => depth += 1,
        }
    }
    true
}

/// Where restored entries go once they have passed the checks.
pub(crate) trait Sink {
    fn dir(&mut self, rel: &str, mode: u32, mtime: u64) -> Result<()>;
    fn file(&mut self, rel: &str, mode: u32, mtime: u64, size: u64, data: &mut dyn Read) -> Result<()>;
    fn symlink(&mut self, rel: &str, target: &str, mtime: u64) -> Result<()>;
}

/// Reads a tar and hands each entry to `sink` once it is known to be safe.
/// Refused: escaping or absolute paths, links that point out, paths through
/// a link, hard links, devices, FIFOs, setuid, setgid and sticky bits, the
/// same path twice (ignoring case), and more than `limits` allow.
pub(crate) fn restore(input: impl Read, limits: Limits, sink: &mut dyn Sink) -> Result<Counts> {
    let hit = Rc::new(Cell::new(false));
    let capped = Capped { inner: input, left: limits.max_bytes.saturating_add(limits.max_entries.saturating_mul(1536)).saturating_add(1 << 20), hit: hit.clone() };
    let reading = |e: io::Error| if hit.get() { too_large(&limits) } else { io_err("read the archive")(e) };
    let mut archive = tar::Archive::new(capped);
    let (mut seen, mut links) = (HashSet::new(), HashSet::new());
    let mut counts = Counts::default();
    let mut entries = 0u64;
    for entry in archive.entries().map_err(reading)? {
        let mut entry = entry.map_err(reading)?;
        entries += 1;
        if entries > limits.max_entries {
            return Err(too_large(&limits));
        }
        let kind = entry.header().entry_type();
        let Some(rel) = relative(&entry.path_bytes())? else {
            if kind == EntryType::Directory {
                continue;
            }
            return Err(unsafe_entry(".", "is not a folder"));
        };
        let folded = rel.to_lowercase();
        if !seen.insert(folded.clone()) {
            return Err(unsafe_entry(&rel, "appears twice"));
        }
        if folded.match_indices('/').any(|(i, _)| links.contains(&folded[..i])) {
            return Err(unsafe_entry(&rel, "lies under a link"));
        }
        let h = entry.header();
        let mode = h.mode().map_err(|_| unsafe_entry(&rel, "has a mode that does not read"))?;
        let mtime = h.mtime().unwrap_or(0);
        if mode & 0o7000 != 0 {
            return Err(unsafe_entry(&rel, "has a setuid, setgid or sticky bit"));
        }
        match kind {
            EntryType::Regular | EntryType::Continuous => {
                let size = entry.size();
                if counts.bytes.saturating_add(size) > limits.max_bytes {
                    return Err(too_large(&limits));
                }
                sink.file(&rel, mode, mtime, size, &mut entry).map_err(|e| if hit.get() { too_large(&limits) } else { e })?;
                counts.files += 1;
                counts.bytes += size;
            }
            EntryType::Directory => {
                sink.dir(&rel, mode, mtime)?;
                counts.dirs += 1;
            }
            EntryType::Symlink => {
                let target = entry.link_name_bytes().ok_or_else(|| unsafe_entry(&rel, "is a link without a target"))?;
                let target = std::str::from_utf8(&target).map_err(|_| unsafe_entry(&rel, "links to a name that is not UTF-8"))?.to_owned();
                if !link_stays_inside(&rel, &target) {
                    return Err(unsafe_entry(&rel, "links outside the folder"));
                }
                sink.symlink(&rel, &target, mtime)?;
                links.insert(folded);
                counts.links += 1;
            }
            EntryType::Link => return Err(unsafe_entry(&rel, "is a hard link")),
            EntryType::Char | EntryType::Block => return Err(unsafe_entry(&rel, "is a device")),
            EntryType::Fifo => return Err(unsafe_entry(&rel, "is a FIFO")),
            _ => return Err(unsafe_entry(&rel, "is a kind of entry a bundle never holds")),
        }
    }
    Ok(counts)
}

/// Fails once more than `left` bytes have been read, and says so in `hit`.
struct Capped<R> {
    inner: R,
    left: u64,
    hit: Rc<Cell<bool>>,
}

impl<R: Read> Read for Capped<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.left == 0 {
            self.hit.set(true);
            return Err(io::Error::other("the archive is larger than the limit"));
        }
        let want = buf.len().min(usize::try_from(self.left).unwrap_or(usize::MAX));
        let n = self.inner.read(&mut buf[..want])?;
        self.left -= n as u64;
        Ok(n)
    }
}

/// Fills a fresh folder that nothing else writes to.
pub(crate) struct FolderSink {
    root: PathBuf,
    dir_modes: Vec<(PathBuf, u32)>,
}

impl FolderSink {
    pub fn new(root: &Path) -> FolderSink {
        FolderSink { root: root.to_owned(), dir_modes: Vec::new() }
    }

    /// Makes the folders above `rel`, checking that each one already there is a real folder.
    fn parent_of(&self, rel: &str) -> Result<PathBuf> {
        let mut path = self.root.clone();
        let parts: Vec<&str> = rel.split('/').collect();
        for part in &parts[..parts.len() - 1] {
            path.push(part);
            real_dir(&path, rel)?;
        }
        path.push(parts[parts.len() - 1]);
        Ok(path)
    }

    /// Sets each folder's own mode last, deepest first, so a read-only
    /// folder is still filled. The owner always keeps full access.
    pub fn finish(self) -> Result<()> {
        for (path, mode) in self.dir_modes.iter().rev() {
            set_mode(path, mode | 0o700)?;
        }
        Ok(())
    }
}

fn real_dir(path: &Path, rel: &str) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => Ok(()),
        Ok(_) => Err(unsafe_entry(rel, "goes through something that is not a folder")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => private_mkdir(path),
        Err(e) => Err(io_err(format!("read {}", path.display()))(e)),
    }
}

fn private_mkdir(path: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path).map_err(io_err(format!("make the folder {}", path.display())))
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o777)).map_err(io_err(format!("set the mode of {}", path.display())))
}

#[cfg(not(unix))]
fn set_mode(_: &Path, _: u32) -> Result<()> {
    Ok(())
}

impl Sink for FolderSink {
    fn dir(&mut self, rel: &str, mode: u32, _: u64) -> Result<()> {
        let path = self.parent_of(rel)?;
        real_dir(&path, rel)?;
        self.dir_modes.push((path, mode));
        Ok(())
    }

    fn file(&mut self, rel: &str, mode: u32, mtime: u64, size: u64, data: &mut dyn Read) -> Result<()> {
        let path = self.parent_of(rel)?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path).map_err(io_err(format!("create {}", path.display())))?;
        let copied = io::copy(&mut data.take(size), &mut file).map_err(io_err(format!("write {}", path.display())))?;
        if copied != size {
            return Err(unsafe_entry(rel, "ends before its size"));
        }
        file.set_modified(UNIX_EPOCH + Duration::from_secs(mtime)).map_err(io_err(format!("set the time of {}", path.display())))?;
        drop(file);
        set_mode(&path, mode)
    }

    #[cfg(unix)]
    fn symlink(&mut self, rel: &str, target: &str, _: u64) -> Result<()> {
        let path = self.parent_of(rel)?;
        std::os::unix::fs::symlink(target, &path).map_err(io_err(format!("make the link {}", path.display())))
    }

    #[cfg(not(unix))]
    fn symlink(&mut self, rel: &str, _: &str, _: u64) -> Result<()> {
        Err(unsafe_entry(rel, "is a link, which this system does not restore"))
    }
}

/// Writes the checked entries out again as a clean tar.
impl<W: Write> Sink for TarOut<W> {
    fn dir(&mut self, rel: &str, mode: u32, mtime: u64) -> Result<()> {
        TarOut::dir(self, rel, mode, mtime).map_err(io_err("write the tar"))
    }

    fn file(&mut self, rel: &str, mode: u32, mtime: u64, size: u64, data: &mut dyn Read) -> Result<()> {
        TarOut::file(self, rel, mode, mtime, size, data).map_err(io_err("write the tar"))
    }

    fn symlink(&mut self, rel: &str, target: &str, mtime: u64) -> Result<()> {
        TarOut::symlink(self, rel, target, mtime).map_err(io_err("write the tar"))
    }
}

/// Reads every entry and keeps nothing: a check that a bundle opens.
pub(crate) struct NullSink;

impl Sink for NullSink {
    fn dir(&mut self, _: &str, _: u32, _: u64) -> Result<()> {
        Ok(())
    }

    fn file(&mut self, _: &str, _: u32, _: u64, _: u64, data: &mut dyn Read) -> Result<()> {
        io::copy(data, &mut io::sink()).map(|_| ()).map_err(io_err("read the archive"))
    }

    fn symlink(&mut self, _: &str, _: &str, _: u64) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_paths_must_stay_relative() {
        assert_eq!(relative(b"a/b").unwrap().as_deref(), Some("a/b"));
        assert_eq!(relative(b"./a//b/").unwrap().as_deref(), Some("a/b"));
        assert_eq!(relative(b"./").unwrap(), None);
        for bad in [&b"/etc/passwd"[..], b"../x", b"a/../../x", b"a/..", b"a\\b", b"\xff"] {
            assert!(relative(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn links_must_resolve_inside() {
        assert!(link_stays_inside("a", "b"));
        assert!(link_stays_inside("a/b", "../c"));
        assert!(link_stays_inside("a/b/c", "../../d"));
        assert!(!link_stays_inside("a", "../b"));
        assert!(!link_stays_inside("a/b", "../../c"));
        assert!(!link_stays_inside("a", "/etc/passwd"));
        assert!(!link_stays_inside("a", ""));
        assert!(!link_stays_inside("a/b", "c/../../../d"));
    }
}
