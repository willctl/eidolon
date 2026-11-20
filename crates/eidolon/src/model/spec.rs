use crate::error::{Error, Result};
use crate::model::{EidolonId, FacetId, Wall};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use xenonet_id::codec::{b64, unb64_n};
use xenonet_id::sign::VerifyingKey;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct HolderKey([u8; 32]);

impl HolderKey {
    pub fn from_bytes(bytes: [u8; 32]) -> HolderKey {
        HolderKey(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn verifying_key(&self) -> VerifyingKey {
        VerifyingKey::Ed25519(self.0)
    }

    pub fn from_verifying_key(key: &VerifyingKey) -> Option<HolderKey> {
        match key {
            VerifyingKey::Ed25519(k) => Some(HolderKey(*k)),
            VerifyingKey::P256(_) => None,
        }
    }

    pub fn to_b64(&self) -> String {
        b64(&self.0)
    }
}

impl fmt::Debug for HolderKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HolderKey({})", self.to_b64())
    }
}

impl Serialize for HolderKey {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_b64())
    }
}

impl<'de> Deserialize<'de> for HolderKey {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<HolderKey, D::Error> {
        let text = String::deserialize(d)?;
        key32(&text).map(HolderKey).ok_or_else(|| serde::de::Error::custom("a holder key is 32 bytes of base64url without padding"))
    }
}

fn key32(text: &str) -> Option<[u8; 32]> {
    unb64_n::<32>(text).filter(|k| b64(k) == text)
}

mod b64_32 {
    use super::{b64, key32, Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(k: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&b64(k))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let text = String::deserialize(d)?;
        key32(&text).ok_or_else(|| serde::de::Error::custom("a key is 32 bytes of base64url without padding"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Holder {
    pub name: String,
    pub sign: HolderKey,
    #[serde(with = "b64_32")]
    pub seal: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FacetKind {
    Folder,
    Browser,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FacetSpec {
    pub id: FacetId,
    pub kind: FacetKind,
    pub name: String,
    pub path: String,
    pub leave_out: Vec<String>,
}

impl FacetSpec {
    pub fn validate(&self) -> Result<()> {
        let name_ok = (1..=48).contains(&self.name.len()) && self.name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if !name_ok {
            return Err(Error::BadValue { what: "facet name", rule: "1 to 48 of a-z, 0-9 or -" });
        }
        let parts: Vec<&str> = self.path.split('/').filter(|p| !p.is_empty()).collect();
        if !self.path.starts_with('/') || parts.is_empty() || parts.contains(&"..") || self.path.contains('\0') {
            return Err(Error::BadValue { what: "facet path", rule: "an absolute path below the root, without `..`" });
        }
        for pattern in &self.leave_out {
            crate::rules::leave_out_is_inside(pattern)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EidolonSpec {
    pub id: EidolonId,
    pub wall: Wall,
    pub holders: Vec<Holder>,
    pub facets: Vec<FacetSpec>,
}

impl EidolonSpec {
    pub fn validate(&self) -> Result<()> {
        for (i, f) in self.facets.iter().enumerate() {
            f.validate()?;
            if self.facets[..i].iter().any(|g| g.name == f.name || g.id == f.id) {
                return Err(Error::BadValue { what: "facet list", rule: "free of repeated names and ids" });
            }
        }
        Ok(())
    }

    pub fn holder_by_key(&self, key: &HolderKey) -> Option<&Holder> {
        self.holders.iter().find(|h| h.sign == *key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facet(name: &str, path: &str) -> FacetSpec {
        FacetSpec { id: FacetId::from_bytes([2; 16]), kind: FacetKind::Folder, name: name.into(), path: path.into(), leave_out: vec![] }
    }

    #[test]
    fn facet_names_and_paths_are_checked() {
        assert!(facet("notes", "/home/agent/notes").validate().is_ok());
        for name in ["", "Notes", "a_b", &"a".repeat(49)] {
            assert!(facet(name, "/x").validate().is_err(), "{name:?}");
        }
        for path in ["relative", "/a/../b", "", "/", "//"] {
            assert!(facet("a", path).validate().is_err(), "{path:?}");
        }
    }

    #[test]
    fn two_facets_may_not_share_a_name() {
        let spec = EidolonSpec {
            id: EidolonId::from_bytes([1; 16]),
            wall: "wall_work".parse().unwrap(),
            holders: vec![],
            facets: vec![facet("a", "/a"), FacetSpec { id: FacetId::from_bytes([3; 16]), ..facet("a", "/b") }],
        };
        assert!(spec.validate().is_err());
    }

    #[test]
    fn keys_have_one_spelling() {
        let k = HolderKey::from_bytes([9; 32]);
        let json = serde_json::to_string(&k).unwrap_or_default();
        assert_eq!(serde_json::from_str::<HolderKey>(&json).ok(), Some(k));
        assert!(serde_json::from_str::<HolderKey>("\"AAAA\"").is_err());
        assert!(serde_json::from_str::<HolderKey>(&format!("\"{}=\"", k.to_b64())).is_err(), "padding");
        assert_eq!(HolderKey::from_verifying_key(&k.verifying_key()), Some(k));
    }
}
