pub mod holding;
mod receive;
mod send;
#[cfg(test)]
mod tests;
pub mod wire;

pub use holding::{seat_first, Holding, Pending};
pub use receive::receive;
pub use send::send;

use std::time::Duration;

/// What one side of a move saw.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MoveStats {
    /// Distinct chunk names the head needs.
    pub chunks_offered: u64,
    /// Chunks that crossed the stream.
    pub chunks_sent: u64,
    /// Frame bytes this side wrote.
    pub bytes_sent: u64,
    /// Frame bytes this side read.
    pub bytes_received: u64,
    pub elapsed: Duration,
}
