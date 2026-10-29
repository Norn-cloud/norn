//! The canonical result of one `decide` graph evaluation (ruling 2026-09-25
//! §3: `decide(slice, event) -> { facts: [FactChange], commands, timers }`).
//!
//! [`DecisionOutput`] is the hand-off between `norn-decisions`, which produces
//! it from an admitted graph (T3-DEC-2), and `norn-kernel`, which applies it
//! in one commit through the cell host (T2-KER-2b). It lives here because both
//! crates need it and neither may depend on the other's internals.
//!
//! The members mirror the graph's output record ([`crate::DECIDE_OUTPUT_MEMBERS`]):
//!
//! - `facts`: canonical [`FactChange`] records;
//! - `commands`: the decision's own `Command` item records, each the payload
//!   of one outbound command;
//! - `timers`: the decision's own `Timer` item records. Two members are
//!   reserved for the kernel: `afterMillis` (required, a non-negative integer
//!   delay from the turn's logical time) and `timerId` (optional string; the
//!   same id replaces a pending timer). The whole record is the payload the
//!   timer delivers back when it fires.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::FactChange;
use crate::runtime::RuntimeObject;

/// Reserved timer-record member: the delay in milliseconds from the turn's
/// logical time (a non-negative integer).
pub const TIMER_AFTER_MILLIS_MEMBER: &str = "afterMillis";

/// Reserved timer-record member: an explicit timer id (a non-empty string).
pub const TIMER_ID_MEMBER: &str = "timerId";

/// The result of one `decide` evaluation. See the module docs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DecisionOutput {
    /// Fact changes, applied in order.
    pub facts: Vec<FactChange>,
    /// Outbound command records, enqueued in order (exact runtime values).
    pub commands: Vec<RuntimeObject>,
    /// Timer records, scheduled in order (exact runtime values).
    pub timers: Vec<RuntimeObject>,
}
