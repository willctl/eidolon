//! An eidolon's facets moved between a cell and the vault. A facet lives
//! in a cell, but packing works on a host folder, so each facet's files
//! pass through a staging folder on the host.

pub mod firefox;
pub mod plan;

use crate::cell::Cell;
use crate::error::{Error, Result};
use crate::model::{EidolonId, FacetKind, FacetSpec};
use crate::pack::{load, pack_folder, seal, unpack_facet, Manifest, SignedHead, FORMAT};
use crate::vault::{Keys, Vault};
use std::path::{Path, PathBuf};
use xenonet_id::codec::{hex, random};
use xenonet_id::sign::Signer;

pub fn leave_out(spec: &FacetSpec) -> Vec<String> {
    let mut all: Vec<String> = match spec.kind {
        FacetKind::Folder => Vec::new(),
        FacetKind::Browser => firefox::PROFILE_LEAVE_OUT.iter().map(|p| (*p).to_owned()).collect(),
    };
    for pattern in &spec.leave_out {
        if !all.contains(pattern) {
            all.push(pattern.clone());
        }
    }
    all
}

/// Copies one facet's files out of the cell into `staging`. A browser is
/// stopped first and left stopped: a sealed eidolon is open nowhere.
async fn take(cell: &Cell, spec: &FacetSpec, staging: &Path) -> Result<()> {
    match spec.kind {
        FacetKind::Folder => cell.copy_out(&spec.path, staging).await,
        FacetKind::Browser => {
            firefox::quiet(cell).await?;
            firefox::take(cell, &spec.path, staging).await
        }
    }
}

async fn place(cell: &Cell, spec: &FacetSpec, staging: &Path) -> Result<()> {
    match spec.kind {
        FacetKind::Folder => cell.copy_in(staging, &spec.path, None).await,
        FacetKind::Browser => firefox::place(cell, &spec.path, staging).await,
    }
}

#[expect(clippy::too_many_arguments, reason = "each is a distinct thing a seal needs; a struct would only rename them")]
pub async fn seal_facets(
    facets: &[FacetSpec],
    cell: &Cell,
    keys: &Keys,
    vault: &Vault,
    signer: &dyn Signer,
    eidolon: EidolonId,
    generation: u64,
    work: &Path,
) -> Result<SignedHead> {
    let mut parts = Vec::new();
    for spec in facets {
        let staging = fresh_dir(work, "take", &spec.name)?;
        let taken = take(cell, spec, &staging).await;
        if taken.is_ok() {
            abort_at("after_take");
        }
        let packed = taken.and_then(|()| pack_folder(spec.id, spec.kind, &staging, &leave_out(spec), keys, vault));
        // The plain files go; the chunks are in the vault by now.
        let _ = std::fs::remove_dir_all(&staging);
        parts.push(packed?.manifest);
    }
    seal(&Manifest { format: FORMAT, eidolon, generation, facets: parts }, keys, vault, signer)
}

/// Unpacks every facet the head names into staging folders, and only then
/// places them in the cell. The caller checks the head's signature first.
pub async fn open_facets(head: &SignedHead, facets: &[FacetSpec], cell: &Cell, keys: &Keys, vault: &Vault, work: &Path) -> Result<()> {
    let manifest = load(head, keys, vault)?;
    let mut staged = Vec::new();
    let unpacked = facets.iter().try_for_each(|spec| {
        let part = manifest.facets.iter().find(|f| f.facet == spec.id && f.kind == spec.kind);
        let part = part.ok_or_else(|| Error::Damaged { what: "manifest", why: format!("it has nothing for facet {}", spec.name) })?;
        let into = fresh_dir(work, "place", &spec.name)?.join("files");
        staged.push((spec, into.clone()));
        unpack_facet(part, keys, vault, &into)
    });
    let placed = async {
        unpacked?;
        abort_at("after_unpack");
        for (spec, into) in &staged {
            place(cell, spec, into).await?;
        }
        Ok(())
    }
    .await;
    for (_, into) in &staged {
        if let Some(parent) = into.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }
    placed
}

pub async fn clear(cell: &Cell, facets: &[FacetSpec]) -> Result<()> {
    if facets.iter().any(|f| f.kind == FacetKind::Browser) {
        firefox::stop(cell).await?;
    }
    for spec in facets {
        // Root, because a browser's profile belongs to the browser's user.
        cell.must_root(&format!("clearing {}", spec.path), &["rm", "-rf", "--", &spec.path]).await?;
    }
    Ok(())
}

fn fresh_dir(work: &Path, what: &str, name: &str) -> Result<PathBuf> {
    let tag = hex(&random::<6>().map_err(|_| Error::random())?);
    let dir = work.join(format!("{what}-{name}-{tag}"));
    std::fs::create_dir_all(&dir).map_err(Error::io("make a staging folder"))?;
    Ok(dir)
}

/// The kill tests' abort points: in a `testing` build, the process dies at
/// once, as `kill -9` would, when `EIDOLON_TEST_ABORT_AT` names `step`.
#[cfg(feature = "testing")]
pub(crate) fn abort_at(step: &str) {
    if std::env::var_os("EIDOLON_TEST_ABORT_AT").is_some_and(|v| v == step) {
        std::process::abort();
    }
}

#[cfg(not(feature = "testing"))]
pub(crate) fn abort_at(_step: &str) {}
