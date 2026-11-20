//! The seat: which machine holds a target now. Each generation is a small
//! JSON record signed as SSHSIG by the holder before it, so the history is a
//! chain that `ssh-keygen -Y verify` can check link by link.

use crate::error::{Code, Error, Result};
use crate::key::{HolderKey, MachineKey, SEAT_NAMESPACE};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ssh_key::rand_core::{OsRng, RngCore};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeatRecord {
    pub target: String,
    pub generation: u64,
    pub holder: HolderKey,
    pub previous: Option<HolderKey>,
    /// The SHA-256 of the bundle, in hex.
    pub bundle: String,
    /// Seconds since 1970, as the signer's clock had it.
    pub at: u64,
    /// 16 random bytes in hex, so no two seats are the same bytes.
    pub nonce: String,
}

impl SeatRecord {
    pub fn new(target: &str, generation: u64, holder: HolderKey, previous: Option<HolderKey>, bundle: String) -> SeatRecord {
        let mut nonce = [0u8; 16];
        OsRng.fill_bytes(&mut nonce);
        SeatRecord { target: target.to_owned(), generation, holder, previous, bundle, at: crate::files::now(), nonce: crate::bundle::hex(&nonce) }
    }
}

/// A record with the exact bytes that were signed, and the signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedSeat {
    pub record: SeatRecord,
    bytes: Vec<u8>,
    signature: String,
}

impl SignedSeat {
    pub fn sign(record: SeatRecord, key: &MachineKey) -> Result<SignedSeat> {
        let mut bytes = serde_json::to_vec(&record).map_err(|e| Error::Io { doing: "write a seat".into(), source: e.into() })?;
        bytes.push(b'\n');
        let signature = key.sign(SEAT_NAMESPACE, &bytes)?;
        Ok(SignedSeat { record, bytes, signature })
    }

    pub fn from_parts(bytes: Vec<u8>, signature: String) -> Result<SignedSeat> {
        let record =
            serde_json::from_slice(&bytes).map_err(|e| Error::refused(Code::BadOffer, format!("A seat record does not read: {e}."), "Ask the sender for its seats again."))?;
        Ok(SignedSeat { record, bytes, signature })
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn signature(&self) -> &str {
        &self.signature
    }

    /// The SHA-256 of the signed bytes, in hex: what an ack names.
    pub fn sha256(&self) -> String {
        crate::bundle::hex(&Sha256::digest(&self.bytes))
    }

    pub fn signed_by(&self, key: &HolderKey) -> bool {
        key.verifies(SEAT_NAMESPACE, &self.bytes, &self.signature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(holder: &MachineKey) -> SeatRecord {
        SeatRecord::new("firefox", 1, holder.public().clone(), None, "00".repeat(32))
    }

    #[test]
    fn a_seat_checks_only_for_its_signer_and_its_own_bytes() {
        let (desk, tower) = (MachineKey::generate().unwrap(), MachineKey::generate().unwrap());
        let seat = SignedSeat::sign(record(&desk), &desk).unwrap();
        assert!(seat.signed_by(desk.public()));
        assert!(!seat.signed_by(tower.public()));
        let mut changed = seat.bytes().to_vec();
        changed.insert(0, b' ');
        let moved = SignedSeat::from_parts(changed, seat.signature().to_owned()).unwrap();
        assert!(!moved.signed_by(desk.public()), "other bytes, same record");
    }

    #[test]
    fn two_seats_signed_alike_are_still_different_bytes() {
        let desk = MachineKey::generate().unwrap();
        let (a, b) = (SignedSeat::sign(record(&desk), &desk).unwrap(), SignedSeat::sign(record(&desk), &desk).unwrap());
        assert_ne!(a.sha256(), b.sha256());
    }

    #[test]
    fn a_record_with_extra_fields_does_not_read() {
        let desk = MachineKey::generate().unwrap();
        let mut value = serde_json::to_value(record(&desk)).unwrap();
        value["extra"] = 1.into();
        assert!(SignedSeat::from_parts(serde_json::to_vec(&value).unwrap(), String::new()).is_err());
    }
}
