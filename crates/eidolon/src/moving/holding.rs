//! A holding's files are written whole and renamed into place, and the
//! seat goes last, so a holding without a seat never finished arriving.

use crate::error::{Error, Result};
use crate::model::{EidolonId, EidolonSpec, HolderKey, Refusal, RefusalCode, SeatRecord, SignedSeat, State};
use crate::rules::{accept_seat, may_hold, may_open};
use crate::vault::{ChunkName, KeyRing};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::path::{Path, PathBuf};
use xenonet_id::sign::Signer;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    pub to: HolderKey,
    pub generation: u64,
}

#[derive(Debug)]
pub struct Holding {
    dir: PathBuf,
    pub spec: EidolonSpec,
    pub seat: SignedSeat,
    pub head: Vec<u8>,
    pub chunks: Vec<ChunkName>,
    pub state: State,
    pub pending: Option<Pending>,
}

pub fn seat_first(spec: &EidolonSpec, signer: &dyn Signer) -> Result<SignedSeat> {
    let holder = HolderKey::from_verifying_key(&signer.public()).ok_or_else(|| Error::Protocol("the machine's key is not an Ed25519 key".into()))?;
    may_hold(spec, &holder)?;
    let record = SeatRecord { eidolon: spec.id, generation: 1, holder, previous: None, at: now() };
    SignedSeat::sign(record, signer)
}

pub(crate) fn now() -> String {
    xenonet_text::time::iso(xenonet_text::time::now_ms())
}

fn put(dir: &Path, name: &'static str, bytes: &[u8]) -> Result<()> {
    xenonet_os::private_file::write(&dir.join(name), bytes, &[]).map_err(Error::io("write a file of the holding"))
}

fn put_json<T: Serialize>(dir: &Path, name: &'static str, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| Error::Damaged { what: name, why: e.to_string() })?;
    put(dir, name, &bytes)
}

fn get(dir: &Path, name: &'static str) -> Result<Vec<u8>> {
    std::fs::read(dir.join(name)).map_err(|e| Error::Damaged { what: name, why: e.to_string() })
}

fn get_json<T: DeserializeOwned>(dir: &Path, name: &'static str) -> Result<T> {
    serde_json::from_slice(&get(dir, name)?).map_err(|e| Error::Damaged { what: name, why: e.to_string() })
}

impl Holding {
    pub fn dir_of(root: &Path, eidolon: &EidolonId) -> PathBuf {
        root.join(eidolon.to_string())
    }

    pub fn exists(root: &Path, eidolon: &EidolonId) -> bool {
        Self::dir_of(root, eidolon).join("seat.json").is_file()
    }

    pub fn create(root: &Path, spec: EidolonSpec, seat: SignedSeat, head: Vec<u8>, chunks: Vec<ChunkName>) -> Result<Holding> {
        if Self::exists(root, &spec.id) {
            return Err(Error::Protocol("this machine already holds that eidolon".into()));
        }
        accept_seat(&spec, None, &seat)?;
        Self::install(root, spec, seat, head, chunks)
    }

    pub(crate) fn install(root: &Path, spec: EidolonSpec, seat: SignedSeat, head: Vec<u8>, chunks: Vec<ChunkName>) -> Result<Holding> {
        let dir = Self::dir_of(root, &spec.id);
        crate::private::dir(&dir).map_err(Error::io("create the holding's folder"))?;
        let state = State::Sealed;
        put_json(&dir, "spec.json", &spec)?;
        put(&dir, "head", &head)?;
        put_json(&dir, "chunks.json", &chunks)?;
        put_json(&dir, "state.json", &state)?;
        if let Err(e) = std::fs::remove_file(dir.join("pending.json")) {
            if e.kind() != std::io::ErrorKind::NotFound {
                return Err(Error::Io { what: "remove an old pending move", source: e });
            }
        }
        put_json(&dir, "seat.json", &seat)?;
        Ok(Holding { dir, spec, seat, head, chunks, state, pending: None })
    }

    pub fn load(root: &Path, eidolon: &EidolonId) -> Result<Holding> {
        let dir = Self::dir_of(root, eidolon);
        let pending = match std::fs::read(dir.join("pending.json")) {
            Ok(bytes) => Some(serde_json::from_slice(&bytes).map_err(|e| Error::Damaged { what: "pending.json", why: e.to_string() })?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(Error::Damaged { what: "pending.json", why: e.to_string() }),
        };
        Ok(Holding {
            spec: get_json(&dir, "spec.json")?,
            seat: get_json(&dir, "seat.json")?,
            head: get(&dir, "head")?,
            chunks: get_json(&dir, "chunks.json")?,
            state: get_json(&dir, "state.json")?,
            pending,
            dir,
        })
    }

    pub fn may_open(&self, machine: &HolderKey) -> Result<(), Refusal> {
        may_open(&self.state, &self.seat, machine, self.pending.is_some())
    }

    pub fn record_head(&mut self, head: Vec<u8>, chunks: Vec<ChunkName>) -> Result<()> {
        put(&self.dir, "head", &head)?;
        put_json(&self.dir, "chunks.json", &chunks)?;
        self.head = head;
        self.chunks = chunks;
        Ok(())
    }

    pub fn set_state(&mut self, state: State) -> Result<()> {
        put_json(&self.dir, "state.json", &state)?;
        self.state = state;
        Ok(())
    }

    pub(crate) fn begin_move(&mut self, pending: Pending) -> Result<()> {
        put_json(&self.dir, "pending.json", &pending)?;
        self.pending = Some(pending);
        Ok(())
    }

    fn clear_pending(&mut self) -> Result<()> {
        match std::fs::remove_file(self.dir.join("pending.json")) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(Error::Io { what: "remove the pending move", source: e }),
        }
        self.pending = None;
        Ok(())
    }

    pub fn abandon_pending(&mut self) -> Result<()> {
        if self.pending.is_none() {
            return Err(no_pending());
        }
        self.clear_pending()
    }

    pub fn confirm_moved(&mut self, keyring: &KeyRing<'_>, taken: &SignedSeat) -> Result<()> {
        let pending = self.pending.ok_or_else(no_pending)?;
        if taken.record.generation != pending.generation || taken.record.holder != pending.to {
            return Err(Error::Refused(Refusal::new(
                RefusalCode::StaleGeneration,
                "That seat is not the one this machine handed over.",
                "Show the seat the other machine holds after the move.",
            )));
        }
        accept_seat(&self.spec, Some(&self.seat), taken)?;
        self.retire(keyring, taken)
    }

    pub(crate) fn retire(&mut self, keyring: &KeyRing<'_>, taken: &SignedSeat) -> Result<()> {
        put_json(&self.dir, "seat.json", taken)?;
        self.seat = taken.clone();
        self.set_state(State::Retired)?;
        keyring.delete(&self.spec.id.to_string())?;
        self.clear_pending()
    }
}

fn no_pending() -> Error {
    Error::refused(RefusalCode::NoPending, "No move is waiting to be settled here.", "Nothing to do.")
}
