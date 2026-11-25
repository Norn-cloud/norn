//! Typed world identity and the D03 outbox lifecycle (Ring 0).
//!
//! - [`outbox`](crate::world::outbox) (#1699): the outbox command lifecycle as one total, pure
//!   transition function ([`step`](crate::world::outbox::step)), the [`Queue`](crate::world::outbox::Queue) a command
//!   waits in, and the generation-free [`OperationKey`](crate::world::outbox::OperationKey).
//! - [`pin`](crate::world::pin) (#1698): the typed [`WorldId`](crate::world::pin::WorldId) (`Main` or a fork bound to
//!   its base anchor), the [`SnapshotPin`](crate::world::pin::SnapshotPin) every pinned read carries, and
//!   [`Bounded`](crate::world::pin::Bounded), which keeps a truncated answer distinct from a complete
//!   one.

pub mod outbox;
pub mod pin;
