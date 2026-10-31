//! The host's clock: the one place the dev host reads time.

use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(feature = "native")]
use std::time::{SystemTime, UNIX_EPOCH};

use norn_kernel::LogicalTime;

/// Supplies the host's logical time (milliseconds since the Unix epoch).
pub trait Clock: Send + Sync {
    /// Now.
    fn now(&self) -> LogicalTime;
}

/// The wall clock.
#[cfg(feature = "native")]
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

#[cfg(feature = "native")]
impl Clock for SystemClock {
    #[expect(
        clippy::disallowed_methods,
        reason = "the native dev host is the capability that supplies wall-clock time to the cell"
    )]
    fn now(&self) -> LogicalTime {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| {
                u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
            });
        LogicalTime::from_millis(millis)
    }
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
