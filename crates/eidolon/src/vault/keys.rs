use crate::error::{Error, Result};
use ring::{aead, hkdf, hmac};
use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::io::{Read, Write};
use std::str::FromStr;
use xenonet_id::codec;
use zeroize::Zeroize;

pub const MAX_CHUNK_BYTES: usize = 1 << 20;
// Deflate can make a little more than the input of incompressible data, but
// the plaintext is what is capped: this is room for nothing but rounding.
const INFLATE_SLACK: usize = 4096;
const VERSION: u8 = 1;
const NAME_INFO: &[u8] = b"eidolon/name/1";
const CONTENT_INFO: &[u8] = b"eidolon/content/1";

#[derive(Clone, PartialEq, Eq)]
pub struct EidolonKey([u8; 32]);

impl Drop for EidolonKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl EidolonKey {
    pub fn generate() -> Result<EidolonKey> {
        let mut bytes = [0u8; 32];
        getrandom_fill(&mut bytes)?;
        Ok(EidolonKey(bytes))
    }

    pub fn from_bytes(bytes: [u8; 32]) -> EidolonKey {
        EidolonKey(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for EidolonKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[an eidolon key]")
    }
}

pub(crate) fn getrandom_fill(out: &mut [u8]) -> Result<()> {
    use ring::rand::SecureRandom as _;
    ring::rand::SystemRandom::new().fill(out).map_err(|_| Error::Crypto("the system gave no random numbers"))
}

pub(crate) struct Len(pub usize);

impl hkdf::KeyType for Len {
    fn len(&self) -> usize {
        self.0
    }
}

pub(crate) fn derive_with(key: &[u8], info: &[u8]) -> Result<[u8; 32]> {
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, &[]).extract(key);
    let mut out = [0u8; 32];
    let infos = [info];
    prk.expand(&infos, Len(32)).and_then(|okm| okm.fill(&mut out)).map_err(|_| Error::Crypto("key derivation failed"))?;
    Ok(out)
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChunkName(pub [u8; 32]);

impl ChunkName {
    pub fn to_hex(&self) -> String {
        codec::hex(&self.0)
    }
}

impl fmt::Display for ChunkName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for ChunkName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ChunkName({})", &self.to_hex()[..8])
    }
}

impl FromStr for ChunkName {
    type Err = Error;

    fn from_str(s: &str) -> Result<ChunkName> {
        if s.len() != 64 {
            return Err(Error::BadValue { what: "chunk name", rule: "64 lowercase hex characters" });
        }
        let mut out = [0u8; 32];
        for (byte, pair) in out.iter_mut().zip(s.as_bytes().chunks(2)) {
            *byte = (nibble(pair[0])? << 4) | nibble(pair[1])?;
        }
        Ok(ChunkName(out))
    }
}

// Lowercase only, so one name has one spelling.
fn nibble(c: u8) -> Result<u8> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        _ => Err(Error::BadValue { what: "chunk name", rule: "64 lowercase hex characters" }),
    }
}

impl Serialize for ChunkName {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for ChunkName {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<ChunkName, D::Error> {
        let text = String::deserialize(d)?;
        text.parse().map_err(de::Error::custom)
    }
}

pub struct Keys {
    name: hmac::Key,
    content: aead::LessSafeKey,
}

impl fmt::Debug for Keys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[an eidolon's chunk keys]")
    }
}

impl Keys {
    // ring's key types hold their own copy of the key and do not wipe it;
    // the bytes we derive them from are wiped here.
    pub fn new(key: &EidolonKey) -> Result<Keys> {
        let mut name = derive_with(&key.0, NAME_INFO)?;
        let mut content = derive_with(&key.0, CONTENT_INFO)?;
        let keys = aead::UnboundKey::new(&aead::CHACHA20_POLY1305, &content)
            .map(|k| Keys { name: hmac::Key::new(hmac::HMAC_SHA256, &name), content: aead::LessSafeKey::new(k) })
            .map_err(|_| Error::Crypto("the content key was refused"));
        name.zeroize();
        content.zeroize();
        keys
    }

    pub fn name_of(&self, plaintext: &[u8]) -> ChunkName {
        let tag = hmac::sign(&self.name, plaintext);
        let mut out = [0u8; 32];
        out.copy_from_slice(tag.as_ref());
        ChunkName(out)
    }

    /// A version byte, then the deflated plaintext under ChaCha20-Poly1305
    /// with the name as associated data.
    ///
    /// The nonce comes from the name, a keyed MAC of the whole plaintext
    /// (SIV style), so equal plaintexts seal to equal bytes and a chunk is
    /// stored once. No keystream ever covers two different messages: the
    /// nonces of two plaintexts meet only if their MACs agree on 96 bits.
    pub fn seal(&self, plaintext: &[u8]) -> (ChunkName, Vec<u8>) {
        let name = self.name_of(plaintext);
        let mut buf = deflate(plaintext);
        // Sealing in memory cannot fail; if ring says otherwise, an empty body never opens.
        if self.content.seal_in_place_append_tag(nonce_of(&name), aead::Aad::from(name.0), &mut buf).is_err() {
            buf.clear();
        }
        let mut body = Vec::with_capacity(buf.len() + 1);
        body.push(VERSION);
        body.extend_from_slice(&buf);
        (name, body)
    }

    pub fn open(&self, name: &ChunkName, sealed: &[u8]) -> Result<Vec<u8>> {
        let (&version, rest) = sealed.split_first().ok_or(Error::ChangedChunk)?;
        if version != VERSION || rest.len() < aead::CHACHA20_POLY1305.tag_len() {
            return Err(Error::ChangedChunk);
        }
        let mut buf = rest.to_vec();
        let deflated = self.content.open_in_place(nonce_of(name), aead::Aad::from(name.0), &mut buf).map_err(|_| Error::ChangedChunk)?;
        let plaintext = inflate(deflated)?;
        hmac::verify(&self.name, &plaintext, &name.0).map_err(|_| Error::ChangedChunk)?;
        Ok(plaintext)
    }
}

fn nonce_of(name: &ChunkName) -> aead::Nonce {
    let mut n = [0u8; 12];
    n.copy_from_slice(&name.0[..12]);
    aead::Nonce::assume_unique_for_key(n)
}

fn deflate(plaintext: &[u8]) -> Vec<u8> {
    let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    // Writing to a Vec cannot fail; an empty result would not open.
    if enc.write_all(plaintext).is_err() {
        return Vec::new();
    }
    enc.finish().unwrap_or_default()
}

fn inflate(deflated: &[u8]) -> Result<Vec<u8>> {
    let limit = (MAX_CHUNK_BYTES + INFLATE_SLACK) as u64;
    let mut out = Vec::new();
    flate2::read::DeflateDecoder::new(deflated).take(limit + 1).read_to_end(&mut out).map_err(|_| Error::ChangedChunk)?;
    if out.len() as u64 > limit {
        return Err(Error::ChunkTooLarge);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn keys() -> Keys {
        Keys::new(&EidolonKey::from_bytes([7; 32])).unwrap()
    }

    fn random_bytes(n: usize) -> Vec<u8> {
        let mut v = vec![0u8; n];
        getrandom_fill(&mut v).unwrap();
        v
    }

    #[test]
    fn empty_one_byte_and_a_mebibyte_of_random_round_trip() {
        let keys = keys();
        for plain in [Vec::new(), vec![42], random_bytes(MAX_CHUNK_BYTES)] {
            let (name, sealed) = keys.seal(&plain);
            assert_eq!(keys.open(&name, &sealed).unwrap(), plain);
        }
    }

    #[test]
    fn the_same_plaintext_seals_to_the_same_name_and_bytes() {
        let keys = keys();
        assert_eq!(keys.seal(b"hello hello hello"), keys.seal(b"hello hello hello"));
    }

    #[test]
    fn different_eidolon_keys_name_the_same_plaintext_differently() {
        let other = Keys::new(&EidolonKey::from_bytes([8; 32])).unwrap();
        let (a, sealed_a) = keys().seal(b"shared file");
        let (b, sealed_b) = other.seal(b"shared file");
        assert_ne!(a, b);
        assert_ne!(sealed_a, sealed_b);
    }

    #[test]
    fn one_flipped_bit_anywhere_is_a_changed_chunk() {
        let keys = keys();
        let (name, sealed) = keys.seal(&random_bytes(300));
        for i in 0..sealed.len() {
            for bit in [0u8, 7] {
                let mut bad = sealed.clone();
                bad[i] ^= 1 << bit;
                assert!(matches!(keys.open(&name, &bad), Err(Error::ChangedChunk)), "byte {i} bit {bit}");
            }
        }
    }

    #[test]
    fn a_chunk_stored_under_another_chunks_name_is_a_changed_chunk() {
        let keys = keys();
        let (_, sealed_a) = keys.seal(b"chunk a");
        let (name_b, _) = keys.seal(b"chunk b");
        assert!(matches!(keys.open(&name_b, &sealed_a), Err(Error::ChangedChunk)));
    }

    #[test]
    fn a_body_whose_plaintext_has_another_name_is_refused_even_when_it_opens() {
        // The tag is valid (same key, nonce and AAD) but the plaintext was
        // swapped before sealing: only the name check catches it.
        let keys = keys();
        let name = keys.name_of(b"what the name says");
        let mut buf = deflate(b"what was really sealed");
        keys.content.seal_in_place_append_tag(nonce_of(&name), aead::Aad::from(name.0), &mut buf).unwrap();
        let mut sealed = vec![VERSION];
        sealed.extend_from_slice(&buf);
        assert!(matches!(keys.open(&name, &sealed), Err(Error::ChangedChunk)));
    }

    #[test]
    fn a_chunk_that_inflates_past_the_cap_is_refused() {
        let keys = keys();
        // 64 MiB of zeros deflates to about 64 KiB.
        let (name, sealed) = keys.seal(&vec![0u8; 64 << 20]);
        assert!(sealed.len() < 200_000);
        assert!(matches!(keys.open(&name, &sealed), Err(Error::ChunkTooLarge)));
    }

    #[test]
    fn short_and_wrong_version_bodies_are_changed_chunks() {
        let keys = keys();
        let (name, mut sealed) = keys.seal(b"x");
        assert!(matches!(keys.open(&name, &[]), Err(Error::ChangedChunk)));
        assert!(matches!(keys.open(&name, &sealed[..10]), Err(Error::ChangedChunk)));
        sealed[0] = 2;
        assert!(matches!(keys.open(&name, &sealed), Err(Error::ChangedChunk)));
    }

    #[test]
    fn names_are_lowercase_hex_and_round_trip_through_text_and_json() {
        let (name, _) = keys().seal(b"abc");
        let text = name.to_string();
        assert_eq!(text.len(), 64);
        assert_eq!(text.parse::<ChunkName>().unwrap(), name);
        assert!(text.to_uppercase().parse::<ChunkName>().is_err());
        assert!("abc".parse::<ChunkName>().is_err());
        let json = serde_json::to_string(&name).unwrap();
        assert_eq!(json, format!("\"{text}\""));
        assert_eq!(serde_json::from_str::<ChunkName>(&json).unwrap(), name);
    }

    #[test]
    fn the_key_does_not_print() {
        assert_eq!(format!("{:?}", EidolonKey::from_bytes([1; 32])), "[an eidolon key]");
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn any_bytes_round_trip(plain in proptest::collection::vec(any::<u8>(), 0..4096)) {
            let keys = keys();
            let (name, sealed) = keys.seal(&plain);
            prop_assert_eq!(keys.open(&name, &sealed).unwrap(), plain);
        }

        #[test]
        fn any_single_byte_change_is_refused(
            plain in proptest::collection::vec(any::<u8>(), 0..512),
            at in any::<usize>(),
            delta in 1u8..=255,
        ) {
            let keys = keys();
            let (name, mut sealed) = keys.seal(&plain);
            let i = at % sealed.len();
            sealed[i] ^= delta;
            prop_assert!(keys.open(&name, &sealed).is_err());
        }
    }
}
