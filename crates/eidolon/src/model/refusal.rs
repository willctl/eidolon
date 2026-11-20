use serde::{Deserialize, Serialize};

macro_rules! codes {
    ($($variant:ident => $text:literal,)*) => {
        /// Why a rule said no. The codes are stable.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum RefusalCode {
            $(#[serde(rename = $text)] $variant,)*
        }

        impl RefusalCode {
            pub fn as_str(self) -> &'static str {
                match self {
                    $(RefusalCode::$variant => $text,)*
                }
            }
        }
    };
}

codes! {
    NotAHolder => "not_a_holder",
    SecondSeat => "second_seat",
    StaleGeneration => "stale_generation",
    SeatNotSignedByHolder => "seat_not_signed_by_holder",
    HeadNotSignedByHolder => "head_not_signed_by_holder",
    WrongWall => "wrong_wall",
    ChangedChunk => "changed_chunk",
    LeaveOutEscapes => "leave_out_escapes",
    Retired => "retired",
    UnknownEidolon => "unknown_eidolon",
    OpenUnfinished => "open_unfinished",
    NeedsCell => "needs_cell",
    NotOpen => "not_open",
    NoBrowser => "no_browser",
    ViewOpen => "view_open",
    NoKey => "no_key",
    MovePending => "move_pending",
    NoPending => "no_pending",
    NotInitialised => "not_initialised",
    AlreadyInitialised => "already_initialised",
    UnknownCell => "unknown_cell",
    CellBroken => "cell_broken",
    CellMissing => "cell_missing",
    UnsafeUrl => "unsafe_url",
    OutsideFiles => "outside_files",
    NotAFile => "not_a_file",
    BadFileName => "bad_file_name",
    NoSuchDownload => "no_such_download",
    StillDownloading => "still_downloading",
    UnknownUid => "unknown_uid",
    StaleElement => "stale_element",
    NoPage => "no_page",
    PageGone => "page_gone",
    NoSuchPage => "no_such_page",
    BadArguments => "bad_arguments",
}

/// A rule's no: a code to match on, what happened, and what to do. It
/// travels in a move as `{"code", "message", "fix"}`.
pub type Refusal = adytum::Refusal<RefusalCode>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_travel_as_their_stable_text() {
        for (code, text) in [(RefusalCode::NotAHolder, "not_a_holder"), (RefusalCode::SeatNotSignedByHolder, "seat_not_signed_by_holder"), (RefusalCode::Retired, "retired")] {
            assert_eq!(code.as_str(), text);
            assert_eq!(serde_json::to_string(&code).unwrap_or_default(), format!("\"{text}\""));
            assert_eq!(serde_json::from_str::<RefusalCode>(&format!("\"{text}\"")).ok(), Some(code));
        }
    }

    #[test]
    fn a_refusal_travels_as_its_code_message_and_fix() {
        let r = Refusal::new(RefusalCode::SecondSeat, "It is open on desk.", "Close it there first.");
        let wire = r#"{"code":"second_seat","message":"It is open on desk.","fix":"Close it there first."}"#;
        assert_eq!(serde_json::to_string(&r).unwrap_or_default(), wire);
        assert_eq!(serde_json::from_str::<Refusal>(wire).ok(), Some(r.clone()));
        assert_eq!(r.to_string(), "It is open on desk. Close it there first.");
    }
}
