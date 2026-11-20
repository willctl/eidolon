//! The vault holds sealed bytes and never needs a key. A chunk is written to
//! a hidden temp file beside its place, flushed, and renamed, so a kill
//! mid-write leaves no half chunk under a real name.

use crate::error::{Error, Result};
use crate::vault::keys::ChunkName;
use std::path::{Path, PathBuf};

pub struct Vault {
    chunks: PathBuf,
}

impl Vault {
    pub fn open(dir: &Path) -> Result<Vault> {
        let chunks = dir.join("chunks");
        crate::private::dir(&chunks).map_err(Error::io("create the vault"))?;
        Ok(Vault { chunks })
    }

    fn shard(&self, name: &ChunkName) -> (PathBuf, PathBuf) {
        let hex = name.to_hex();
        let dir = self.chunks.join(&hex[..2]);
        let file = dir.join(&hex[2..]);
        (dir, file)
    }

    pub fn has(&self, name: &ChunkName) -> bool {
        self.shard(name).1.is_file()
    }

    pub fn put(&self, name: &ChunkName, sealed: &[u8]) -> Result<()> {
        let (dir, file) = self.shard(name);
        crate::private::dir(&dir).map_err(Error::io("create the vault"))?;
        xenonet_os::private_file::write(&file, sealed, &[]).map_err(Error::io("write a chunk"))
    }

    pub fn get(&self, name: &ChunkName) -> Result<Vec<u8>> {
        std::fs::read(self.shard(name).1).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => Error::MissingChunk(*name),
            _ => Error::Io { what: "read a chunk", source: e },
        })
    }

    pub fn list(&self) -> Result<Vec<ChunkName>> {
        let mut names = Vec::new();
        for shard in std::fs::read_dir(&self.chunks).map_err(Error::io("read the vault"))? {
            let shard = shard.map_err(Error::io("read the vault"))?;
            let shard_name = shard.file_name().to_string_lossy().into_owned();
            if shard_name.len() != 2 || !shard.path().is_dir() {
                continue;
            }
            for file in std::fs::read_dir(shard.path()).map_err(Error::io("read the vault"))? {
                let file_name = file.map_err(Error::io("read the vault"))?.file_name().to_string_lossy().into_owned();
                if let Ok(name) = format!("{shard_name}{file_name}").parse::<ChunkName>() {
                    names.push(name);
                }
            }
        }
        names.sort();
        Ok(names)
    }

    pub fn remove(&self, name: &ChunkName) -> Result<bool> {
        match std::fs::remove_file(self.shard(name).1) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(Error::Io { what: "remove a chunk", source: e }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::keys::{EidolonKey, Keys};

    struct Tmp(PathBuf);

    impl Tmp {
        fn new() -> Tmp {
            let id = xenonet_id::codec::hex(&xenonet_id::codec::random::<8>().unwrap());
            Tmp(std::env::temp_dir().join(format!("eidolon-vault-test-{id}")))
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn keys() -> Keys {
        Keys::new(&EidolonKey::from_bytes([9; 32])).unwrap()
    }

    #[test]
    fn put_get_has_list_and_remove() {
        let tmp = Tmp::new();
        let vault = Vault::open(&tmp.0).unwrap();
        let keys = keys();
        let (a, sealed_a) = keys.seal(b"one");
        let (b, sealed_b) = keys.seal(b"two");
        assert!(!vault.has(&a));
        assert!(matches!(vault.get(&a), Err(Error::MissingChunk(_))));
        vault.put(&a, &sealed_a).unwrap();
        vault.put(&b, &sealed_b).unwrap();
        vault.put(&b, &sealed_b).unwrap();
        assert!(vault.has(&a));
        assert_eq!(vault.get(&a).unwrap(), sealed_a);
        assert_eq!(keys.open(&b, &vault.get(&b).unwrap()).unwrap(), b"two");
        let mut want = vec![a, b];
        want.sort();
        assert_eq!(vault.list().unwrap(), want);
        assert!(vault.remove(&a).unwrap());
        assert!(!vault.remove(&a).unwrap());
        assert!(!vault.has(&a));
    }

    #[test]
    fn a_chunk_lives_at_its_two_character_shard() {
        let tmp = Tmp::new();
        let vault = Vault::open(&tmp.0).unwrap();
        let (name, sealed) = keys().seal(b"x");
        vault.put(&name, &sealed).unwrap();
        let hex = name.to_hex();
        assert!(tmp.0.join("chunks").join(&hex[..2]).join(&hex[2..]).is_file());
    }

    #[test]
    fn a_temp_file_from_a_killed_write_is_not_a_chunk() {
        let tmp = Tmp::new();
        let vault = Vault::open(&tmp.0).unwrap();
        let (name, sealed) = keys().seal(b"real");
        vault.put(&name, &sealed).unwrap();
        let shard = tmp.0.join("chunks").join(&name.to_hex()[..2]);
        std::fs::write(shard.join(".deadbeef.123.abc.tmp"), b"half a chu").unwrap();
        assert_eq!(vault.list().unwrap(), vec![name]);
    }

    #[test]
    fn strangers_in_the_vault_are_not_chunks() {
        let tmp = Tmp::new();
        let vault = Vault::open(&tmp.0).unwrap();
        std::fs::write(tmp.0.join("chunks").join("README"), b"x").unwrap();
        std::fs::create_dir_all(tmp.0.join("chunks").join("zz")).unwrap();
        std::fs::write(tmp.0.join("chunks").join("zz").join("notachunk"), b"x").unwrap();
        assert_eq!(vault.list().unwrap().len(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn directories_and_chunks_are_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = Tmp::new();
        let vault = Vault::open(&tmp.0).unwrap();
        let (name, sealed) = keys().seal(b"secret");
        vault.put(&name, &sealed).unwrap();
        let hex = name.to_hex();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let chunks = tmp.0.join("chunks");
        assert_eq!(mode(&chunks), 0o700);
        assert_eq!(mode(&chunks.join(&hex[..2])), 0o700);
        assert_eq!(mode(&chunks.join(&hex[..2]).join(&hex[2..])), 0o600);
    }
}
