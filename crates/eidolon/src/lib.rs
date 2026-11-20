//! Eidolon seals a folder target, such as a Firefox profile, as
//! `age(zstd(tar))` encrypted to every machine on its holder list, moves it
//! between those machines with exactly one holder at a time, and opens it
//! into a fresh folder that is swapped into place.
//!
//! [`Home`] is a machine's store and does every operation. Targets are
//! [`Target`]s read from TOML; `firefox` is a preset. Seats are SSHSIG-signed
//! JSON ([`SignedSeat`]) checked by [`rules::accept_seat`]. A move is an offer
//! written by [`Home::write_offer`] and read by [`Home::receive`], which
//! answers with an [`Ack`]; both are plain bytes on any duplex.

mod archive;
mod bundle;
mod error;
mod files;
mod holders;
mod home;
mod key;
mod lock;
mod moving;
pub mod rules;
mod seat;
mod swap;
mod target;

pub use archive::{Counts, Limits};
pub use bundle::{hex, sha256_of, Recipient};
pub use error::{Code, Error, Refusal, Result};
pub use holders::{check_name, Holder, Holders};
pub use home::{unpack, Dest, Home, Opened, Sealed, Source, Status, Verified};
pub use key::{HolderKey, KeySource, MachineKey, ACK_NAMESPACE, KEYRING_SERVICE, KEYRING_USER, SEAT_NAMESPACE};
pub use lock::{hold as hold_app_lock, AppLock};
pub use moving::{Ack, AckStatement, Moved, Received, Settled};
pub use seat::{SeatRecord, SignedSeat};
pub use target::{Closed, Rules, Target};
