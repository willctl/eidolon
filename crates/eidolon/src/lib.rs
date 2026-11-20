//! An entity's state kept as one sealed thing, an eidolon, opened in an
//! Adytum cell and moved between machines.

pub mod cell;
mod error;
pub mod facet;
pub mod home;
pub mod model;
pub mod moving;
pub mod pack;
mod private;
pub mod rules;
pub mod vault;

#[cfg(feature = "browser")]
pub mod bidi;
#[cfg(feature = "browser")]
pub mod browser;
#[cfg(feature = "browser")]
pub mod mcp;
#[cfg(feature = "browser")]
pub mod view;

#[cfg(feature = "testing")]
pub mod testing;

pub use error::{Error, Result};
