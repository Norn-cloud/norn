//! The replay clock is explicit and shared handles observe the same time.
use std::sync::Arc;

use norn_api::{Clock, ManualClock};
use norn_kernel::LogicalTime;

fn read_clock<C: Clock>(clock: &C) -> LogicalTime {
    clock.now()
}

#[test]
fn replay_clock_can_rewind_and_shared_trait_objects_see_updates() {
    let clock = Arc::new(ManualClock::at(100));
    let shared: Arc<dyn Clock> = Arc::<ManualClock>::clone(&clock);
    assert_eq!(read_clock::<Arc<ManualClock>>(&clock), LogicalTime::from_millis(100));
    assert_eq!(read_clock::<Arc<dyn Clock>>(&shared), LogicalTime::from_millis(100));
    clock.set(0);
    assert_eq!(shared.now(), LogicalTime::EPOCH);
    clock.set(u64::MAX);
    assert_eq!(shared.now().as_millis(), u64::MAX);
    assert_eq!(ManualClock::default().now(), LogicalTime::EPOCH);
}
