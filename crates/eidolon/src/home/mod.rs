//! A machine's eidolon home (`$EIDOLON_HOME`, else `~/.eidolon`): its
//! identity and keys, its vault, and the eidolons it holds.

mod eidolons;
mod store;

pub use eidolons::{Held, NewFacet, Opened, Sealed};
pub use store::FileKeyStore;

use crate::error::{Error, Result};
use crate::model::{EidolonId, Holder, HolderKey, RefusalCode};
use crate::vault::Vault;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use x25519_dalek::{PublicKey, StaticSecret};
use xenonet_id::codec::random;
use xenonet_id::sign::Signer;
use xenonet_id::store::{KeyStore, StoredSigner};
use zeroize::Zeroizing;

const SIGN_KEY: &str = "machine-sign";
const SEAL_KEY: &str = "machine-seal";

pub const FILE_STORE_WARNING: &str = "The keys are kept in owner-only files under the keys folder of the eidolon home. That is weaker than the operating system's key store, which this build does not use yet: anyone who can read your files as you can read the keys.";

#[derive(Debug, Serialize, Deserialize)]
struct MachineFile {
    name: String,
    key_store: String,
}

pub struct Machine {
    pub signer: StoredSigner,
    pub key: HolderKey,
    pub seal_secret: StaticSecret,
    pub holder: Holder,
}

#[derive(Debug, Clone)]
pub struct Home {
    dir: PathBuf,
}

impl Home {
    pub fn at(dir: impl Into<PathBuf>) -> Home {
        Home { dir: dir.into() }
    }

    pub fn from_env() -> Result<Home> {
        if let Some(dir) = std::env::var_os("EIDOLON_HOME").filter(|d| !d.is_empty()) {
            return Ok(Home::at(dir));
        }
        let home = std::env::var_os("HOME").filter(|h| !h.is_empty()).ok_or(Error::BadValue { what: "home folder", rule: "set: give EIDOLON_HOME a folder" })?;
        Ok(Home::at(PathBuf::from(home).join(".eidolon")))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn holdings(&self) -> PathBuf {
        self.dir.join("holdings")
    }

    fn vault(&self) -> Result<Vault> {
        Vault::open(&self.dir.join("vault"))
    }

    fn store(&self) -> Result<FileKeyStore> {
        FileKeyStore::open(&self.dir.join("keys")).map_err(|e| Error::KeyStore(e.to_string()))
    }

    fn machine_file(&self) -> PathBuf {
        self.dir.join("machine.json")
    }

    /// Makes this machine's keys and name. Refused when it has them: a second
    /// identity would orphan every seat that names the first.
    pub fn init(&self, name: &str) -> Result<Machine> {
        if self.machine_file().exists() {
            return Err(Error::refused(RefusalCode::AlreadyInitialised, "This machine already has an identity.", "Use `eidolon machine show` to read it."));
        }
        if !((1..=48).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')) {
            return Err(Error::BadValue { what: "machine name", rule: "1 to 48 of a-z, 0-9 or -, such as `desk`" });
        }
        let store = self.store()?;
        let failed = |e: xenonet_id::IdError| Error::KeyStore(e.to_string());
        StoredSigner::load_or_create(&store, SIGN_KEY).map_err(failed)?;
        let seal = Zeroizing::new(random::<32>().map_err(failed)?);
        store.put(SEAL_KEY, &*seal).map_err(failed)?;
        let file = MachineFile { name: name.to_owned(), key_store: "file".to_owned() };
        let bytes = serde_json::to_vec_pretty(&file).map_err(|e| Error::Damaged { what: "machine file", why: e.to_string() })?;
        xenonet_os::private_file::write(&self.machine_file(), &bytes, &[]).map_err(Error::io("write the machine's name"))?;
        self.machine()
    }

    pub fn machine(&self) -> Result<Machine> {
        let bytes = match std::fs::read(self.machine_file()) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::refused(RefusalCode::NotInitialised, "This machine has no identity yet.", "Run `eidolon machine init --name <name>` first."));
            }
            Err(e) => return Err(Error::Io { what: "read the machine's name", source: e }),
        };
        let file: MachineFile = serde_json::from_slice(&bytes).map_err(|e| Error::Damaged { what: "machine file", why: e.to_string() })?;
        let store = self.store()?;
        let damaged = || Error::Damaged { what: "machine's keys", why: "missing or not keys".to_owned() };
        let signer = StoredSigner::load(&store, SIGN_KEY).map_err(|_| damaged())?.ok_or_else(damaged)?;
        let seal: [u8; 32] = store.get(SEAL_KEY).map_err(|_| damaged())?.and_then(|b| b.try_into().ok()).ok_or_else(damaged)?;
        let seal_secret = StaticSecret::from(seal);
        let key = HolderKey::from_verifying_key(&signer.public()).ok_or_else(damaged)?;
        let holder = Holder { name: file.name, sign: key, seal: PublicKey::from(&seal_secret).to_bytes() };
        Ok(Machine { signer, key, seal_secret, holder })
    }

    pub fn files_dir(&self, eid: &EidolonId) -> Result<PathBuf> {
        let dir = self.dir.join("files").join(eid.to_string());
        crate::private::dir(&dir).map_err(Error::io("make the files folder"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(Error::io("make the files folder private"))?;
        }
        Ok(dir)
    }

    fn work(&self) -> PathBuf {
        self.dir.join("tmp")
    }

    fn run_dir(&self) -> Result<PathBuf> {
        let tag = xenonet_id::codec::hex(&random::<6>().map_err(|_| Error::random())?);
        let dir = self.work().join(format!("run-{}-{tag}", std::process::id()));
        crate::private::dir(&dir).map_err(Error::io("make the working folder"))?;
        Ok(dir)
    }

    fn sweep_work(&self) {
        let Ok(entries) = std::fs::read_dir(self.work()) else { return };
        for entry in entries.filter_map(std::result::Result::ok) {
            let name = entry.file_name().to_string_lossy().into_owned();
            let pid = name.strip_prefix("run-").and_then(|r| r.split('-').next()).and_then(|p| p.parse::<u32>().ok());
            if pid.is_some_and(|p| p != std::process::id() && !xenonet_os::process::running(p, true)) {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
}
