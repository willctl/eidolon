//! Fakes for tests: folders standing in for cells, and a WebDriver BiDi
//! server with a small model of one page. The rest of a fake cell is
//! Adytum's `adytum::testing`.

mod bidi;
mod cells;

pub use bidi::{screenshot_bytes, FakeBidi, NodeKind, Page, PAGE_TEXT, SCREENSHOT_BASE64};
pub use cells::{folder_cells, running};
