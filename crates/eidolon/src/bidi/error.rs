use std::fmt;
use std::time::Duration;

use crate::bidi::frame::FrameError;

/// Why a connection or a command failed. Cloneable, because one broken
/// connection fails every command waiting on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Io(String),
    Handshake(String),
    Protocol(String),
    /// A message is longer than the limit; the connection is closed.
    MessageTooLarge {
        limit: usize,
    },
    Closed(String),
    Remote {
        code: String,
        message: String,
    },
    Script(String),
    Timeout(Duration),
    NotReady(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "the connection to the browser failed: {e}"),
            Self::Handshake(e) => write!(f, "the browser did not accept the WebSocket upgrade: {e}"),
            Self::Protocol(e) => write!(f, "the browser broke the protocol: {e}"),
            Self::MessageTooLarge { limit } => write!(f, "the browser sent a message over the limit of {limit} bytes, so the connection was closed"),
            Self::Closed(why) => write!(f, "the connection to the browser is closed: {why}"),
            Self::Remote { code, message } => write!(f, "the browser refused the command ({code}): {message}"),
            Self::Script(text) => write!(f, "the script threw: {text}"),
            Self::Timeout(d) => write!(f, "the browser did not answer within {} ms", d.as_millis()),
            Self::NotReady(why) => write!(f, "the browser is not ready for a session: {why}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

impl From<FrameError> for Error {
    fn from(e: FrameError) -> Self {
        match e {
            FrameError::TooLarge { limit, .. } => Self::MessageTooLarge { limit },
            other => Self::Protocol(other.to_string()),
        }
    }
}
