use crate::error::{Error, Result};
use crate::model::{EidolonId, FacetId, FacetKind, HolderKey};
use crate::vault::{ChunkName, Keys, Vault};
use serde::{Deserialize, Serialize};
use xenonet_id::codec::{b64, unb64};
use xenonet_id::sign::{Purpose, Signer};

pub const FORMAT: u32 = 1;

pub const MANIFEST_PIECE: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Node {
    Dir { mode: u32 },
    File { mode: u32, size: u64, chunks: Vec<ChunkName> },
    Symlink { target: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub path: String,
    pub node: Node,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FacetManifest {
    pub facet: FacetId,
    pub kind: FacetKind,
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    pub eidolon: EidolonId,
    pub generation: u64,
    pub facets: Vec<FacetManifest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Head {
    pub format: u32,
    pub eidolon: EidolonId,
    pub generation: u64,
    pub manifest: Vec<ChunkName>,
}

impl Head {
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let names: Vec<String> = self.manifest.iter().map(|n| format!("\"{}\"", n.to_hex())).collect();
        format!("{{\"format\":{},\"eidolon\":\"{}\",\"generation\":{},\"manifest\":[{}]}}", self.format, self.eidolon, self.generation, names.join(",")).into_bytes()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedHead {
    pub head: Head,
    pub signature: String,
}

impl SignedHead {
    pub fn verify(&self, key: &HolderKey) -> bool {
        unb64(&self.signature).is_some_and(|sig| key.verifying_key().verify(Purpose::EIDOLON_MANIFEST, &self.head.canonical_bytes(), &sig))
    }
}

pub fn seal(manifest: &Manifest, keys: &Keys, vault: &Vault, signer: &dyn Signer) -> Result<SignedHead> {
    let bytes = serde_json::to_vec(manifest).map_err(|e| Error::Damaged { what: "manifest", why: e.to_string() })?;
    let mut names = Vec::new();
    for piece in bytes.chunks(MANIFEST_PIECE) {
        names.push(crate::pack::folder::store_chunk(piece, keys, vault)?);
    }
    let head = Head { format: FORMAT, eidolon: manifest.eidolon, generation: manifest.generation, manifest: names };
    let sig = signer.sign(Purpose::EIDOLON_MANIFEST, &head.canonical_bytes()).map_err(|e| Error::KeyStore(e.message))?;
    Ok(SignedHead { head, signature: b64(&sig) })
}

/// Reads the manifest a head names. The caller checks the head's signature first.
pub fn load(head: &SignedHead, keys: &Keys, vault: &Vault) -> Result<Manifest> {
    if head.head.format != FORMAT {
        return Err(Error::Damaged { what: "manifest", why: format!("format {} is not known", head.head.format) });
    }
    let mut bytes = Vec::new();
    for name in &head.head.manifest {
        bytes.extend_from_slice(&keys.open(name, &vault.get(name)?)?);
    }
    let manifest: Manifest = serde_json::from_slice(&bytes).map_err(|e| Error::Damaged { what: "manifest", why: e.to_string() })?;
    if manifest.format != FORMAT || manifest.eidolon != head.head.eidolon || manifest.generation != head.head.generation {
        return Err(Error::Damaged { what: "manifest", why: "it does not match its head".to_owned() });
    }
    Ok(manifest)
}
