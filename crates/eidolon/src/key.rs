//! Each machine has one Ed25519 OpenSSH key. It opens bundles as an age
//! `ssh-ed25519` recipient and signs seats and acks as SSHSIG.

use crate::error::{io_err, Code, Error, Result};
use crate::files;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use ssh_key::rand_core::OsRng;
use ssh_key::{Algorithm, HashAlg, LineEnding, PrivateKey, PublicKey, SshSig};
use std::fmt;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use zeroize::Zeroizing;

/// The SSHSIG namespace of a seat. `ssh-keygen -Y verify -n` takes it.
pub const SEAT_NAMESPACE: &str = "eidolon-seat@willctl.com";
/// The SSHSIG namespace of a receiver's ack.
pub const ACK_NAMESPACE: &str = "eidolon-ack@willctl.com";

/// The key store entry the machine key lives under by default.
pub const KEYRING_SERVICE: &str = "eidolon";
pub const KEYRING_USER: &str = "machine-key";

/// Where the machine key is kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySource {
    /// The operating system's key store, through the keyring crate.
    Keyring,
    /// An OpenSSH private key file that only its owner can read.
    File(PathBuf),
}

/// An Ed25519 public key, compared and written without its comment.
#[derive(Clone, PartialEq, Eq)]
pub struct HolderKey(PublicKey);

impl HolderKey {
    pub fn new(key: &PublicKey) -> Result<HolderKey> {
        if key.algorithm() != Algorithm::Ed25519 {
            return Err(Error::refused(Code::BadHolder, format!("That key is {}, not ssh-ed25519.", key.algorithm()), "Use the line `eidolon key show` prints."));
        }
        Ok(HolderKey(PublicKey::new(key.key_data().clone(), "")))
    }

    pub fn public(&self) -> &PublicKey {
        &self.0
    }

    /// The `ssh-ed25519 AAAA...` line.
    pub fn line(&self) -> String {
        self.0.to_openssh().unwrap_or_default()
    }

    pub fn recipient(&self) -> Result<age::ssh::Recipient> {
        age::ssh::Recipient::from_str(&self.line()).map_err(|_| Error::refused(Code::BadHolder, "That key does not work as an age recipient.", "Use an ssh-ed25519 key."))
    }

    /// Whether `armored` is an SSHSIG by this key over `msg` in `namespace`.
    pub fn verifies(&self, namespace: &str, msg: &[u8], armored: &str) -> bool {
        SshSig::from_pem(armored).is_ok_and(|sig| self.0.verify(namespace, msg, &sig).is_ok())
    }
}

impl FromStr for HolderKey {
    type Err = Error;
    fn from_str(s: &str) -> Result<HolderKey> {
        let key =
            PublicKey::from_openssh(s.trim()).map_err(|_| Error::refused(Code::BadHolder, "That is not an OpenSSH public key.", "Use the line `eidolon key show` prints."))?;
        HolderKey::new(&key)
    }
}

impl fmt::Display for HolderKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.line())
    }
}

impl fmt::Debug for HolderKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HolderKey({})", self.0.fingerprint(HashAlg::Sha256))
    }
}

impl Serialize for HolderKey {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.line())
    }
}

impl<'de> Deserialize<'de> for HolderKey {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<HolderKey, D::Error> {
        String::deserialize(d)?.parse().map_err(serde::de::Error::custom)
    }
}

/// This machine's private key.
pub struct MachineKey {
    private: PrivateKey,
    public: HolderKey,
}

impl MachineKey {
    /// Loads the key if this machine has one, writing nothing.
    pub fn load(source: &KeySource) -> Result<Option<MachineKey>> {
        let pem = match source {
            KeySource::File(path) => read_file_key(path)?,
            KeySource::Keyring => read_keyring_key()?,
        };
        pem.map(|pem| MachineKey::from_openssh(&pem)).transpose()
    }

    /// Loads the key, or makes it. True when it was made now.
    pub fn create(source: &KeySource) -> Result<(MachineKey, bool)> {
        if let Some(key) = MachineKey::load(source)? {
            return Ok((key, false));
        }
        let key = MachineKey::generate()?;
        match source {
            KeySource::File(path) => write_file_key(path, &key.to_openssh())?,
            KeySource::Keyring => keyring_entry()?.set_password(&key.to_openssh()).map_err(no_store)?,
        }
        Ok((key, true))
    }

    pub fn generate() -> Result<MachineKey> {
        let private = PrivateKey::random(&mut OsRng, Algorithm::Ed25519).map_err(|e| Error::Io { doing: "make a key".into(), source: std::io::Error::other(e.to_string()) })?;
        MachineKey::from_private(private)
    }

    #[cfg(test)]
    pub(crate) fn from_seed(seed: [u8; 32]) -> MachineKey {
        let pair = ssh_key::private::Ed25519Keypair::from_seed(&seed);
        MachineKey::from_private(PrivateKey::from(pair)).unwrap()
    }

    pub fn from_openssh(pem: &str) -> Result<MachineKey> {
        let private = PrivateKey::from_openssh(pem).map_err(|_| {
            Error::refused(Code::BadHolder, "The machine key is not an unencrypted OpenSSH private key.", "Point --key-file at a key `ssh-keygen -t ed25519 -N ''` made.")
        })?;
        MachineKey::from_private(private)
    }

    fn from_private(private: PrivateKey) -> Result<MachineKey> {
        let public = HolderKey::new(private.public_key())?;
        Ok(MachineKey { private, public })
    }

    pub fn to_openssh(&self) -> Zeroizing<String> {
        self.private.to_openssh(LineEnding::LF).unwrap_or_default()
    }

    pub fn public(&self) -> &HolderKey {
        &self.public
    }

    /// An armored SSHSIG over `msg`, with SHA-512 as `ssh-keygen` uses.
    pub fn sign(&self, namespace: &str, msg: &[u8]) -> Result<String> {
        let failed = |e: ssh_key::Error| Error::Io { doing: "sign".into(), source: std::io::Error::other(e.to_string()) };
        self.private.sign(namespace, HashAlg::Sha512, msg).and_then(|sig| sig.to_pem(LineEnding::LF)).map_err(failed)
    }

    pub fn identity(&self) -> Result<age::ssh::Identity> {
        age::ssh::Identity::from_buffer(self.to_openssh().as_bytes(), None).map_err(io_err("read the machine key as an age identity"))
    }
}

fn read_file_key(path: &Path) -> Result<Option<Zeroizing<String>>> {
    match std::fs::read_to_string(path) {
        Ok(pem) => {
            let pem = Zeroizing::new(pem);
            only_owner_reads(path)?;
            Ok(Some(pem))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_err(format!("read the key file {}", path.display()))(e)),
    }
}

/// Writes a new key file, owner-only, and syncs it and its folder.
fn write_file_key(path: &Path, pem: &str) -> Result<()> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    files::private_dir(parent).map_err(io_err("make the key file's folder"))?;
    let mut f = files::create_private(path).map_err(io_err(format!("create the key file {}", path.display())))?;
    f.write_all(pem.as_bytes()).and_then(|()| f.sync_all()).map_err(io_err("write the key file"))?;
    files::sync_dir(parent).map_err(io_err("sync the key file's folder"))
}

#[cfg(unix)]
fn only_owner_reads(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path).map_err(io_err("read the key file's mode"))?.permissions().mode();
    if mode & 0o077 != 0 {
        return Err(Error::refused(Code::KeyNotPrivate, format!("The key file {} can be read by others (mode {:o}).", path.display(), mode & 0o777), "Run `chmod 600` on it."));
    }
    Ok(())
}

#[cfg(not(unix))]
#[expect(clippy::unnecessary_wraps, reason = "the same signature as the Unix version")]
fn only_owner_reads(_: &Path) -> Result<()> {
    Ok(())
}

fn no_store(e: keyring::Error) -> Error {
    Error::refused(Code::NoKeyStore, format!("The key store did not answer: {e}."), "Pass --key-file or set EIDOLON_KEY_FILE on a machine without one.")
}

fn keyring_entry() -> Result<keyring::Entry> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER).map_err(no_store)
}

fn read_keyring_key() -> Result<Option<Zeroizing<String>>> {
    match keyring_entry()?.get_password() {
        Ok(pem) => Ok(Some(Zeroizing::new(pem))),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(no_store(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signature_checks_only_for_its_key_namespace_and_bytes() {
        let (desk, tower) = (MachineKey::generate().unwrap(), MachineKey::generate().unwrap());
        let sig = desk.sign(SEAT_NAMESPACE, b"seat").unwrap();
        assert!(desk.public().verifies(SEAT_NAMESPACE, b"seat", &sig));
        assert!(!tower.public().verifies(SEAT_NAMESPACE, b"seat", &sig));
        assert!(!desk.public().verifies(ACK_NAMESPACE, b"seat", &sig));
        assert!(!desk.public().verifies(SEAT_NAMESPACE, b"seat2", &sig));
        assert!(!desk.public().verifies(SEAT_NAMESPACE, b"seat", "junk"));
    }

    #[test]
    fn a_key_file_is_made_once_and_must_be_private() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys/desk");
        assert!(MachineKey::load(&KeySource::File(path.clone())).unwrap().is_none(), "no key yet, and none made");
        assert!(!path.exists());
        let (first, made) = MachineKey::create(&KeySource::File(path.clone())).unwrap();
        let (again, made_again) = MachineKey::create(&KeySource::File(path.clone())).unwrap();
        assert!(made && !made_again);
        assert_eq!(first.public(), again.public());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            let err = MachineKey::load(&KeySource::File(path)).err().unwrap();
            assert_eq!(err.code(), Some(Code::KeyNotPrivate));
        }
    }

    #[test]
    fn public_keys_lose_their_comment_and_only_ed25519_passes() {
        let key = MachineKey::generate().unwrap();
        let with_comment = format!("{} desk@somewhere", key.public().line());
        assert_eq!(&with_comment.parse::<HolderKey>().unwrap(), key.public());
        assert!(!key.public().line().contains('@'));
        let rsa = "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAAAgQDEeAnXDhTqBUmJ1u1cfK0bVlmHh2d0oWUc5ZD1aq0sQb2lmnn2Ut3Eu3pOrQpNStn1aFJqBkDNsdmXxcLRmRqY2C4B7W9VQ8Z4Jg5fZf8KQvwYsz3m+5/WpUZ0d6BfCv6pXx0p1QTp8D2bDH5eXyP0gc9E8OQ6n2E3nqLdM6rz8w==";
        assert!(rsa.parse::<HolderKey>().is_err());
    }
}
