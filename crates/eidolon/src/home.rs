//! A machine's Eidolon home: its holder list, its targets, and for each
//! target this machine's path, the seat history and the current bundle.
//!
//! ```text
//! holders                          allowed_signers of every holder
//! recipients                       extra age recipients, one per line (optional)
//! targets/<name>/target.toml       the rules, the same on every machine
//! targets/<name>/path              where the target is on this machine
//! targets/<name>/seats/<gen>.json  each seat, with <gen>.json.sig beside it
//! targets/<name>/bundles/<sha>.age the bundle the newest seat names
//! targets/<name>/pending/          a move that waits for its ack
//! ```

use crate::archive::{self, Counts, FolderSink, Limits, NullSink, TarOut};
use crate::bundle::{self, Recipient};
use crate::error::{io_err, Code, Error, Result};
use crate::files::{self, write_private};
use crate::holders::{check_name, Holder, Holders};
use crate::key::{HolderKey, KeySource, MachineKey};
use crate::lock::{self, AppLock};
use crate::rules::{accept_chain, accept_seat, may_hold};
use crate::seat::{SeatRecord, SignedSeat};
use crate::swap;
use crate::target::Target;
use serde::Serialize;
use std::fs;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};

pub struct Home {
    root: PathBuf,
    key: MachineKey,
}

/// Where a seal reads the target from.
pub enum Source<'a> {
    /// A folder: the target's path on this machine when none is given.
    Folder(Option<PathBuf>),
    /// A tar stream, such as one a cell writes, filtered by the target's rules.
    Tar(&'a mut dyn Read),
}

/// Where an open puts the target.
pub enum Dest<'a> {
    /// A folder, replaced whole: the target's path on this machine when none is given.
    Folder(Option<PathBuf>),
    /// A clean tar stream, for a caller to unpack elsewhere.
    Tar(&'a mut dyn Write),
}

#[derive(Debug, Clone, Serialize)]
pub struct Sealed {
    pub target: String,
    pub generation: u64,
    pub bundle: String,
    pub size: u64,
    pub counts: Counts,
}

#[derive(Debug, Clone, Serialize)]
pub struct Opened {
    pub target: String,
    pub generation: u64,
    pub to: Option<PathBuf>,
    pub counts: Counts,
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub target: String,
    pub about: String,
    pub path: Option<PathBuf>,
    pub generation: Option<u64>,
    pub holder: Option<String>,
    pub held_here: bool,
    pub pending_to: Option<String>,
    pub bundle: Option<String>,
    pub bundle_size: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Verified {
    pub target: String,
    pub generation: u64,
    pub holder: String,
    pub seats_checked: usize,
    /// The bundle decrypted and every entry passed the restore checks.
    pub opened: Option<Counts>,
}

impl Home {
    pub fn open(root: impl Into<PathBuf>, key: &KeySource) -> Result<Home> {
        let root = root.into();
        files::private_dir(&root).map_err(io_err(format!("make the home {}", root.display())))?;
        let key = MachineKey::load_or_create(key)?;
        Ok(Home { root, key })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn key(&self) -> &MachineKey {
        &self.key
    }

    pub(crate) fn me(&self) -> &HolderKey {
        self.key.public()
    }

    pub fn holders(&self) -> Result<Holders> {
        match fs::read_to_string(self.root.join("holders")) {
            Ok(text) => Holders::parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Holders::default()),
            Err(e) => Err(io_err("read the holder list")(e)),
        }
    }

    pub(crate) fn holders_text(&self) -> Result<String> {
        Ok(self.holders()?.render())
    }

    pub fn add_holder(&self, name: &str, key: HolderKey) -> Result<bool> {
        check_name("holder", name)?;
        let mut holders = self.holders()?;
        let added = holders.add(Holder { name: name.to_owned(), key })?;
        write_private(&self.root.join("holders"), holders.render().as_bytes()).map_err(io_err("write the holder list"))?;
        Ok(added)
    }

    pub(crate) fn dir(&self, name: &str) -> PathBuf {
        self.root.join("targets").join(name)
    }

    /// Adds a target, or sets this machine's path for one already here. Rules
    /// that differ from the ones already here are refused.
    pub fn add_target(&self, name: &str, target: Option<Target>, path: Option<PathBuf>) -> Result<Status> {
        check_name("target", name)?;
        let dir = self.dir(name);
        let existing = self.target_if_any(name)?;
        match (&existing, target) {
            (Some(have), Some(want)) if *have != want => {
                return Err(Error::refused(Code::TargetDiffers, format!("{name} is already here with other rules."), "Pick another name, or keep the rules it has."));
            }
            (None, None) => {
                let target = Target::preset(name).ok_or_else(|| unknown_target(name))?;
                self.write_target(name, &target)?;
            }
            (None, Some(target)) => self.write_target(name, &target)?,
            _ => {}
        }
        if let Some(path) = path {
            let path = std::path::absolute(&path).map_err(io_err(format!("read the path {}", path.display())))?;
            let text = path.to_str().ok_or_else(|| Error::refused(Code::NoPath, "The path is not UTF-8.", "Use a path made of UTF-8 names."))?;
            write_private(&dir.join("path"), text.as_bytes()).map_err(io_err("write the target's path"))?;
        }
        self.status(name)
    }

    pub(crate) fn write_target(&self, name: &str, target: &Target) -> Result<()> {
        let dir = self.dir(name);
        files::private_dir(&dir).map_err(io_err(format!("make {}", dir.display())))?;
        write_private(&dir.join("target.toml"), target.to_toml().as_bytes()).map_err(io_err("write the target"))
    }

    pub(crate) fn target_if_any(&self, name: &str) -> Result<Option<Target>> {
        check_name("target", name)?;
        match fs::read_to_string(self.dir(name).join("target.toml")) {
            Ok(text) => Target::parse(&text).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_err(format!("read the target {name}"))(e)),
        }
    }

    pub fn target(&self, name: &str) -> Result<Target> {
        self.target_if_any(name)?.ok_or_else(|| unknown_target(name))
    }

    pub fn targets(&self) -> Result<Vec<String>> {
        let mut names = Vec::new();
        match fs::read_dir(self.root.join("targets")) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry.map_err(io_err("list the targets"))?;
                    if let Some(name) = entry.file_name().to_str() {
                        if entry.path().join("target.toml").is_file() {
                            names.push(name.to_owned());
                        }
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_err("list the targets")(e)),
        }
        names.sort();
        Ok(names)
    }

    pub fn path_of(&self, name: &str) -> Result<Option<PathBuf>> {
        match fs::read_to_string(self.dir(name).join("path")) {
            Ok(text) => Ok(Some(PathBuf::from(text.trim_end_matches('\n')))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_err("read the target's path")(e)),
        }
    }

    fn path_or(&self, name: &str, given: Option<PathBuf>) -> Result<PathBuf> {
        match given {
            Some(p) => std::path::absolute(&p).map_err(io_err(format!("read the path {}", p.display()))),
            None => self.path_of(name)?.ok_or_else(|| Error::refused(Code::NoPath, format!("{name} has no path on this machine."), format!("Run `eidolon target add {name} --path <folder>`, or pass a folder."))),
        }
    }

    /// The seat history, checked link by link from generation 1.
    pub fn seats(&self, name: &str, holders: &Holders) -> Result<Vec<SignedSeat>> {
        let dir = self.dir(name).join("seats");
        let mut found = Vec::new();
        match fs::read_dir(&dir) {
            Ok(entries) => {
                for entry in entries {
                    let path = entry.map_err(io_err("list the seats"))?.path();
                    if path.extension().is_some_and(|e| e == "json") {
                        let generation: u64 = path.file_stem().and_then(|s| s.to_str()).and_then(|s| s.parse().ok()).unwrap_or(0);
                        found.push((generation, path));
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io_err("list the seats")(e)),
        }
        found.sort();
        let mut chain = Vec::with_capacity(found.len());
        for (_, path) in found {
            let bytes = fs::read(&path).map_err(io_err(format!("read {}", path.display())))?;
            let sig = fs::read_to_string(path.with_extension("json.sig")).map_err(io_err(format!("read the signature of {}", path.display())))?;
            chain.push(SignedSeat::from_parts(bytes, sig)?);
        }
        if !chain.is_empty() {
            accept_chain(holders, name, None, &chain)?;
        }
        Ok(chain)
    }

    /// Writes a seat: the signature first, then the record, so a record on disk always has its signature.
    pub(crate) fn write_seat(&self, name: &str, seat: &SignedSeat) -> Result<()> {
        let dir = self.dir(name).join("seats");
        files::private_dir(&dir).map_err(io_err("make the seats folder"))?;
        let file = dir.join(format!("{:020}.json", seat.record.generation));
        write_private(&file.with_extension("json.sig"), seat.signature().as_bytes()).map_err(io_err("write a seat's signature"))?;
        write_private(&file, seat.bytes()).map_err(io_err("write a seat"))
    }

    pub(crate) fn bundle_path(&self, name: &str, sha256: &str) -> PathBuf {
        self.dir(name).join("bundles").join(format!("{sha256}.age"))
    }

    /// Removes every bundle but `keep`.
    pub(crate) fn prune_bundles(&self, name: &str, keep: Option<&str>) -> Result<()> {
        let dir = self.dir(name).join("bundles");
        let Ok(entries) = fs::read_dir(&dir) else { return Ok(()) };
        for entry in entries {
            let path = entry.map_err(io_err("list the bundles"))?.path();
            let kept = keep.is_some_and(|k| path.file_name().and_then(|n| n.to_str()) == Some(&format!("{k}.age")));
            if !kept {
                files::remove_if_present(&path).map_err(io_err(format!("remove {}", path.display())))?;
            }
        }
        Ok(())
    }

    fn recipients(&self, holders: &Holders, extra: Vec<Recipient>) -> Result<Vec<Recipient>> {
        let mut all = holders.iter().map(|h| Recipient::holder(&h.key)).collect::<Result<Vec<_>>>()?;
        match fs::read_to_string(self.root.join("recipients")) {
            Ok(text) => all.extend(Recipient::parse_file(&text)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_err("read the recipients file")(e)),
        }
        all.extend(extra);
        Ok(all)
    }

    /// The newest seat, which must name this machine, with no move pending.
    pub(crate) fn held(&self, name: &str, holders: &Holders) -> Result<(Vec<SignedSeat>, SignedSeat)> {
        if let Some(next) = self.pending(name, holders)? {
            return Err(Error::refused(
                Code::MovePending,
                format!("A move of {name} to {} waits for its ack.", holders.name_of(&next.record.holder)),
                format!("Run `eidolon settle {name} --confirm <ack>` or `--abandon` first."),
            ));
        }
        let chain = self.seats(name, holders)?;
        let last = chain.last().cloned().ok_or_else(|| Error::refused(Code::NotSealed, format!("{name} has not been sealed on this machine."), format!("Run `eidolon seal {name}` on the machine that has it.")))?;
        if last.record.holder != *self.me() {
            return Err(Error::refused(
                Code::NotTheHolder,
                format!("{} holds {name} now (generation {}).", holders.name_of(&last.record.holder), last.record.generation),
                "Move it here from that machine first.",
            ));
        }
        Ok((chain, last))
    }

    /// Captures the target into a new bundle and signs the next seat for this machine.
    pub fn seal(&self, name: &str, source: Source<'_>, extra: Vec<Recipient>) -> Result<Sealed> {
        let target = self.target(name)?;
        let rules = target.rules()?;
        let holders = self.holders()?;
        may_hold(&holders, self.me())?;
        let chain = match self.held(name, &holders) {
            Ok((chain, _)) => chain,
            Err(e) if e.code() == Some(Code::NotSealed) => Vec::new(),
            Err(e) => return Err(e),
        };
        let recipients = self.recipients(&holders, extra)?;
        let dir = self.dir(name).join("bundles");
        files::private_dir(&dir).map_err(io_err("make the bundles folder"))?;
        let tmp = files::temp_beside(&dir.join("new"));
        let file = files::create_private(&tmp).map_err(io_err("create the bundle"))?;
        let sealed = bundle::seal(&recipients, BufWriter::new(file), |tar| {
            let mut out = TarOut::new(tar);
            match source {
                Source::Folder(given) => {
                    let path = self.path_or(name, given)?;
                    swap::recover(&path)?;
                    let _lock = hold(&path, &target)?;
                    archive::capture_folder(&path, &rules, &mut out)?;
                }
                Source::Tar(input) => archive::capture_tar(input, &rules, &mut out)?,
            }
            Ok(out.finish().map_err(io_err("finish the tar"))?.1)
        });
        let (counts, written) = match sealed {
            Ok(v) => v,
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                return Err(e);
            }
        };
        written.out.into_inner().map_err(|e| io_err("write the bundle")(e.into_error())).and_then(|f| f.sync_all().map_err(io_err("write the bundle")))?;
        let bundle = self.bundle_path(name, &written.sha256);
        fs::rename(&tmp, &bundle).map_err(io_err("keep the bundle"))?;
        let last = chain.last();
        let record = SeatRecord {
            target: name.to_owned(),
            generation: last.map_or(1, |s| s.record.generation + 1),
            holder: self.me().clone(),
            previous: last.map(|s| s.record.holder.clone()),
            bundle: written.sha256.clone(),
            at: files::now(),
        };
        let seat = SignedSeat::sign(record, &self.key)?;
        accept_seat(&holders, name, last, &seat)?;
        self.write_seat(name, &seat)?;
        self.prune_bundles(name, Some(&written.sha256))?;
        Ok(Sealed { target: name.to_owned(), generation: seat.record.generation, bundle: written.sha256, size: written.size, counts })
    }

    /// Restores the newest bundle, into a folder or as a tar.
    pub fn open_target(&self, name: &str, dest: Dest<'_>, limits: Limits) -> Result<Opened> {
        let target = self.target(name)?;
        let holders = self.holders()?;
        let (_, last) = self.held(name, &holders)?;
        let bundle = self.checked_bundle(name, &last)?;
        let generation = last.record.generation;
        match dest {
            Dest::Folder(given) => {
                let path = self.path_or(name, given)?;
                swap::recover(&path)?;
                let lock = hold(&path, &target)?;
                let counts = swap::replace(&path, lock, |fresh| {
                    let mut sink = FolderSink::new(fresh);
                    let counts = archive::restore(bundle::open(&bundle, &self.key)?, limits, &mut sink)?;
                    sink.finish()?;
                    Ok(counts)
                })?;
                Ok(Opened { target: name.to_owned(), generation, to: Some(path), counts })
            }
            Dest::Tar(out) => {
                let mut tar = TarOut::new(out);
                let counts = archive::restore(bundle::open(&bundle, &self.key)?, limits, &mut tar)?;
                tar.finish().map_err(io_err("finish the tar"))?;
                Ok(Opened { target: name.to_owned(), generation, to: None, counts })
            }
        }
    }

    pub(crate) fn checked_bundle(&self, name: &str, seat: &SignedSeat) -> Result<PathBuf> {
        let path = self.bundle_path(name, &seat.record.bundle);
        if !path.is_file() {
            return Err(Error::refused(Code::NotSealed, format!("The bundle of generation {} is missing.", seat.record.generation), format!("Seal {name} again, or move it here again.")));
        }
        if bundle::sha256_of(&path)? != seat.record.bundle {
            return Err(Error::refused(Code::BundleChanged, "The bundle's bytes differ from the ones its seat names.", format!("Seal {name} again, or move it here again.")));
        }
        Ok(path)
    }

    /// Checks the seat history, and where this machine holds the target,
    /// that the bundle matches its seat and opens.
    pub fn verify(&self, name: &str, limits: Limits) -> Result<Verified> {
        self.target(name)?;
        let holders = self.holders()?;
        let chain = self.seats(name, &holders)?;
        let last = chain.last().ok_or_else(|| Error::refused(Code::NotSealed, format!("{name} has no seat here."), format!("Run `eidolon seal {name}`.")))?;
        let opened = if last.record.holder == *self.me() {
            let bundle = self.checked_bundle(name, last)?;
            Some(archive::restore(bundle::open(&bundle, &self.key)?, limits, &mut NullSink)?)
        } else {
            None
        };
        Ok(Verified { target: name.to_owned(), generation: last.record.generation, holder: holders.name_of(&last.record.holder), seats_checked: chain.len(), opened })
    }

    pub fn status(&self, name: &str) -> Result<Status> {
        let target = self.target(name)?;
        let holders = self.holders()?;
        let chain = self.seats(name, &holders)?;
        let last = chain.last();
        let pending = self.pending(name, &holders)?;
        let bundle = last.map(|s| self.bundle_path(name, &s.record.bundle)).filter(|p| p.is_file());
        Ok(Status {
            target: name.to_owned(),
            about: target.about,
            path: self.path_of(name)?,
            generation: last.map(|s| s.record.generation),
            holder: last.map(|s| holders.name_of(&s.record.holder)),
            held_here: last.is_some_and(|s| s.record.holder == *self.me()),
            pending_to: pending.map(|p| holders.name_of(&p.record.holder)),
            bundle: last.map(|s| s.record.bundle.clone()),
            bundle_size: bundle.and_then(|p| fs::metadata(p).ok()).map(|m| m.len()),
        })
    }

    pub fn ls(&self) -> Result<Vec<Status>> {
        self.targets()?.iter().map(|t| self.status(t)).collect()
    }
}

pub(crate) fn hold(path: &Path, target: &Target) -> Result<Option<AppLock>> {
    target.closed.as_ref().map_or(Ok(None), |c| lock::hold(path, c))
}

pub(crate) fn unknown_target(name: &str) -> Error {
    let presets: Vec<_> = Target::presets().collect();
    Error::refused(Code::UnknownTarget, format!("There is no target {name:?} here."), format!("Add it with `eidolon target add {name} --path <folder>` (presets: {}) or `--file <target.toml>`.", presets.join(", ")))
}
