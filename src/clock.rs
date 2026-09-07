//! Wall clock time. Reading the clock is a side effect, so it enters through a trait
//! and a test supplies a fake.

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

/// The current time, in seconds since the unix epoch.
pub trait Clock: fmt::Debug {
    fn now_secs(&self) -> u64;
}

/// Reads the system clock. A clock set before the epoch reads as zero, which orders
/// a session last rather than failing the tick.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_secs(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    }
}
