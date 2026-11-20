//! The tar inside a bundle. Capture writes it from a folder or from another
//! tar, filtered by a target's rules. Restore reads it entry by entry and
//! refuses anything that could write outside the folder it fills. Both run
//! every entry through the same [`Checker`], so a seal never keeps what an
//! open would refuse.

use crate::error::{io_err, Code, Error, Result};
use crate::target::Rules;
use serde::Serialize;
use std::cell::Cell;
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tar::{EntryType, Header};

/// The largest GNU long name, long link or PAX header read.
const MAX_EXTENSION: u64 = 64 << 10;
/// 9999-12-31, the latest modification time set on a restored file.
const MAX_MTIME: u64 = 253_402_300_799;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub files: u64,
    pub dirs: u64,
    pub links: u64,
    pub bytes: u64,
    /// Sockets, devices, FIFOs and hard links in a capture's source, which a bundle never holds.
    pub skipped: u64,
}

/// How much a capture or a restore takes before it refuses with `too_large`.
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
    Error::refused(
        Code::TooLarge,
        format!("The archive holds more than {} bytes or {} entries.", limits.max_bytes, limits.max_entries),
        "Raise --max-size if the target is really that big.",
    )
}

/// What kind of entry the checker sees.
pub(crate) enum Kind<'a> {
    File(u64),
    Dir,
    Link(&'a str),
}

/// The rules every entry of a bundle meets, kept across the whole archive:
/// relative paths, no twins (ignoring case), nothing under a link, links
/// that resolve inside without passing through another link, no setuid,
/// setgid or sticky bit, and the limits.
pub(crate) struct Checker {
    limits: Limits,
    seen: HashSet<String>,
    parents: HashSet<String>,
    links: HashSet<String>,
    through: HashSet<String>,
    bytes: u64,
    entries: u64,
}

impl Checker {
    pub fn new(limits: Limits) -> Checker {
        Checker { limits, seen: HashSet::new(), parents: HashSet::new(), links: HashSet::new(), through: HashSet::new(), bytes: 0, entries: 0 }
    }

    pub fn check(&mut self, rel: &str, kind: &Kind<'_>, mode: u32) -> Result<()> {
        self.entries += 1;
        if self.entries > self.limits.max_entries {
            return Err(too_large(&self.limits));
        }
        let folded = rel.to_lowercase();
        if self.seen.contains(&folded) {
            return Err(unsafe_entry(rel, "appears twice"));
        }
        if prefixes(&folded).any(|p| self.links.contains(p)) {
            return Err(unsafe_entry(rel, "lies under a link"));
        }
        if mode & 0o7000 != 0 {
            return Err(unsafe_entry(rel, "has a setuid, setgid or sticky bit"));
        }
        match kind {
            Kind::File(size) => {
                self.bytes = self.bytes.checked_add(*size).filter(|b| *b <= self.limits.max_bytes).ok_or_else(|| too_large(&self.limits))?;
            }
            Kind::Dir => {}
            Kind::Link(target) => {
                if self.parents.contains(&folded) || self.through.contains(&folded) {
                    return Err(unsafe_entry(rel, "is a link where another entry already goes through"));
                }
                let passed = resolve_link(&folded, &target.to_lowercase(), &self.links).ok_or_else(|| unsafe_entry(rel, "links outside the folder or through another link"))?;
                self.through.extend(passed);
            }
        }
        self.parents.extend(prefixes(&folded).map(str::to_owned));
        if matches!(kind, Kind::Link(_)) {
            self.links.insert(folded.clone());
        }
        self.seen.insert(folded);
        Ok(())
    }
}

/// `a` and `a/b` for `a/b/c`: the folders above a path.
fn prefixes(rel: &str) -> impl Iterator<Item = &str> {
    rel.match_indices('/').map(|(i, _)| &rel[..i])
}

/// Resolves a link at `rel` to `target` without touching the disk. Returns
/// the folders it passes on the way, or none when it climbs out of the
/// folder, is absolute, or passes through one of `links`.
fn resolve_link(rel: &str, target: &str, links: &HashSet<String>) -> Option<Vec<String>> {
    if target.is_empty() || target.starts_with(['/', '\\']) || target.contains(['\\', '\0', ':']) {
        return None;
    }
    let mut at: Vec<&str> = rel.split('/').collect();
    at.pop();
    let mut passed = Vec::new();
    let parts: Vec<&str> = target.split('/').filter(|p| !p.is_empty() && *p != ".").collect();
    for (i, part) in parts.iter().enumerate() {
        if *part == ".." {
            at.pop()?;
        } else {
            at.push(part);
        }
        if i + 1 < parts.len() && !at.is_empty() {
            let here = at.join("/");
            if links.contains(&here) {
                return None;
            }
            passed.push(here);
        }
    }
    Some(passed)
}

/// Writes clean entries: relative paths, permission bits only, no owners.
/// With a checker, every entry must pass it first.
pub(crate) struct TarOut<W: Write> {
    builder: tar::Builder<W>,
    checker: Option<Checker>,
    pub counts: Counts,
}

impl<W: Write> TarOut<W> {
    pub fn new(out: W) -> TarOut<W> {
        TarOut { builder: tar::Builder::new(out), checker: None, counts: Counts::default() }
    }

    pub fn checked(out: W, limits: Limits) -> TarOut<W> {
        TarOut { builder: tar::Builder::new(out), checker: Some(Checker::new(limits)), counts: Counts::default() }
    }

    fn header(kind: EntryType, mode: u32, mtime: u64, size: u64) -> Header {
        let mut h = Header::new_gnu();
        h.set_entry_type(kind);
        h.set_mode(mode & 0o777);
        h.set_mtime(mtime.min(MAX_MTIME));
        h.set_size(size);
        h.set_uid(0);
        h.set_gid(0);
        h
    }

    fn check(&mut self, rel: &str, kind: &Kind<'_>, mode: u32) -> Result<()> {
        self.checker.as_mut().map_or(Ok(()), |c| c.check(rel, kind, mode & 0o777))
    }

    pub fn dir(&mut self, rel: &str, mode: u32, mtime: u64) -> Result<()> {
        self.check(rel, &Kind::Dir, mode)?;
        let mut h = Self::header(EntryType::Directory, mode, mtime, 0);
        self.builder.append_data(&mut h, rel, io::empty()).map_err(io_err(format!("archive {rel}")))?;
        self.counts.dirs += 1;
        Ok(())
    }

    pub fn file(&mut self, rel: &str, mode: u32, mtime: u64, size: u64, data: &mut dyn Read) -> Result<()> {
        self.check(rel, &Kind::File(size), mode)?;
        let mut h = Self::header(EntryType::Regular, mode, mtime, size);
        self.builder.append_data(&mut h, rel, Exact { inner: data, left: size }).map_err(io_err(format!("archive {rel}")))?;
        self.counts.files += 1;
        self.counts.bytes += size;
        Ok(())
    }

    pub fn symlink(&mut self, rel: &str, target: &str, mtime: u64) -> Result<()> {
        self.check(rel, &Kind::Link(target), 0o777)?;
        let mut h = Self::header(EntryType::Symlink, 0o777, mtime, 0);
        self.builder.append_link(&mut h, rel, target).map_err(io_err(format!("archive {rel}")))?;
        self.counts.links += 1;
        Ok(())
    }

    pub fn finish(self) -> Result<(W, Counts)> {
        let counts = self.counts;
        Ok((self.builder.into_inner().map_err(io_err("finish the tar"))?, counts))
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

/// Opens a file without following a link that took its place since it was listed.
fn open_no_follow(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits().cast_signed());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path)
}

/// Captures the folder `root` by `rules`, in name order, without following links.
pub(crate) fn capture_folder<W: Write>(root: &Path, rules: &Rules, out: &mut TarOut<W>) -> Result<()> {
    if !root.is_dir() {
        return Err(Error::refused(Code::NoPath, format!("{} is not a folder.", root.display()), "Give the target's folder with `eidolon target add <name> --path` or --from."));
    }
    walk(root, "", rules, out)
}

fn walk<W: Write>(dir: &Path, prefix: &str, rules: &Rules, out: &mut TarOut<W>) -> Result<()> {
    let listing = || io_err(format!("list {}", dir.display()));
    let mut names: Vec<_> = fs::read_dir(dir).map_err(listing())?.map(|e| e.map(|e| e.file_name())).collect::<io::Result<_>>().map_err(listing())?;
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
        let keep = rules.keeps(&rel);
        if kind.is_dir() {
            if keep {
                out.dir(&rel, mode_of(&meta), secs(meta.modified()))?;
            }
            walk(&path, &rel, rules, out)?;
        } else if !keep {
        } else if kind.is_file() {
            capture_file(&path, &rel, out)?;
        } else if kind.is_symlink() {
            let target = fs::read_link(&path).map_err(io_err(format!("read the link {}", path.display())))?;
            let target = target.to_str().ok_or_else(|| unsafe_entry(&rel, "links to a name that is not UTF-8"))?;
            out.symlink(&rel, target, secs(meta.modified()))?;
        } else {
            out.counts.skipped += 1;
        }
    }
    Ok(())
}

/// A file is read from the descriptor it was opened as, and must keep its
/// size from open to end.
fn capture_file<W: Write>(path: &Path, rel: &str, out: &mut TarOut<W>) -> Result<()> {
    let changed = || Error::refused(Code::Changed, format!("{rel} changed while it was read."), "Close the app that writes it, then seal again.");
    let mut file = open_no_follow(path).map_err(|_| changed())?;
    let meta = file.metadata().map_err(io_err(format!("read {}", path.display())))?;
    if !meta.is_file() {
        return Err(changed());
    }
    let size = meta.len();
    out.file(rel, mode_of(&meta), secs(meta.modified()), size, &mut file).map_err(|e| match e {
        Error::Io { .. } => changed(),
        e @ Error::Refused(_) => e,
    })?;
    let mut extra = [0u8; 1];
    let grew = file.read(&mut extra).map_err(io_err(format!("read {}", path.display())))? > 0;
    if grew || file.metadata().map_err(io_err(format!("read {}", path.display())))?.len() != size {
        return Err(changed());
    }
    Ok(())
}

/// Captures from another tar, such as one a cell streams out, by `rules`.
/// Only files, folders and links cross; their paths must stay relative, and
/// the input is held to the same limits as a restore.
pub(crate) fn capture_tar<W: Write>(input: impl Read, rules: &Rules, limits: Limits, out: &mut TarOut<W>) -> Result<()> {
    let hit = Rc::new(Cell::new(None));
    let guarded = guard(input, limits, &hit);
    let reading = |e: io::Error| hit_error(hit.get(), &limits).unwrap_or_else(|| io_err("read the tar given to capture")(e));
    let mut archive = tar::Archive::new(guarded);
    for entry in archive.entries().map_err(reading)? {
        let mut entry = entry.map_err(reading)?;
        let Some(rel) = relative(&entry.path_bytes())? else { continue };
        if !rules.keeps(&rel) {
            continue;
        }
        let h = entry.header();
        let (mode, mtime) = (h.mode().unwrap_or(0o600), h.mtime().unwrap_or(0));
        match h.entry_type() {
            EntryType::Regular | EntryType::Continuous => {
                let size = entry.size();
                out.file(&rel, mode, mtime, size, &mut entry).map_err(|e| hit_error(hit.get(), &limits).unwrap_or(e))?;
            }
            EntryType::Directory => out.dir(&rel, mode, mtime)?,
            EntryType::Symlink => {
                let target = entry.link_name_bytes().ok_or_else(|| unsafe_entry(&rel, "is a link without a target"))?;
                let target = std::str::from_utf8(&target).map_err(|_| unsafe_entry(&rel, "links to a name that is not UTF-8"))?.to_owned();
                out.symlink(&rel, &target, mtime)?;
            }
            _ => out.counts.skipped += 1,
        }
    }
    Ok(())
}

/// Names Windows keeps for devices, which a file cannot have there.
#[cfg(windows)]
fn reserved_on_windows(part: &str) -> bool {
    let stem = part.split('.').next().unwrap_or(part).to_ascii_uppercase();
    let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || (stem.len() == 4 && (stem.starts_with("COM") || stem.starts_with("LPT")) && stem.as_bytes()[3].is_ascii_digit() && stem.as_bytes()[3] != b'0');
    device || part.ends_with(['.', ' '])
}

#[cfg(not(windows))]
fn reserved_on_windows(_: &str) -> bool {
    false
}

/// An entry's path as `a/b/c`, or none for the archive's root itself.
/// Absolute paths, `..`, backslashes and names that are not UTF-8 are
/// refused, and on Windows its device names and names ending in a dot or space.
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
            p if reserved_on_windows(p) => return Err(unsafe_entry(text, "has a name Windows keeps for itself")),
            p => parts.push(p),
        }
    }
    Ok((!parts.is_empty()).then(|| parts.join("/")))
}

/// Where restored entries go once they have passed the checks.
pub(crate) trait Sink {
    fn dir(&mut self, rel: &str, mode: u32, mtime: u64) -> Result<()>;
    fn file(&mut self, rel: &str, mode: u32, mtime: u64, size: u64, data: &mut dyn Read) -> Result<()>;
    fn symlink(&mut self, rel: &str, target: &str, mtime: u64) -> Result<()>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Hit {
    TooLarge,
    Extension,
    Sparse,
}

pub(crate) fn hit_error(hit: Option<Hit>, limits: &Limits) -> Option<Error> {
    hit.map(|h| match h {
        Hit::TooLarge => too_large(limits),
        Hit::Extension => unsafe_entry("(extension header)", &format!("is a long name or PAX header over {MAX_EXTENSION} bytes")),
        Hit::Sparse => unsafe_entry("(sparse file)", "is a sparse file, which a bundle never holds"),
    })
}

pub(crate) fn guard<R: Read>(input: R, limits: Limits, hit: &Rc<Cell<Option<Hit>>>) -> Guard<R> {
    let cap = limits.max_bytes.saturating_add(limits.max_entries.saturating_mul(1536)).saturating_add(1 << 20);
    Guard { inner: input, left: cap, hit: hit.clone(), header: Vec::with_capacity(512), data_left: 0, pax: None, pax_size: None, collect_left: 0 }
}

/// Reads ahead of the tar crate block by block. It stops a stream with more
/// bytes than the limits allow, and refuses a GNU long name, long link or
/// PAX header over [`MAX_EXTENSION`], and sparse files, before the tar crate
/// reads one into memory.
pub(crate) struct Guard<R> {
    inner: R,
    left: u64,
    hit: Rc<Cell<Option<Hit>>>,
    header: Vec<u8>,
    data_left: u64,
    pax: Option<Vec<u8>>,
    pax_size: Option<u64>,
    collect_left: u64,
}

fn field_size(field: &[u8]) -> Option<u64> {
    if field.first().is_some_and(|b| b & 0x80 != 0) {
        return field[1..].iter().try_fold(u64::from(field[0] & 0x7f), |n, b| n.checked_mul(256).and_then(|n| n.checked_add(u64::from(*b))));
    }
    let text = std::str::from_utf8(field).ok()?.trim_matches(|c: char| c == '\0' || c == ' ');
    if text.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(text, 8).ok()
}

fn pax_size(records: &[u8]) -> Option<u64> {
    let text = String::from_utf8_lossy(records);
    text.lines().filter_map(|l| l.split_once(' ').map(|(_, kv)| kv)).find_map(|kv| kv.strip_prefix("size=")).and_then(|v| v.trim().parse().ok())
}

impl<R> Guard<R> {
    fn fail(&self, hit: Hit) -> io::Error {
        self.hit.set(Some(hit));
        io::Error::other("the archive broke a limit")
    }

    fn observe(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            if self.data_left > 0 {
                let n = bytes.len().min(usize::try_from(self.data_left).unwrap_or(usize::MAX));
                if let Some(pax) = self.pax.as_mut() {
                    let take = n.min(usize::try_from(self.collect_left).unwrap_or(usize::MAX));
                    pax.extend_from_slice(&bytes[..take]);
                    self.collect_left -= take as u64;
                }
                self.data_left -= n as u64;
                bytes = &bytes[n..];
                if self.data_left == 0 {
                    if let Some(pax) = self.pax.take() {
                        self.pax_size = pax_size(&pax);
                    }
                }
                continue;
            }
            let n = bytes.len().min(512 - self.header.len());
            self.header.extend_from_slice(&bytes[..n]);
            bytes = &bytes[n..];
            if self.header.len() == 512 {
                let block = std::mem::take(&mut self.header);
                self.start_entry(&block)?;
            }
        }
        Ok(())
    }

    fn start_entry(&mut self, block: &[u8]) -> io::Result<()> {
        if block.iter().all(|b| *b == 0) {
            return Ok(());
        }
        let size = field_size(&block[124..136]).ok_or_else(|| self.fail(Hit::Extension))?;
        let kind = block[156];
        let extension = matches!(kind, b'L' | b'K' | b'x' | b'g');
        if extension && size > MAX_EXTENSION {
            return Err(self.fail(Hit::Extension));
        }
        if kind == b'S' {
            return Err(self.fail(Hit::Sparse));
        }
        let size = if extension { size } else { self.pax_size.take().unwrap_or(size) };
        if kind == b'x' {
            self.pax = Some(Vec::new());
            self.collect_left = size;
        }
        self.data_left = size.div_ceil(512).checked_mul(512).ok_or_else(|| self.fail(Hit::TooLarge))?;
        Ok(())
    }
}

impl<R: Read> Read for Guard<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.left == 0 {
            return Err(self.fail(Hit::TooLarge));
        }
        let want = buf.len().min(usize::try_from(self.left).unwrap_or(usize::MAX));
        let n = self.inner.read(&mut buf[..want])?;
        self.left -= n as u64;
        self.observe(&buf[..n])?;
        Ok(n)
    }
}

/// Reads a tar and hands each entry to `sink` once it is known to be safe.
/// Refused: escaping or absolute paths, links that point out or through
/// another link, paths through a link, hard links, devices, FIFOs, sparse
/// files, setuid, setgid and sticky bits, the same path twice (ignoring
/// case), extension headers over 64 KiB, and more than `limits` allow.
pub(crate) fn restore(input: impl Read, limits: Limits, sink: &mut dyn Sink) -> Result<Counts> {
    let hit = Rc::new(Cell::new(None));
    let guarded = guard(input, limits, &hit);
    let reading = |e: io::Error| hit_error(hit.get(), &limits).unwrap_or_else(|| io_err("read the archive")(e));
    let mut archive = tar::Archive::new(guarded);
    let mut checker = Checker::new(limits);
    let mut counts = Counts::default();
    for entry in archive.entries().map_err(reading)? {
        let mut entry = entry.map_err(reading)?;
        let kind = entry.header().entry_type();
        let Some(rel) = relative(&entry.path_bytes())? else {
            if kind == EntryType::Directory {
                continue;
            }
            return Err(unsafe_entry(".", "is not a folder"));
        };
        let h = entry.header();
        let mode = h.mode().map_err(|_| unsafe_entry(&rel, "has a mode that does not read"))?;
        let mtime = h.mtime().unwrap_or(0).min(MAX_MTIME);
        match kind {
            EntryType::Regular | EntryType::Continuous => {
                let size = entry.size();
                checker.check(&rel, &Kind::File(size), mode)?;
                sink.file(&rel, mode, mtime, size, &mut entry).map_err(|e| hit_error(hit.get(), &limits).unwrap_or(e))?;
                counts.files += 1;
                counts.bytes += size;
            }
            EntryType::Directory => {
                checker.check(&rel, &Kind::Dir, mode)?;
                sink.dir(&rel, mode, mtime)?;
                counts.dirs += 1;
            }
            EntryType::Symlink => {
                let target = entry.link_name_bytes().ok_or_else(|| unsafe_entry(&rel, "is a link without a target"))?;
                let target = std::str::from_utf8(&target).map_err(|_| unsafe_entry(&rel, "links to a name that is not UTF-8"))?.to_owned();
                checker.check(&rel, &Kind::Link(&target), mode & 0o777)?;
                sink.symlink(&rel, &target, mtime)?;
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
        let mut parts = rel.split('/').peekable();
        while let Some(part) = parts.next() {
            path.push(part);
            if parts.peek().is_some() {
                real_dir(&path, rel)?;
            }
        }
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
    crate::files::new_private_dir(path).map_err(io_err(format!("make the folder {}", path.display())))
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o777)).map_err(io_err(format!("set the mode of {}", path.display())))
}

#[cfg(not(unix))]
#[expect(clippy::unnecessary_wraps, reason = "the same signature as the Unix version")]
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
        if let Some(when) = UNIX_EPOCH.checked_add(Duration::from_secs(mtime.min(MAX_MTIME))) {
            file.set_modified(when).map_err(io_err(format!("set the time of {}", path.display())))?;
        }
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
        TarOut::dir(self, rel, mode, mtime)
    }

    fn file(&mut self, rel: &str, mode: u32, mtime: u64, size: u64, data: &mut dyn Read) -> Result<()> {
        TarOut::file(self, rel, mode, mtime, size, data)
    }

    fn symlink(&mut self, rel: &str, target: &str, mtime: u64) -> Result<()> {
        TarOut::symlink(self, rel, target, mtime)
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

    fn inside(rel: &str, target: &str) -> bool {
        resolve_link(rel, target, &HashSet::new()).is_some()
    }

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
        assert!(inside("a", "b"));
        assert!(inside("a/b", "../c"));
        assert!(inside("a/b/c", "../../d"));
        assert!(!inside("a", "../b"));
        assert!(!inside("a/b", "../../c"));
        assert!(!inside("a", "/etc/passwd"));
        assert!(!inside("a", ""));
        assert!(!inside("a/b", "c/../../../d"));
        let links = HashSet::from(["a/b/l".to_owned()]);
        assert!(resolve_link("x", "a/b/l/..", &links).is_none(), "through another link");
        assert!(resolve_link("x", "a/b/l", &links).is_some(), "to another link, which was checked itself");
    }

    #[cfg(unix)]
    #[test]
    fn a_link_swapped_in_for_a_file_is_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("secret"), "outside").unwrap();
        std::os::unix::fs::symlink(dir.path().join("secret"), dir.path().join("f")).unwrap();
        assert!(open_no_follow(&dir.path().join("f")).is_err());
        let mut out = TarOut::checked(Vec::new(), Limits::default());
        assert_eq!(capture_file(&dir.path().join("f"), "f", &mut out).err().and_then(|e| e.code()), Some(Code::Changed));
    }

    #[test]
    fn sizes_read_in_octal_and_base_256() {
        assert_eq!(field_size(b"00000001750\0"), Some(1000));
        assert_eq!(field_size(&[0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x03, 0xe8]), Some(1000));
        assert_eq!(field_size(b"\0\0\0\0\0\0\0\0\0\0\0\0"), Some(0));
        assert_eq!(pax_size(b"20 path=a/long/name\n12 size=99\n"), Some(99));
    }
}
