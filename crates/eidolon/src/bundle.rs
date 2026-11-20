//! A bundle is `age(zstd(tar))`, so stock tools open it:
//! `age -d -i key bundle.age | zstd -d | tar -x`.

use crate::error::{io_err, Code, Error, Result};
use crate::key::{HolderKey, MachineKey};
use age::secrecy::SecretString;
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::iter;
use std::path::Path;
use std::str::FromStr;

const ZSTD_LEVEL: i32 = 3;

/// An age recipient a bundle is encrypted to.
pub struct Recipient(Box<dyn age::Recipient + Send>);

impl Recipient {
    pub fn holder(key: &HolderKey) -> Result<Recipient> {
        Ok(Recipient(Box::new(key.recipient()?)))
    }

    /// An extra recipient: a native `age1...` key, an `ssh-ed25519` or
    /// `ssh-rsa` key, or a plugin's `age1<plugin>1...`, such as a YubiKey's.
    pub fn parse(text: &str) -> Result<Recipient> {
        let text = text.trim();
        let bad =
            || Error::refused(Code::BadRecipient, format!("{text:?} is not an age recipient."), "Give an `age1...` or `ssh-ed25519 ...` recipient, or one an age plugin printed.");
        if text.starts_with("ssh-") {
            return age::ssh::Recipient::from_str(text).map(|r| Recipient(Box::new(r))).map_err(|_| bad());
        }
        if let Ok(r) = age::x25519::Recipient::from_str(text) {
            return Ok(Recipient(Box::new(r)));
        }
        let plugin = age::plugin::Recipient::from_str(text).map_err(|_| bad())?;
        let name = plugin.plugin().to_owned();
        age::plugin::RecipientPluginV1::new(&name, &[plugin], &[], Quiet).map(|r| Recipient(Box::new(r))).map_err(|_| {
            Error::refused(Code::BadRecipient, format!("The age plugin `age-plugin-{name}` is not on PATH."), format!("Install age-plugin-{name}, or leave that recipient out."))
        })
    }

    /// One recipient per line, `#` comments, as age's `-R` reads.
    pub fn parse_file(text: &str) -> Result<Vec<Recipient>> {
        text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(Recipient::parse).collect()
    }
}

/// Plugins may print a message; Eidolon answers no questions for them.
#[derive(Clone, Copy)]
struct Quiet;

impl age::Callbacks for Quiet {
    fn display_message(&self, message: &str) {
        eprintln!("{message}");
    }

    fn confirm(&self, _: &str, _: &str, _: Option<&str>) -> Option<bool> {
        None
    }

    fn request_public_string(&self, _: &str) -> Option<String> {
        None
    }

    fn request_passphrase(&self, _: &str) -> Option<SecretString> {
        None
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

struct Hashing<W> {
    inner: W,
    hash: Sha256,
    len: u64,
}

impl<W: Write> Write for Hashing<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hash.update(&buf[..n]);
        self.len += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// What a sealed bundle came to.
pub struct Written<W> {
    pub out: W,
    pub sha256: String,
    pub size: u64,
}

/// Encrypts to `recipients` what `fill` writes as the tar, compressed.
pub(crate) fn seal<W: Write, T>(recipients: &[Recipient], out: W, fill: impl FnOnce(&mut dyn Write) -> Result<T>) -> Result<(T, Written<W>)> {
    let encryptor = age::Encryptor::with_recipients(recipients.iter().map(|r| r.0.as_ref() as &dyn age::Recipient))
        .map_err(|e| Error::refused(Code::BadRecipient, format!("The recipients do not go together: {e}."), "Leave out the recipient that differs."))?;
    let writing = || io_err("write the bundle");
    let hashing = Hashing { inner: out, hash: Sha256::new(), len: 0 };
    let age = encryptor.wrap_output(hashing).map_err(writing())?;
    let mut zstd = zstd::Encoder::new(age, ZSTD_LEVEL).map_err(writing())?;
    let filled = fill(&mut zstd)?;
    let hashing = zstd.finish().and_then(age::stream::StreamWriter::finish).map_err(writing())?;
    let mut out = hashing.inner;
    out.flush().map_err(writing())?;
    Ok((filled, Written { out, sha256: hex(&hashing.hash.finalize()), size: hashing.len }))
}

pub fn sha256_of(path: &Path) -> Result<String> {
    let mut file = File::open(path).map_err(io_err(format!("read {}", path.display())))?;
    let mut hash = Sha256::new();
    io::copy(&mut file, &mut hash).map_err(io_err(format!("read {}", path.display())))?;
    Ok(hex(&hash.finalize()))
}

/// The tar inside a bundle, decrypted with this machine's key and decompressed.
pub(crate) fn open(path: &Path, key: &MachineKey) -> Result<impl Read> {
    let file = File::open(path).map_err(io_err(format!("read {}", path.display())))?;
    let damaged = |e: age::DecryptError| Error::refused(Code::BundleChanged, format!("The bundle does not decrypt: {e}."), "Seal the target again on its holder.");
    let decryptor = age::Decryptor::new_buffered(BufReader::new(file)).map_err(damaged)?;
    let identity = key.identity()?;
    let reader = decryptor.decrypt(iter::once(&identity as &dyn age::Identity)).map_err(|e| match e {
        age::DecryptError::NoMatchingKeys => Error::refused(
            Code::NotForThisMachine,
            "The bundle is not encrypted to this machine's key.",
            "Check that this machine was on the holder list when the target was sealed.",
        ),
        e => damaged(e),
    })?;
    zstd::Decoder::new(reader).map_err(io_err("start decompressing the bundle"))
}
