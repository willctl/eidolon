use crate::model::Refusal;
use crate::vault::ChunkName;
use std::fmt;

/// Why something Eidolon was asked to do did not happen. [`Error::code`] is
/// stable, for scripts; the text says what happened and what to do.
#[derive(Debug)]
pub enum Error {
    Refused(Refusal),
    RefusedByPeer(Refusal),
    /// A chunk did not open, or opened to something its name does not name.
    ChangedChunk,
    ChunkTooLarge,
    MissingChunk(ChunkName),
    BadValue {
        what: &'static str,
        rule: &'static str,
    },
    Damaged {
        what: &'static str,
        why: String,
    },
    UnsafePath(String),
    NotEmpty(String),
    Cut,
    Protocol(String),
    Io {
        what: &'static str,
        source: std::io::Error,
    },
    Crypto(&'static str),
    KeyStore(String),
    Cell(adytum::Error),
    Command {
        what: String,
        status: Option<i32>,
        stderr: String,
    },
    Fingerprint {
        expected: String,
        found: Vec<String>,
    },
    #[cfg(feature = "browser")]
    Bidi(crate::bidi::Error),
    #[cfg(feature = "browser")]
    GaveUp {
        attempts: u32,
        last: String,
    },
    #[cfg(feature = "browser")]
    Mcp(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub fn code(&self) -> String {
        let code = match self {
            Error::Refused(r) | Error::RefusedByPeer(r) => r.code.as_str(),
            Error::ChangedChunk | Error::ChunkTooLarge | Error::MissingChunk(_) => "changed_chunk",
            Error::BadValue { .. } => "bad_value",
            Error::Damaged { .. } => "damaged",
            Error::UnsafePath(_) => "unsafe_path",
            Error::NotEmpty(_) => "not_empty",
            Error::Cut => "cut",
            Error::Protocol(_) => "protocol",
            Error::Io { .. } => "io",
            Error::Crypto(_) => "crypto",
            Error::KeyStore(_) => "key_store",
            Error::Cell(e) => e.code(),
            Error::Command { .. } => "cell_command",
            Error::Fingerprint { .. } => "fingerprint",
            #[cfg(feature = "browser")]
            Error::Bidi(_) | Error::GaveUp { .. } => "browser",
            #[cfg(feature = "browser")]
            Error::Mcp(_) => "mcp",
        };
        code.to_owned()
    }

    pub(crate) fn io(what: &'static str) -> impl FnOnce(std::io::Error) -> Error {
        move |source| Error::Io { what, source }
    }

    pub(crate) fn refused(code: crate::model::RefusalCode, message: impl Into<String>, fix: impl Into<String>) -> Error {
        Error::Refused(Refusal::new(code, message, fix))
    }

    pub(crate) fn random() -> Error {
        Error::Crypto("the system gave no random numbers")
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Refused(r) => write!(f, "{} {}", r.message, r.fix),
            Error::RefusedByPeer(r) => write!(f, "The other machine refused: {} {}", r.message, r.fix),
            Error::ChangedChunk | Error::ChunkTooLarge => {
                f.write_str("A chunk does not match its name, so it was damaged or replaced. Move the eidolon again from the machine that sealed it.")
            }
            Error::MissingChunk(n) => write!(f, "The vault has no chunk {n}. Move the eidolon again from the machine that has it."),
            Error::BadValue { what, rule } => write!(f, "That {what} is not valid. It must be {rule}."),
            Error::Damaged { what, why } => write!(f, "The {what} is damaged ({why}). Restore it from the machine that sealed or sent the eidolon."),
            Error::UnsafePath(path) => write!(f, "The entry {path:?} would write outside its folder, so nothing was unpacked. Do not open this manifest."),
            Error::NotEmpty(path) => write!(f, "{path} is not empty. Unpack into a folder that does not exist yet or is empty."),
            Error::Cut => f.write_str("The connection ended before the move finished. Run the move again; it sends only what is missing."),
            Error::Protocol(why) => write!(f, "The other machine broke the move protocol: {why}. Check that both sides run the same version."),
            Error::Io { what, source } => write!(f, "Could not {what}: {source}."),
            Error::Crypto(why) => write!(f, "Encryption failed: {why}. Try again."),
            Error::KeyStore(why) => write!(f, "The key store failed: {why}. Check that the keys folder can be written."),
            Error::Cell(e) => write!(f, "{e} {}", e.fix()),
            Error::Command { what, status, stderr } => {
                let how = status.map_or_else(|| "was stopped".to_owned(), |s| format!("exited with status {s}"));
                let said = if stderr.is_empty() { String::new() } else { format!(" It said: {stderr}") };
                write!(f, "{what} failed: the command {how}.{said} Fix that and run it again; finished steps are safe to repeat.")
            }
            Error::Fingerprint { expected, found } => write!(
                f,
                "The key Mozilla's repository signs with is not the one Eidolon pins (wanted {expected}, found {}). Nothing was trusted. Check Mozilla's documentation before changing the pin.",
                if found.is_empty() { "none".to_owned() } else { found.join(", ") }
            ),
            #[cfg(feature = "browser")]
            Error::Bidi(e) => write!(f, "{e}."),
            #[cfg(feature = "browser")]
            Error::Mcp(why) => write!(f, "The MCP server stopped: {why}."),
            #[cfg(feature = "browser")]
            Error::GaveUp { attempts, last } => write!(
                f,
                "The browser did not answer after {attempts} attempts; the last said: {last}. Start it with `systemctl start eidolon-browser` in the cell and check `journalctl -u eidolon-browser`."
            ),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } => Some(source),
            Error::Cell(e) => Some(e),
            _ => None,
        }
    }
}

impl From<Refusal> for Error {
    fn from(r: Refusal) -> Error {
        Error::Refused(r)
    }
}

impl From<adytum::Error> for Error {
    fn from(e: adytum::Error) -> Error {
        Error::Cell(e)
    }
}

#[cfg(feature = "browser")]
impl From<crate::bidi::Error> for Error {
    fn from(e: crate::bidi::Error) -> Error {
        Error::Bidi(e)
    }
}
