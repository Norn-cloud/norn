//! Default strategies for host-supplied turn data.
//!
//! Weights favour the main world and the v1 envelope so most turns exercise
//! the domain, while forks, historical views, and tight caps appear often
//! enough to cover every kernel refusal in a modest case budget.

use norn_kernel::{CommitId, Entropy, EnvelopeLimits, LogicalTime, Observations, World, WorldId};
use proptest::prelude::*;

/// Logical times in `[0, 1_000_000)` ms.
pub fn logical_time() -> BoxedStrategy<LogicalTime> {
    (0_u64..1_000_000)
        .prop_map(LogicalTime::from_millis)
        .boxed()
}

/// Arbitrary 32-byte entropy.
pub fn entropy() -> BoxedStrategy<Entropy> {
    proptest::array::uniform32(any::<u8>())
        .prop_map(Entropy::from_bytes)
        .boxed()
}

/// Worlds: main (weight 6), fork (2), historical (1). Shrinks toward main.
pub fn world() -> BoxedStrategy<World> {
    prop_oneof![
        6 => Just(World::Main),
        2 => (0_u8..4).prop_map(|n| World::Fork { id: WorldId(format!("fork-{n}")) }),
        1 => (0_u8..4).prop_map(|n| World::Historical { commit: CommitId(format!("commit-{n}")) }),
    ]
    .boxed()
}

/// Observations built from [`logical_time`], [`entropy`], and [`world`].
pub fn observations() -> BoxedStrategy<Observations> {
    (logical_time(), entropy(), world())
        .prop_map(|(now, entropy, world)| Observations {
            now,
            entropy,
            world,
        })
        .boxed()
}

/// Envelope caps: [`EnvelopeLimits::V1`] (weight 4) or tight caps in `0..=2`
/// per kind (1), so every `EnvelopeExceeded` kind is reachable. Shrinks
/// toward `V1`.
pub fn limits() -> BoxedStrategy<EnvelopeLimits> {
    prop_oneof![
        4 => Just(EnvelopeLimits::V1),
        1 => (0_u32..=2, 0_u32..=2, 0_u32..=2).prop_map(|(max_events, max_commands, max_timers)| {
            EnvelopeLimits { max_events, max_commands, max_timers, ..EnvelopeLimits::V1 }
        }),
    ]
    .boxed()
}
