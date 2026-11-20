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
use crate::home::{hold, Home};
use crate::key::{HolderKey, ACK_NAMESPACE};
use crate::rules::{accept_chain, accept_seat, may_hold};
use crate::seat::{SeatRecord, SignedSeat};
use crate::swap;
use crate::target::Target;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

const SMALL: u64 = 1 << 20;

/// What a receiver signs: that it took the seat, or that it refused and kept nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AckStatement {
    pub target: String,
    pub generation: u64,
    pub bundle: String,
    pub receiver: HolderKey,
    pub taken: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<Refusal>,
}

/// The receiver's answer, as JSON. A signed statement, or an error the
/// receiver could not sign because the offer did not read.
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
    bundle: PathBuf,
    sha256: String,
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
    /// already holds is from a move that finished, and is cleared.
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
            files::remove_dir_if_present(&self.pending_dir(name)).map_err(io_err("clear a finished move"))?;
            return Ok(None);
        }
        Ok(Some(next))
    }

    /// Writes the next seat, naming `to`, as pending; or finds the one
    /// already pending for `to`, so a move cut off is sent again unchanged.
    pub fn start_move(&self, name: &str, to: &str) -> Result<SignedSeat> {
        let holders = self.holders()?;
        let receiver = holders
            .by_name(to)
            .ok_or_else(|| Error::refused(Code::UnknownHolder, format!("{to} is not on the holder list."), "Add it with `eidolon holders add` first."))?
            .key
            .clone();
        if receiver == *self.me() {
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
        let record = SeatRecord {
            target: name.to_owned(),
            generation: last.record.generation + 1,
            holder: receiver,
            previous: Some(last.record.holder.clone()),
            bundle: last.record.bundle.clone(),
            at: files::now(),
        };
        let next = SignedSeat::sign(record, self.key())?;
        accept_seat(&holders, name, Some(&last), &next)?;
        let dir = self.pending_dir(name);
        files::private_dir(&dir).map_err(io_err("make the pending folder"))?;
        write_private(&dir.join("next.json.sig"), next.signature().as_bytes()).map_err(io_err("write the pending move"))?;
        write_private(&dir.join("next.json"), next.bytes()).map_err(io_err("write the pending move"))?;
        Ok(next)
    }

    /// Writes the offer for the pending move of `name`.
    pub fn write_offer(&self, name: &str, out: impl Write) -> Result<()> {
        let holders = self.holders()?;
        let next = self.pending(name, &holders)?.ok_or_else(|| no_pending(name))?;
        let mut chain = self.seats(name, &holders)?;
        chain.push(next);
        let bundle = self.checked_bundle(name, &chain[chain.len() - 2])?;
        let mut tar = TarOut::new(out);
        let writing = || io_err("write the offer");
        let target = self.target(name)?.to_toml();
        tar.file("target.toml", 0o600, 0, target.len() as u64, &mut target.as_bytes()).map_err(writing())?;
        let holders_text = self.holders_text()?;
        tar.file("holders", 0o600, 0, holders_text.len() as u64, &mut holders_text.as_bytes()).map_err(writing())?;
        for seat in &chain {
            let g = seat.record.generation;
            tar.file(&format!("seats/{g}.json"), 0o600, 0, seat.bytes().len() as u64, &mut seat.bytes()).map_err(writing())?;
            tar.file(&format!("seats/{g}.json.sig"), 0o600, 0, seat.signature().len() as u64, &mut seat.signature().as_bytes()).map_err(writing())?;
        }
        let size = fs::metadata(&bundle).map_err(writing())?.len();
        let mut file = fs::File::open(&bundle).map_err(writing())?;
        tar.file("bundle.age", 0o600, 0, size, &mut file).map_err(writing())?;
        let (mut out, _) = tar.finish().map_err(writing())?;
        out.flush().map_err(writing())
    }

    /// Moves `name` to `to` over a duplex: writes the offer to `to_receiver`,
    /// closes it, reads the ack from `from_receiver` and settles.
    pub fn send(&self, name: &str, to: &str, clear: bool, to_receiver: impl Write, mut from_receiver: impl Read) -> Result<Moved> {
        self.start_move(name, to)?;
        let sent = self.write_offer(name, to_receiver);
        let mut ack = Vec::new();
        let read = from_receiver.by_ref().take(SMALL).read_to_end(&mut ack);
        if ack.is_empty() {
            sent?;
            read.map_err(io_err("read the ack"))?;
            return Err(unsettled(name, "The receiver sent no ack."));
        }
        match self.settle_with_ack(name, &ack, clear)? {
            Settled::Moved(moved) => Ok(moved),
            Settled::Refused(r) => Err(Error::Refused(Refusal::new(r.code, r.message, format!("{} The seat stays on this machine.", r.fix)))),
            Settled::Abandoned => Err(unsettled(name, "The move was abandoned.")),
        }
    }

    /// Settles the pending move with the receiver's ack.
    pub fn settle_with_ack(&self, name: &str, ack: &[u8], clear: bool) -> Result<Settled> {
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
        if statement.target != r.target || statement.generation != r.generation || statement.bundle != r.bundle || statement.receiver != r.holder {
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
        let cleared = if clear { self.clear(name)? } else { false };
        Ok(Settled::Moved(Moved { target: name.to_owned(), to: holders.name_of(&next.record.holder), generation: next.record.generation, cleared }))
    }

    /// Drops the pending move. Only safe when the receiver did not take it:
    /// otherwise two machines would each think the other holds the seat.
    pub fn abandon(&self, name: &str) -> Result<Settled> {
        let holders = self.holders()?;
        self.pending(name, &holders)?.ok_or_else(|| no_pending(name))?;
        files::remove_dir_if_present(&self.pending_dir(name)).map_err(io_err("clear the pending move"))?;
        Ok(Settled::Abandoned)
    }

    /// Removes this machine's live copy of a target it no longer holds.
    pub fn clear(&self, name: &str) -> Result<bool> {
        let target = self.target(name)?;
        let holders = self.holders()?;
        let chain = self.seats(name, &holders)?;
        if chain.last().is_some_and(|s| s.record.holder == *self.me()) {
            return Err(Error::refused(Code::NotTheHolder, format!("This machine still holds {name}."), "Move it first; only a copy that moved away is cleared."));
        }
        let Some(path) = self.path_of(name)? else { return Ok(false) };
        swap::recover(&path)?;
        let lock = hold(&path, &target)?;
        swap::remove(&path, lock)
    }

    /// Reads an offer, takes the seat if every check passes, and writes the
    /// ack. A refused offer gets a signed refusal when it read far enough to
    /// say which move it was.
    pub fn receive(&self, input: impl Read, mut output: impl Write, limits: Limits) -> Result<Received> {
        let incoming = self.root().join("incoming");
        files::private_dir(&incoming).map_err(io_err("make the incoming folder"))?;
        let tmp = Incoming(files::temp_beside(&incoming.join("bundle")));
        let (ack, result) = match read_offer(input, &tmp.0, limits) {
            Err(e) => (Ack { error: Some(as_refusal(&e)), ..Ack::default() }, Err(e)),
            Ok(offer) => {
                let result = self.take(&offer, limits);
                let next = &offer.chain[offer.chain.len() - 1].record;
                let statement = AckStatement {
                    target: next.target.clone(),
                    generation: next.generation,
                    bundle: next.bundle.clone(),
                    receiver: self.me().clone(),
                    taken: result.is_ok(),
                    refusal: result.as_ref().err().map(as_refusal),
                };
                let text = serde_json::to_string(&statement).map_err(|e| io_err("write the ack")(e.into()))?;
                let signature = self.key().sign(ACK_NAMESPACE, text.as_bytes())?;
                (Ack { statement: Some(text), signature: Some(signature), error: None }, result)
            }
        };
        let mut bytes = serde_json::to_vec(&ack).map_err(|e| io_err("write the ack")(e.into()))?;
        bytes.push(b'\n');
        output.write_all(&bytes).and_then(|()| output.flush()).map_err(io_err("send the ack"))?;
        result
    }

    fn take(&self, offer: &Offer, limits: Limits) -> Result<Received> {
        let next = &offer.chain[offer.chain.len() - 1];
        let name = next.record.target.clone();
        crate::holders::check_name("target", &name)?;
        let holders = self.holders()?;
        if !holders.same_as(&offer.holders) {
            return Err(Error::refused(
                Code::HoldersDiffer,
                "The sender's holder list differs from this machine's.",
                "Make the lists the same on both machines with `eidolon holders add`, then move again.",
            ));
        }
        may_hold(&holders, self.me())?;
        match self.target_if_any(&name)? {
            Some(have) if have != offer.target => {
                return Err(Error::refused(
                    Code::TargetDiffers,
                    format!("{name} is here with other rules than the sender's."),
                    "Make the rules the same on both machines, then move again.",
                ));
            }
            _ => {}
        }
        if self.pending(&name, &holders)?.is_some() {
            return Err(Error::refused(Code::MovePending, format!("This machine has a move of {name} pending."), format!("Settle it here with `eidolon settle {name}` first.")));
        }
        let local = self.seats(&name, &holders)?;
        accept_chain(&holders, &name, local.last(), &offer.chain)?;
        if next.record.holder != *self.me() {
            return Err(Error::refused(
                Code::NotTheHolder,
                format!("That seat names {}, not this machine.", holders.name_of(&next.record.holder)),
                "Send it to the machine it names.",
            ));
        }
        let from = holders.name_of(next.record.previous.as_ref().unwrap_or(&next.record.holder));
        let received = |again| Received { target: name.clone(), generation: next.record.generation, from: from.clone(), again };
        if local.last() == Some(next) {
            return Ok(received(true));
        }
        if offer.sha256 != next.record.bundle {
            return Err(Error::refused(Code::BundleChanged, "The bundle's bytes differ from the ones its seat names.", "Send it again."));
        }
        archive::restore(bundle::open(&offer.bundle, self.key())?, limits, &mut NullSink)?;
        if self.target_if_any(&name)?.is_none() {
            self.write_target(&name, &offer.target)?;
        }
        let bundles = self.dir(&name).join("bundles");
        files::private_dir(&bundles).map_err(io_err("make the bundles folder"))?;
        fs::rename(&offer.bundle, self.bundle_path(&name, &offer.sha256)).map_err(io_err("keep the bundle"))?;
        let known = local.last().map_or(0, |s| s.record.generation);
        for seat in offer.chain.iter().filter(|s| s.record.generation > known) {
            self.write_seat(&name, seat)?;
        }
        self.prune_bundles(&name, Some(&offer.sha256))?;
        Ok(received(false))
    }
}

fn read_offer(input: impl Read, bundle_path: &Path, limits: Limits) -> Result<Offer> {
    let bad = |why: String| Error::refused(Code::BadOffer, why, "Send the offer again; it must come from `eidolon send`.");
    let reading = |e: io::Error| bad(format!("The offer does not read: {e}."));
    let mut archive = tar::Archive::new(input);
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
    if chain.is_empty() {
        return Err(bad("The offer has no seat.".into()));
    }
    Ok(Offer { target, holders, chain, bundle: bundle_path.to_owned(), sha256 })
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
