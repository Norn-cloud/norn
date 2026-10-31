//! The host's clock: the one place the dev host reads time.

use std::sync::atomic::{AtomicU64, Ordering};

use norn_kernel::LogicalTime;

/// Supplies the host's logical time (milliseconds since the Unix epoch).
pub trait Clock: Send + Sync {
    /// Now.
    fn now(&self) -> LogicalTime;
}

/// A clock tests move by hand.
#[derive(Debug, Default)]
pub struct ManualClock(AtomicU64);

impl ManualClock {
    /// A clock at `millis`.
    #[must_use]
    pub const fn at(millis: u64) -> Self {
        Self(AtomicU64::new(millis))
    }

    /// Moves the clock to `millis`.
    pub fn set(&self, millis: u64) {
        self.0.store(millis, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> LogicalTime {
        LogicalTime::from_millis(self.0.load(Ordering::SeqCst))
    }
}

impl<T: Clock + ?Sized> Clock for std::sync::Arc<T> {
    fn now(&self) -> LogicalTime {
        (**self).now()
    }
}
