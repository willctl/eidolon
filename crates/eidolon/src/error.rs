use serde::{Deserialize, Serialize};
use std::fmt;
use std::io;

macro_rules! codes {
    ($($variant:ident => $text:literal,)*) => {
        /// Why Eidolon said no. The text of each code is stable.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum Code {
            $(#[serde(rename = $text)] $variant,)*
        }

        impl Code {
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Code::$variant => $text,)*
                }
            }
        }
    };
}

codes! {
    NotAHolder => "not_a_holder",
    NotTheHolder => "not_the_holder",
    StaleGeneration => "stale_generation",
    SeatNotSignedByHolder => "seat_not_signed_by_holder",
    SecondSeat => "second_seat",
    HoldersDiffer => "holders_differ",
    TargetDiffers => "target_differs",
    UnknownTarget => "unknown_target",
    UnknownHolder => "unknown_holder",
    NoPath => "no_path",
    NotSealed => "not_sealed",
    NotOpened => "not_opened",
    Changed => "changed",
    Busy => "busy",
    NoKey => "no_key",
    Ahead => "ahead",
    AppRunning => "app_running",
    MovePending => "move_pending",
    NoPending => "no_pending",
    UnsafeEntry => "unsafe_entry",
    TooLarge => "too_large",
    BundleChanged => "bundle_changed",
    NotForThisMachine => "not_for_this_machine",
    BadAck => "bad_ack",
    BadOffer => "bad_offer",
    BadTarget => "bad_target",
    BadHolder => "bad_holder",
    BadRecipient => "bad_recipient",
    BadName => "bad_name",
    KeyNotPrivate => "key_not_private",
    NoKeyStore => "no_key_store",
}

/// A rule's no: a code to match on, what happened, and what to do about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusal {
    pub code: Code,
    pub message: String,
    pub fix: String,
}

impl Refusal {
    pub fn new(code: Code, message: impl Into<String>, fix: impl Into<String>) -> Refusal {
        Refusal { code, message: message.into(), fix: fix.into() }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.message, self.fix)
    }
}

#[derive(Debug)]
pub enum Error {
    Refused(Refusal),
    Io { doing: String, source: io::Error },
}

impl Error {
    pub fn refused(code: Code, message: impl Into<String>, fix: impl Into<String>) -> Error {
        Error::Refused(Refusal::new(code, message, fix))
    }

    pub fn code(&self) -> Option<Code> {
        match self {
            Error::Refused(r) => Some(r.code),
            Error::Io { .. } => None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Refused(r) => r.fmt(f),
            Error::Io { doing, source } => write!(f, "Could not {doing}: {source}."),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Refused(_) => None,
            Error::Io { source, .. } => Some(source),
        }
    }
}

impl From<Refusal> for Error {
    fn from(r: Refusal) -> Error {
        Error::Refused(r)
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// `io_err("read the holders")` turns an `io::Error` into an [`Error`] that says what failed.
pub(crate) fn io_err(doing: impl Into<String>) -> impl FnOnce(io::Error) -> Error {
    let doing = doing.into();
    move |source| Error::Io { doing, source }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_travel_as_their_stable_text() {
        for (code, text) in [(Code::NotAHolder, "not_a_holder"), (Code::SeatNotSignedByHolder, "seat_not_signed_by_holder"), (Code::UnsafeEntry, "unsafe_entry")] {
            assert_eq!(code.as_str(), text);
            assert_eq!(serde_json::to_string(&code).unwrap(), format!("\"{text}\""));
            assert_eq!(serde_json::from_str::<Code>(&format!("\"{text}\"")).unwrap(), code);
        }
    }
}
