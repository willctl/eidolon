//! Moving a target to another holder. The sender writes the next seat as
//! pending, then sends an offer: a tar of the target's rules, the holder
//! list, every seat and the bundle. The receiver checks it all, keeps it, and
//! answers with a signed ack. Only a valid ack retires the sender's seat.
//!
//! The offer and the ack are plain bytes, so they travel over any duplex (a
//! spawned `ssh tower eidolon receive`) or as files carried by hand.

use crate::archive::{self, Limits, NullSink, TarOut};
use crate::bundle;
use crate::error::{io_err, Code, Error, Refusal, Result};
use crate::files::{self, write_private};
use crate::holders::Holders;
use crate::home::{hold, lock_file, Home};
use crate::key::{HolderKey, ACK_NAMESPACE};
use crate::rules::{accept_chain, accept_seat, may_hold};
use crate::seat::{SeatRecord, SignedSeat};
use crate::swap;
use crate::target::Target;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cell::Cell;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

const SMALL: u64 = 1 << 20;

/// What a receiver signs about one pending seat, named by the SHA-256 of
/// its bytes: that it took it, or that it refused and kept nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AckStatement {
    pub target: String,
    pub generation: u64,
    pub bundle: String,
    /// The SHA-256 of the offered seat's signed bytes, in hex.
    pub seat: String,
    pub receiver: HolderKey,
    pub taken: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<Refusal>,
}

/// The receiver's answer, as JSON. A signed statement, or an error it did
/// not sign: the offer did not read, or the receiver cannot say for sure
/// that it kept nothing.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ack {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statement: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Refusal>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Moved {
    pub target: String,
    pub to: String,
    pub generation: u64,
    pub cleared: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Received {
    pub target: String,
    pub generation: u64,
    pub from: String,
    /// This machine had already taken that exact seat, and acked again.
    pub again: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Settled {
    Moved(Moved),
    /// The receiver signed that it kept nothing; the seat stays here.
    Refused(Refusal),
    Abandoned,
}

struct Offer {
    target: Target,
    holders: Holders,
    chain: Vec<SignedSeat>,
    next: SignedSeat,
    bundle: PathBuf,
    sha256: String,
}

impl Offer {
    fn next(&self) -> &SignedSeat {
        &self.next
    }
}

/// Why a take failed, and whether the receiver may sign that it kept
/// nothing: only when it failed before anything was written and the
/// receiver does not already have that seat or a later one.
struct Fault {
    error: Error,
    signable: bool,
}

fn signable(error: Error) -> Fault {
    Fault { error, signable: true }
}

fn unsigned(error: Error) -> Fault {
    Fault { error, signable: false }
}

/// A file in the home's `incoming` folder, removed when dropped.
struct Incoming(PathBuf);

impl Drop for Incoming {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

impl Home {
    fn pending_dir(&self, name: &str) -> PathBuf {
        self.dir(name).join("pending")
    }

    /// The seat a move waits to hand over. A pending seat the history
    /// already holds is from a move that finished, and counts as none.
    pub fn pending(&self, name: &str, holders: &Holders) -> Result<Option<SignedSeat>> {
        let file = self.pending_dir(name).join("next.json");
        let bytes = match fs::read(&file) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io_err("read the pending move")(e)),
        };
        let sig = fs::read_to_string(file.with_extension("json.sig")).map_err(io_err("read the pending move's signature"))?;
        let next = SignedSeat::from_parts(bytes, sig)?;
        let chain = self.seats(name, holders)?;
        if chain.last().is_some_and(|s| s.record.generation >= next.record.generation) {
            return Ok(None);
        }
        Ok(Some(next))
    }

    /// Writes the next seat, naming `to`, as pending; or finds the one
    /// already pending for `to`, so a move cut off is sent again unchanged.
    pub fn start_move(&self, name: &str, to: &str) -> Result<SignedSeat> {
        self.target(name)?;
        let _busy = self.lock(name)?;
        self.start_move_held(name, to)
    }

    fn start_move_held(&self, name: &str, to: &str) -> Result<SignedSeat> {
        let key = self.key()?;
        let holders = self.holders()?;
        let receiver = holders
            .by_name(to)
            .ok_or_else(|| Error::refused(Code::UnknownHolder, format!("{to} is not on the holder list."), "Add it with `eidolon holders add` first."))?
            .key
            .clone();
        if receiver == *key.public() {
            return Err(Error::refused(Code::NotAHolder, format!("{to} is this machine."), "Move the target to another machine."));
        }
        if let Some(next) = self.pending(name, &holders)? {
            if next.record.holder == receiver {
                return Ok(next);
            }
            return Err(Error::refused(
                Code::MovePending,
                format!("A move of {name} to {} waits for its ack.", holders.name_of(&next.record.holder)),
                format!("Run `eidolon settle {name} --confirm <ack>` or `--abandon` first."),
            ));
        }
        let (_, last) = self.held(name, &holders)?;
        self.checked_bundle(name, &last)?;
        let generation = last
            .record
            .generation
            .checked_add(1)
            .ok_or_else(|| Error::refused(Code::StaleGeneration, "The seat is at the last generation there is.", "Add the folder as a new target."))?;
        let record = SeatRecord::new(name, generation, receiver, Some(last.record.holder.clone()), last.record.bundle.clone());
        let next = SignedSeat::sign(record, key)?;
        accept_seat(&holders, name, Some(&last), &next)?;
        let dir = self.pending_dir(name);
        files::remove_dir_if_present(&dir).map_err(io_err("clear a finished move"))?;
        write_private(&dir.join("next.json.sig"), next.signature().as_bytes()).map_err(io_err("write the pending move"))?;
        write_private(&dir.join("next.json"), next.bytes()).map_err(io_err("write the pending move"))?;
        Ok(next)
    }

    /// Writes the offer for the pending move of `name`.
    pub fn write_offer(&self, name: &str, out: impl Write) -> Result<()> {
        let holders = self.holders()?;
        let next = self.pending(name, &holders)?.ok_or_else(|| no_pending(name))?;
        let chain = self.seats(name, &holders)?;
        let held = chain.last().ok_or_else(|| no_pending(name))?;
        let bundle = self.checked_bundle(name, held)?;
        let mut tar = TarOut::new(out);
        let target = self.target(name)?.to_toml();
        tar.file("target.toml", 0o600, 0, target.len() as u64, &mut target.as_bytes())?;
        let holders_text = self.holders_text()?;
        tar.file("holders", 0o600, 0, holders_text.len() as u64, &mut holders_text.as_bytes())?;
        for seat in chain.iter().chain(std::iter::once(&next)) {
            let g = seat.record.generation;
            tar.file(&format!("seats/{g}.json"), 0o600, 0, seat.bytes().len() as u64, &mut seat.bytes())?;
            tar.file(&format!("seats/{g}.json.sig"), 0o600, 0, seat.signature().len() as u64, &mut seat.signature().as_bytes())?;
        }
        let writing = || io_err("write the offer");
        let size = fs::metadata(&bundle).map_err(writing())?.len();
        let mut file = fs::File::open(&bundle).map_err(writing())?;
        tar.file("bundle.age", 0o600, 0, size, &mut file)?;
        let (mut out, _) = tar.finish()?;
        out.flush().map_err(writing())
    }

    /// Writes the pending seat and the offer to `out`, for a move by hand.
    pub fn offer(&self, name: &str, to: &str, out: impl Write) -> Result<SignedSeat> {
        self.target(name)?;
        let _busy = self.lock(name)?;
        let next = self.start_move_held(name, to)?;
        self.write_offer(name, out)?;
        Ok(next)
    }

    /// Moves `name` to `to` over a duplex that `connect` opens once the
    /// pending seat is written: writes the offer to its first half, closes
    /// it, reads the ack from the second and settles.
    pub fn send<W: Write, R: Read>(&self, name: &str, to: &str, clear: bool, connect: impl FnOnce() -> Result<(W, R)>) -> Result<Moved> {
        self.target(name)?;
        let _busy = self.lock(name)?;
        self.start_move_held(name, to)?;
        let (to_receiver, mut from_receiver) = connect()?;
        let sent = self.write_offer(name, to_receiver);
        let mut ack = Vec::new();
        let read = from_receiver.by_ref().take(SMALL).read_to_end(&mut ack);
        if ack.is_empty() {
            let why = match (sent.err().map(|e| e.to_string()), read.err().map(|e| e.to_string())) {
                (Some(e), _) | (None, Some(e)) => format!("The receiver sent no ack ({e})"),
                (None, None) => "The receiver sent no ack.".to_owned(),
            };
            return Err(unsettled(name, &why));
        }
        match self.settle_held(name, &ack, clear)? {
            Settled::Moved(moved) => Ok(moved),
            Settled::Refused(r) => Err(Error::Refused(Refusal::new(r.code, r.message, format!("{} The seat stays on this machine.", r.fix)))),
            Settled::Abandoned => Err(unsettled(name, "The move was abandoned.")),
        }
    }

    /// Settles the pending move with the receiver's ack.
    pub fn settle_with_ack(&self, name: &str, ack: &[u8], clear: bool) -> Result<Settled> {
        self.target(name)?;
        let _busy = self.lock(name)?;
        self.settle_held(name, ack, clear)
    }

    fn settle_held(&self, name: &str, ack: &[u8], clear: bool) -> Result<Settled> {
        let holders = self.holders()?;
        let next = self.pending(name, &holders)?.ok_or_else(|| no_pending(name))?;
        let ack: Ack = serde_json::from_slice(ack).map_err(|e| unsettled(name, &format!("The ack does not read: {e}.")))?;
        let (Some(text), Some(signature)) = (ack.statement, ack.signature) else {
            let why = ack.error.map_or_else(|| "The ack is empty.".to_owned(), |r| format!("The receiver could not take it: {r}"));
            return Err(unsettled(name, &why));
        };
        if !next.record.holder.verifies(ACK_NAMESPACE, text.as_bytes(), &signature) {
            return Err(unsettled(name, &format!("The ack is not signed by {}.", holders.name_of(&next.record.holder))));
        }
        let statement: AckStatement = serde_json::from_str(&text).map_err(|e| unsettled(name, &format!("The ack's statement does not read: {e}.")))?;
        let r = &next.record;
        if statement.seat != next.sha256() || statement.target != r.target || statement.generation != r.generation || statement.bundle != r.bundle || statement.receiver != r.holder
        {
            return Err(unsettled(name, "The ack is for another move."));
        }
        if !statement.taken {
            files::remove_dir_if_present(&self.pending_dir(name)).map_err(io_err("clear the pending move"))?;
            let refusal = statement.refusal.unwrap_or_else(|| Refusal::new(Code::BadAck, "The receiver refused without saying why.", "Ask on the receiver."));
            return Ok(Settled::Refused(refusal));
        }
        self.write_seat(name, &next)?;
        files::remove_dir_if_present(&self.pending_dir(name)).map_err(io_err("clear the pending move"))?;
        self.prune_bundles(name, None)?;
        let cleared = if clear { self.clear_held(name)? } else { false };
        Ok(Settled::Moved(Moved { target: name.to_owned(), to: holders.name_of(&next.record.holder), generation: next.record.generation, cleared }))
    }

    /// Drops the pending move. Only safe when the receiver did not take it:
    /// otherwise two machines would each think the other holds the seat.
    pub fn abandon(&self, name: &str) -> Result<Settled> {
        self.target(name)?;
        let _busy = self.lock(name)?;
        let holders = self.holders()?;
        self.pending(name, &holders)?.ok_or_else(|| no_pending(name))?;
        files::remove_dir_if_present(&self.pending_dir(name)).map_err(io_err("clear the pending move"))?;
        Ok(Settled::Abandoned)
    }

    /// Removes this machine's live copy of a target it no longer holds.
    pub fn clear(&self, name: &str) -> Result<bool> {
        self.target(name)?;
        let _busy = self.lock(name)?;
        self.clear_held(name)
    }

    fn clear_held(&self, name: &str) -> Result<bool> {
        let target = self.target(name)?;
        let holders = self.holders()?;
        let chain = self.seats(name, &holders)?;
        if chain.last().is_some_and(|s| Some(&s.record.holder) == self.public_key()) {
            return Err(Error::refused(Code::NotTheHolder, format!("This machine still holds {name}."), "Move it first; only a copy that moved away is cleared."));
        }
        let Some(path) = self.path_of(name)? else { return Ok(false) };
        swap::recover(&path)?;
        let lock = hold(&path, &target)?;
        swap::remove(&path, lock)
    }

    /// Reads an offer, takes the seat if every check passes, and writes the
    /// ack. A refusal is signed only when nothing was written and this
    /// machine does not already hold that seat or a later one.
    pub fn receive(&self, input: impl Read, mut output: impl Write, limits: Limits) -> Result<Received> {
        self.key()?;
        let _busy = lock_file(&self.root().join("receive.lock"), "a receive")?;
        let incoming = self.root().join("incoming");
        files::remove_dir_if_present(&incoming).map_err(io_err("clear what a cut receive left"))?;
        files::private_dir(&incoming).map_err(io_err("make the incoming folder"))?;
        let tmp = Incoming(files::temp_beside(&incoming.join("bundle")));
        let (ack, result) = match read_offer(input, &tmp.0, limits) {
            Err(e) => (Ack { error: Some(as_refusal(&e)), ..Ack::default() }, Err(e)),
            Ok(offer) => match self.take(&offer, limits) {
                Ok(received) => (self.statement(offer.next(), None)?, Ok(received)),
                Err(Fault { error, signable: true }) => (self.statement(offer.next(), Some(as_refusal(&error)))?, Err(error)),
                Err(Fault { error, signable: false }) => (Ack { error: Some(as_refusal(&error)), ..Ack::default() }, Err(error)),
            },
        };
        let mut bytes = serde_json::to_vec(&ack).map_err(|e| io_err("write the ack")(e.into()))?;
        bytes.push(b'\n');
        output.write_all(&bytes).and_then(|()| output.flush()).map_err(io_err("send the ack"))?;
        result
    }

    fn statement(&self, next: &SignedSeat, refusal: Option<Refusal>) -> Result<Ack> {
        let key = self.key()?;
        let r = &next.record;
        let statement = AckStatement {
            target: r.target.clone(),
            generation: r.generation,
            bundle: r.bundle.clone(),
            seat: next.sha256(),
            receiver: key.public().clone(),
            taken: refusal.is_none(),
            refusal,
        };
        let text = serde_json::to_string(&statement).map_err(|e| io_err("write the ack")(e.into()))?;
        let signature = key.sign(ACK_NAMESPACE, text.as_bytes())?;
        Ok(Ack { statement: Some(text), signature: Some(signature), error: None })
    }

    fn take(&self, offer: &Offer, limits: Limits) -> Result<Received, Fault> {
        let next = offer.next();
        let name = next.record.target.clone();
        crate::holders::check_name("target", &name).map_err(signable)?;
        let _busy = self.lock(&name).map_err(unsigned)?;
        let me = self.me().map_err(unsigned)?.clone();
        let holders = self.holders().map_err(unsigned)?;
        let local = self.seats(&name, &holders).map_err(unsigned)?;
        let from = holders.name_of(next.record.previous.as_ref().unwrap_or(&next.record.holder));
        let received = |again| Received { target: name.clone(), generation: next.record.generation, from: from.clone(), again };
        if let Some(last) = local.last().filter(|l| l.record.generation >= next.record.generation) {
            if local.contains(next) {
                return Ok(received(true));
            }
            return Err(unsigned(Error::refused(
                Code::Ahead,
                format!("This machine has generation {} of {name}, past the offered {}, and not the offered seat.", last.record.generation, next.record.generation),
                "Do not abandon the move: find which machine holds the newest seat, and settle from there.",
            )));
        }
        if !holders.same_as(&offer.holders) {
            return Err(signable(Error::refused(
                Code::HoldersDiffer,
                "The sender's holder list differs from this machine's.",
                "Make the lists the same on both machines with `eidolon holders add`, then move again.",
            )));
        }
        may_hold(&holders, &me).map_err(|r| signable(r.into()))?;
        let have = self.target_if_any(&name).map_err(unsigned)?;
        if have.as_ref().is_some_and(|have| *have != offer.target) {
            return Err(signable(Error::refused(
                Code::TargetDiffers,
                format!("{name} is here with other rules than the sender's."),
                "Make the rules the same on both machines, then move again.",
            )));
        }
        if self.pending(&name, &holders).map_err(unsigned)?.is_some() {
            return Err(unsigned(Error::refused(
                Code::MovePending,
                format!("This machine has a move of {name} pending."),
                format!("Settle it here with `eidolon settle {name}` first."),
            )));
        }
        accept_chain(&holders, &name, local.last(), &offer.chain).map_err(|r| signable(r.into()))?;
        if next.record.holder != me {
            return Err(signable(Error::refused(
                Code::NotTheHolder,
                format!("That seat names {}, not this machine.", holders.name_of(&next.record.holder)),
                "Send it to the machine it names.",
            )));
        }
        if offer.sha256 != next.record.bundle {
            return Err(signable(Error::refused(Code::BundleChanged, "The bundle's bytes differ from the ones its seat names.", "Send it again.")));
        }
        let key = self.key().map_err(unsigned)?;
        bundle::open(&offer.bundle, key).and_then(|tar| archive::restore(tar, limits, &mut NullSink)).map_err(signable)?;
        self.keep(&name, offer, have.is_none(), local.last().map_or(0, |s| s.record.generation)).map_err(unsigned)?;
        Ok(received(false))
    }

    /// Keeps a checked offer: the rules if new, the bundle, then the seats.
    fn keep(&self, name: &str, offer: &Offer, new_target: bool, known: u64) -> Result<()> {
        if new_target {
            self.write_target(name, &offer.target)?;
        }
        let bundles = self.dir(name).join("bundles");
        files::private_dir(&bundles).map_err(io_err("make the bundles folder"))?;
        fs::rename(&offer.bundle, self.bundle_path(name, &offer.sha256)).map_err(io_err("keep the bundle"))?;
        for seat in offer.chain.iter().filter(|s| s.record.generation > known) {
            self.write_seat(name, seat)?;
        }
        self.prune_bundles(name, Some(&offer.sha256))
    }
}

fn read_offer(input: impl Read, bundle_path: &Path, limits: Limits) -> Result<Offer> {
    let bad = |why: String| Error::refused(Code::BadOffer, why, "Send the offer again; it must come from `eidolon send`.");
    let hit = Rc::new(Cell::new(None));
    let reading = |e: io::Error| archive::hit_error(hit.get(), &limits).unwrap_or_else(|| bad(format!("The offer does not read: {e}.")));
    let mut archive = tar::Archive::new(archive::guard(input, limits, &hit));
    let (mut target, mut holders, mut seats, mut sigs, mut sha) = (None, None, Vec::new(), std::collections::BTreeMap::new(), None);
    for entry in archive.entries().map_err(reading)? {
        let mut entry = entry.map_err(reading)?;
        let path = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
        let small = |entry: &mut tar::Entry<'_, _>| -> Result<String> {
            if entry.size() > SMALL {
                return Err(bad(format!("{path} is too large.")));
            }
            let mut text = String::new();
            entry.read_to_string(&mut text).map_err(reading)?;
            Ok(text)
        };
        if path == "target.toml" {
            target = Some(Target::parse(&small(&mut entry)?)?);
        } else if path == "holders" {
            holders = Some(Holders::parse(&small(&mut entry)?)?);
        } else if path == "bundle.age" {
            if entry.size() > limits.max_bytes {
                return Err(Error::refused(Code::TooLarge, "The bundle is larger than the limit.", "Raise --max-size if the target is really that big."));
            }
            let mut file = files::create_private(bundle_path).map_err(io_err("keep the incoming bundle"))?;
            let mut hash = Sha256::new();
            let mut buf = vec![0; 1 << 16];
            loop {
                let n = entry.read(&mut buf).map_err(reading)?;
                if n == 0 {
                    break;
                }
                hash.update(&buf[..n]);
                file.write_all(&buf[..n]).map_err(io_err("keep the incoming bundle"))?;
            }
            file.sync_all().map_err(io_err("keep the incoming bundle"))?;
            sha = Some(bundle::hex(&hash.finalize()));
        } else if let Some(g) = path.strip_prefix("seats/").and_then(|p| p.strip_suffix(".json.sig")).and_then(|g| g.parse::<u64>().ok()) {
            sigs.insert(g, small(&mut entry)?);
        } else if let Some(g) = path.strip_prefix("seats/").and_then(|p| p.strip_suffix(".json")).and_then(|g| g.parse::<u64>().ok()) {
            seats.push((g, small(&mut entry)?));
        } else {
            return Err(bad(format!("The offer has an entry {path:?} that no offer holds.")));
        }
    }
    seats.sort();
    let mut chain = Vec::with_capacity(seats.len());
    for (i, (g, bytes)) in seats.into_iter().enumerate() {
        let sig = sigs.remove(&g).ok_or_else(|| bad(format!("Seat {g} has no signature.")))?;
        let seat = SignedSeat::from_parts(bytes.into_bytes(), sig)?;
        if seat.record.generation != g || g != i as u64 + 1 {
            return Err(bad(format!("The seats are not generations 1 to {} in order.", chain.len() + 1)));
        }
        chain.push(seat);
    }
    let (Some(target), Some(holders), Some(sha256)) = (target, holders, sha) else {
        return Err(bad("The offer lacks its target, holders or bundle.".into()));
    };
    let next = chain.last().cloned().ok_or_else(|| bad("The offer has no seat.".into()))?;
    Ok(Offer { target, holders, chain, next, bundle: bundle_path.to_owned(), sha256 })
}

fn as_refusal(e: &Error) -> Refusal {
    match e {
        Error::Refused(r) => r.clone(),
        Error::Io { .. } => Refusal::new(Code::BadOffer, e.to_string(), "Fix the receiver, then send again."),
    }
}

fn no_pending(name: &str) -> Error {
    Error::refused(Code::NoPending, format!("No move of {name} is pending."), "There is nothing to settle.")
}

fn unsettled(name: &str, why: &str) -> Error {
    Error::refused(
        Code::BadAck,
        format!("{why} The move of {name} is still pending, and this machine keeps it closed."),
        format!("Send it again with the same `eidolon send`, or run `eidolon settle {name} --confirm <ack>`, or `--abandon` if the receiver surely took nothing."),
    )
}
