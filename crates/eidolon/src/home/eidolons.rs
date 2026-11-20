use crate::cell::{CellName, Cells};
use crate::error::{Error, Result};
use crate::facet::{self, abort_at, firefox, open_facets, plan, seal_facets};
use crate::home::Home;
use crate::model::{EidolonId, EidolonSpec, FacetId, FacetKind, FacetSpec, Holder, RefusalCode, SignedSeat, State, Wall};
use crate::moving::{receive, seat_first, send, Holding, MoveStats};
use crate::pack::{load, seal, FacetManifest, Manifest, Node, SignedHead, FORMAT};
use crate::rules::{head_signed_by_holder, may_hold, may_open, same_wall};
use crate::vault::{ChunkName, EidolonKey, KeyRing, Keys, Vault};
use std::collections::BTreeSet;
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncWrite};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFacet {
    pub kind: FacetKind,
    pub name: String,
    pub path: String,
    pub leave_out: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Sealed {
    pub cell: CellName,
    pub files: usize,
    pub chunks: usize,
    pub elapsed: Duration,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Opened {
    pub provisioned: bool,
    /// An open that began here earlier and never finished was cleared from this cell.
    pub cleared: Option<String>,
    pub elapsed: Duration,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Held {
    pub eidolon: EidolonId,
    pub wall: Wall,
    pub state: State,
    pub generation: u64,
    pub seat_here: bool,
    pub pending_move: bool,
    pub facets: usize,
}

impl Home {
    pub fn holding(&self, eid: &EidolonId) -> Result<Holding> {
        if !Holding::exists(&self.holdings(), eid) {
            return Err(Error::refused(RefusalCode::UnknownEidolon, format!("This machine holds no eidolon {eid}."), "Run `eidolon ls` to see what it holds."));
        }
        Holding::load(&self.holdings(), eid)
    }

    pub fn create(&self, wall: Wall, holders: Vec<Holder>, facets: Vec<NewFacet>) -> Result<EidolonId> {
        let machine = self.machine()?;
        let mut specs = Vec::new();
        for f in facets {
            specs.push(FacetSpec { id: FacetId::new_random()?, kind: f.kind, name: f.name, path: f.path, leave_out: f.leave_out });
        }
        let spec = EidolonSpec { id: EidolonId::new_random()?, wall, holders, facets: specs };
        spec.validate()?;
        may_hold(&spec, &machine.key)?;
        let key = EidolonKey::generate()?;
        let keys = Keys::new(&key)?;
        let empty = Manifest {
            format: FORMAT,
            eidolon: spec.id,
            generation: 1,
            facets: spec.facets.iter().map(|f| FacetManifest { facet: f.id, kind: f.kind, entries: Vec::new() }).collect(),
        };
        let head = seal(&empty, &keys, &self.vault()?, &machine.signer)?;
        let chunks = chunks_of(&head, &empty);
        let seat = seat_first(&spec, &machine.signer)?;
        let id = spec.id;
        let store = self.store()?;
        let ring = KeyRing::new(&store);
        ring.put(&id.to_string(), &key)?;
        if let Err(e) = Holding::create(&self.holdings(), spec, seat, head_bytes(&head)?, chunks) {
            let _ = ring.delete(&id.to_string());
            return Err(e);
        }
        Ok(id)
    }

    pub async fn seal(&self, cells: &Cells, eid: &EidolonId, cell: Option<&CellName>) -> Result<Sealed> {
        let started = Instant::now();
        let machine = self.machine()?;
        let mut holding = self.holding(eid)?;
        let from = match (&holding.state, cell) {
            (State::Opening { cell: unfinished_in }, _) => return Err(unfinished(eid, unfinished_in)),
            (State::Open { cell: open }, Some(given)) if open != given.as_str() => {
                return Err(Error::refused(RefusalCode::SecondSeat, format!("That eidolon is open in {open}, not {given}."), format!("Seal it from {open}.")));
            }
            (State::Open { cell: open }, _) => cell_name(open)?,
            (_, Some(given)) => given.clone(),
            (_, None) => return Err(Error::refused(RefusalCode::NeedsCell, "This eidolon is sealed, so it needs a cell to take its files from.", "Add `--cell <cell>`.")),
        };
        let as_if = if matches!(holding.state, State::Open { .. }) { State::Sealed } else { holding.state.clone() };
        may_open(&as_if, &holding.seat, &machine.key, holding.pending.is_some())?;
        same_wall(&holding.spec.wall, &cells.wall(&from)?)?;
        let vault = self.vault()?;
        let keys = self.keys(eid)?;
        let ready = cells.ready(&from).await?;
        self.sweep_work();
        let work = self.run_dir()?;
        let sealed = seal_facets(&holding.spec.facets, &ready, &keys, &vault, &machine.signer, *eid, holding.seat.record.generation, &work).await;
        let _ = std::fs::remove_dir_all(&work);
        let head = sealed?;
        // Every chunk is in the vault before the holding names the head, and
        // the head is recorded before the state says sealed, so a kill
        // between any two leaves a holding that still opens.
        abort_at("after_chunks");
        let manifest = load(&head, &keys, &vault)?;
        let chunks = chunks_of(&head, &manifest);
        abort_at("before_head_recorded");
        holding.record_head(head_bytes(&head)?, chunks.clone())?;
        abort_at("after_head_recorded");
        holding.set_state(State::Sealed)?;
        abort_at("after_state_sealed");
        cells.release(&from, &eid.to_string())?;
        Ok(Sealed { cell: from, files: file_count(&manifest), chunks: chunks.len(), elapsed: started.elapsed() })
    }

    pub async fn open(&self, cells: &Cells, eid: &EidolonId, cell: &CellName) -> Result<Opened> {
        let started = Instant::now();
        let machine = self.machine()?;
        let mut holding = self.holding(eid)?;
        holding.may_open(&machine.key)?;
        same_wall(&holding.spec.wall, &cells.wall(cell)?)?;
        let vault = self.vault()?;
        let keys = self.keys(eid)?;
        let head = head_of(&holding)?;
        head_signed_by_holder(&holding.spec, &head)?;
        check_chunks(&head, &keys, &vault)?;
        let ready = cells.ready(cell).await?;
        let browser = holding.spec.facets.iter().any(|f| f.kind == FacetKind::Browser);
        let provisioned = browser && firefox::provision(&ready).await?;
        let cleared = match holding.state.clone() {
            State::Opening { cell: earlier } if earlier != cell.as_str() => {
                clear_unfinished(cells, &holding, &earlier).await?;
                Some(earlier)
            }
            _ => None,
        };
        // Recorded before the first file is placed, so the next command finds
        // a kill from here on and finishes or clears it.
        holding.set_state(State::Opening { cell: cell.to_string() })?;
        cells.hold(cell, &eid.to_string())?;
        abort_at("after_state_opening");
        if browser {
            // A browser left running by an open cut short would write over the profile.
            firefox::stop(&ready).await?;
        }
        self.sweep_work();
        let work = self.run_dir()?;
        let placed = open_facets(&head, &holding.spec.facets, &ready, &keys, &vault, &work).await;
        let _ = std::fs::remove_dir_all(&work);
        placed?;
        abort_at("before_state_open");
        if browser {
            plan::start_browser().run(&ready).await?;
        }
        holding.set_state(State::Open { cell: cell.to_string() })?;
        abort_at("after_state_open");
        Ok(Opened { provisioned, cleared, elapsed: started.elapsed() })
    }

    pub fn verify(&self, eid: &EidolonId) -> Result<usize> {
        let holding = self.holding(eid)?;
        let head = head_of(&holding)?;
        head_signed_by_holder(&holding.spec, &head)?;
        check_chunks(&head, &self.keys(eid)?, &self.vault()?)
    }

    pub async fn send<S>(&self, eid: &EidolonId, stream: S) -> Result<(MoveStats, u64)>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send,
    {
        let machine = self.machine()?;
        let mut holding = self.holding(eid)?;
        match &holding.state {
            State::Open { cell } => {
                return Err(Error::refused(RefusalCode::SecondSeat, format!("That eidolon is open in {cell}, so it cannot be moved."), format!("Run `eidolon seal {eid}` first.")));
            }
            State::Opening { cell } => return Err(unfinished(eid, cell)),
            State::Sealed | State::Retired => {}
        }
        let store = self.store()?;
        let stats = send(stream, &mut holding, &self.vault()?, &KeyRing::new(&store), &machine.signer).await?;
        Ok((stats, holding.seat.record.generation))
    }

    pub async fn receive<S>(&self, stream: S) -> Result<MoveStats>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send,
    {
        let machine = self.machine()?;
        let store = self.store()?;
        receive(stream, &self.holdings(), &self.vault()?, &KeyRing::new(&store), &machine.signer, &machine.key, &machine.seal_secret).await
    }

    pub fn abandon_move(&self, eid: &EidolonId) -> Result<()> {
        self.holding(eid)?.abandon_pending()
    }

    pub fn confirm_move(&self, eid: &EidolonId, taken: &SignedSeat) -> Result<()> {
        let store = self.store()?;
        self.holding(eid)?.confirm_moved(&KeyRing::new(&store), taken)
    }

    pub fn list(&self) -> Result<Vec<Held>> {
        let me = self.machine().ok().map(|m| m.key);
        let root = self.holdings();
        let entries = match std::fs::read_dir(&root) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(Error::Io { what: "read the holdings", source: e }),
        };
        let mut ids: Vec<EidolonId> = entries.filter_map(std::result::Result::ok).filter_map(|e| e.file_name().to_str().and_then(|n| n.parse().ok())).collect();
        ids.sort();
        let mut out = Vec::new();
        for id in ids.into_iter().filter(|id| Holding::exists(&root, id)) {
            let h = Holding::load(&root, &id)?;
            out.push(Held {
                eidolon: id,
                wall: h.spec.wall.clone(),
                generation: h.seat.record.generation,
                seat_here: me.is_some_and(|m| h.seat.record.holder == m),
                pending_move: h.pending.is_some(),
                facets: h.spec.facets.len(),
                state: h.state,
            });
        }
        Ok(out)
    }

    #[cfg(feature = "browser")]
    pub async fn browser_cell(&self, cells: &Cells, eid: &EidolonId) -> Result<(crate::cell::Cell, crate::view::ViewGuard)> {
        let holding = self.holding(eid)?;
        let State::Open { cell } = &holding.state else {
            return Err(Error::refused(RefusalCode::NotOpen, "That eidolon is not open in a cell.", format!("Run `eidolon open {eid} --cell <cell>` first.")));
        };
        if !holding.spec.facets.iter().any(|f| f.kind == FacetKind::Browser) {
            return Err(Error::refused(RefusalCode::NoBrowser, "That eidolon has no browser facet.", "Make one with `eidolon new ... --browser <name>`."));
        }
        let guard = crate::view::ViewGuard::new(self.holdings().join(eid.to_string()));
        guard.refuse_if_open()?;
        let name = cell_name(cell)?;
        same_wall(&holding.spec.wall, &cells.wall(&name)?)?;
        Ok((cells.ready(&name).await?, guard))
    }

    fn keys(&self, eid: &EidolonId) -> Result<Keys> {
        let store = self.store()?;
        let key = KeyRing::new(&store).get(&eid.to_string())?.ok_or_else(|| {
            Error::refused(RefusalCode::NoKey, "This machine has no key for that eidolon.", "It was moved away, or never lived here; ask the machine that holds it.")
        })?;
        Keys::new(&key)
    }
}

async fn clear_unfinished(cells: &Cells, holding: &Holding, cell: &str) -> Result<()> {
    let eid = holding.spec.id.to_string();
    let name = cell_name(cell)?;
    let ready = match cells.ready(&name).await {
        Ok(ready) => ready,
        Err(Error::Refused(r)) if matches!(r.code, RefusalCode::UnknownCell | RefusalCode::CellMissing) => {
            let _ = cells.release(&name, &eid);
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    facet::clear(&ready, &holding.spec.facets).await?;
    cells.release(&name, &eid)
}

fn cell_name(text: &str) -> Result<CellName> {
    text.parse().map_err(|_| Error::Damaged { what: "holding's state", why: format!("{text:?} is not a cell name") })
}

fn unfinished(eid: &EidolonId, cell: &str) -> Error {
    Error::refused(
        RefusalCode::OpenUnfinished,
        format!("An open of that eidolon began in {cell} and did not finish, so the files there may be partial."),
        format!("Run `eidolon open {eid} --cell {cell}` to finish it, or open it in another cell and {cell} is cleared first."),
    )
}

fn head_bytes(head: &SignedHead) -> Result<Vec<u8>> {
    serde_json::to_vec(head).map_err(|e| Error::Damaged { what: "head", why: e.to_string() })
}

fn head_of(holding: &Holding) -> Result<SignedHead> {
    serde_json::from_slice(&holding.head).map_err(|e| Error::Damaged { what: "head", why: e.to_string() })
}

fn chunks_of(head: &SignedHead, manifest: &Manifest) -> Vec<ChunkName> {
    let mut names: BTreeSet<ChunkName> = head.head.manifest.iter().copied().collect();
    for entry in manifest.facets.iter().flat_map(|f| &f.entries) {
        if let Node::File { chunks, .. } = &entry.node {
            names.extend(chunks.iter().copied());
        }
    }
    names.into_iter().collect()
}

/// Opens every chunk the head needs, read from the head itself and not from
/// the holding's list, which a kill between two writes can leave behind.
fn check_chunks(head: &SignedHead, keys: &Keys, vault: &Vault) -> Result<usize> {
    let names = chunks_of(head, &load(head, keys, vault)?);
    let bad = names.iter().filter(|name| vault.get(name).and_then(|sealed| keys.open(name, &sealed)).is_err()).count();
    if bad > 0 {
        return Err(Error::refused(
            RefusalCode::ChangedChunk,
            format!("{bad} of {} chunks did not open or did not match their names.", names.len()),
            "Do not open this copy; move the eidolon again from the machine that sealed it.",
        ));
    }
    Ok(names.len())
}

fn file_count(manifest: &Manifest) -> usize {
    manifest.facets.iter().flat_map(|f| &f.entries).filter(|e| matches!(e.node, Node::File { .. })).count()
}
