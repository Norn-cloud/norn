//! The versioned resource-limit profile (T2-LIM-1, ruling 2026-09-27 §1/§4).
//!
//! A [`ResourceLimitProfile`] is declarative data: the synchronous budget a
//! cell turn is admitted under. It is versioned, content-addressed with
//! `norn-canonical` (Norn1 canonical JSON, domain-tagged SHA-256), and its
//! [`LimitProfileRef`] (id + version + hash) is recorded in receipts so a
//! receipt names the exact limits it was admitted under.
//!
//! **Every number here is a v1 qualification target pending measurement**
//! (ruling 2026-09-27 §1, "Tense"), not a benchmark result. Calibrating the
//! fuel budgets and the per-turn aggregate caps is `T3-BUDGET-1` (#2775); any
//! change to a value is a new profile version with a new hash, never an edit of
//! `norn.limits/1`.
//!
//! This module only defines and hashes the data. Enforcement lives in
//! `norn-kernel` (envelope counts in `step`, slice/byte ingress checks) and, for
//! CPU/fuel/memory metering, in `T3-BUDGET-1`.
//!
//! Units: durations are integer microseconds, sizes are bytes (`u64`), counts
//! are `u32`, and fuel is the Norn ZEN metering unit (`zen_expression::meter`
//! cost units of the pinned `Norn-cloud/zen` fork, NORN.md "Metering cost
//! model": one per executed VM opcode, graph node visit, and decision-table
//! row, plus data-proportional builtin charges).

use std::collections::BTreeMap;

use norn_canonical::{CanonicalJsonError, HashProfile, JsonValue};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Identifier of the shipped default profile family.
pub const NORN_LIMITS_PROFILE_ID: &str = "norn.limits";

/// Version of the shipped default profile.
pub const NORN_LIMITS_V1_VERSION: u32 = 1;

/// Pinned content hash of `norn.limits/1`: lower-case hex SHA-256 of
/// `NORN1_DOMAIN_TAG || canonical_json(ResourceLimitProfile::norn_limits_v1())`.
///
/// Golden value. A change here means the profile's bytes changed, which must
/// be a new profile version instead (tested in `tests/limit_profile.rs`).
pub const NORN_LIMITS_V1_SHA256: &str =
    "1f0296b10aa2d862ce777ad83183c7e184e69be9891ef0a4c442df36e53c6f05";

/// The hash profile the limit-profile content hash is computed under.
pub const LIMIT_PROFILE_HASH_PROFILE: HashProfile = HashProfile::Norn1;

const KIB: u64 = 1024;
const MIB: u64 = 1024 * 1024;

/// A versioned resource-limit profile (ruling 2026-09-27 §1).
///
/// All values are v1 qualification targets pending measurement.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResourceLimitProfile {
    /// Profile family identifier, e.g. `norn.limits`.
    pub id: String,
    /// Profile version within the family. Any changed value is a new version.
    pub version: u32,
    /// One warm `decide`.
    pub decide: DecideBudget,
    /// The whole synchronous cell turn: one aggregate budget.
    pub turn: TurnBudget,
    /// The declared input slice.
    pub slice: SliceLimits,
    /// Working memory.
    pub memory: MemoryLimits,
    /// The per-decision output envelope.
    pub envelope: EnvelopeLimits,
    /// An inline custom helper, only if D33 ever admits one.
    pub helper: HelperLimits,
}

/// Budget for one warm `decide` (§1 row 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DecideBudget {
    /// Target p99 CPU per decision, in microseconds (qualification target;
    /// not enforceable by a clock inside the kernel).
    pub cpu_p99_micros: u64,
    /// The CPU envelope the fuel budget is calibrated against, in
    /// microseconds.
    pub fuel_envelope_micros: u64,
    /// Deterministic fuel per decision, in Norn ZEN metering units.
    /// Provisional and uncalibrated until `T3-BUDGET-1`.
    pub fuel_units: u64,
}

/// Budget for one synchronous cell turn (§1 row 2): one aggregate budget that
/// every triggered decision, validation, and helper debits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TurnBudget {
    /// Target p99 active CPU per turn, in microseconds.
    pub cpu_p99_micros: u64,
    /// Deterministic fuel per turn, in Norn ZEN metering units, shared by every
    /// decision in the turn. Provisional and uncalibrated until `T3-BUDGET-1`.
    pub fuel_units: u64,
}

/// Bounds on the declared input slice (§1 row 3), enforced while reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SliceLimits {
    /// Maximum canonical input bytes of the slice.
    pub max_canonical_bytes: u64,
    /// Maximum selected records.
    pub max_records: u32,
    /// Maximum relation depth.
    pub max_relation_depth: u32,
    /// The byte ceiling a later, measured profile may raise
    /// `max_canonical_bytes` to. Informational: never enforced in place of
    /// `max_canonical_bytes`.
    pub qualification_ceiling_bytes: u64,
}

/// Working-memory targets (§1 row 4). Allocation metering is `T3-BUDGET-1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryLimits {
    /// Maximum incremental working allocation per admitted turn, in bytes.
    pub max_turn_working_bytes: u64,
    /// Integrated live-footprint target under the concurrency fixture, in
    /// bytes.
    pub live_footprint_target_bytes: u64,
}

/// Per-decision envelope caps (§1 row 5).
///
/// Counts are `u32` and sizes `u64` so the serialized profile is identical on
/// native and `wasm32` hosts. `norn-kernel`'s `step` enforces the counts; the
/// host checks `max_output_bytes` against the bytes it serializes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EnvelopeLimits {
    /// Maximum serialized metadata/output bytes per decision.
    pub max_output_bytes: u64,
    /// Maximum events (facts) per decision.
    pub max_events: u32,
    /// Maximum `perform` effects per decision.
    pub max_commands: u32,
    /// Maximum timer effects (schedule + cancel) per decision.
    pub max_timers: u32,
}

impl EnvelopeLimits {
    /// The `norn.limits/1` envelope: 32 KiB output and at most 32 of each per
    /// decision.
    pub const V1: Self = Self {
        max_output_bytes: 32 * KIB,
        max_events: 32,
        max_commands: 32,
        max_timers: 32,
    };
}

impl Default for EnvelopeLimits {
    fn default() -> Self {
        Self::V1
    }
}

/// Limits for an inline custom helper (§1 row 6). D33 admits none in v1, so
/// these only bind a future admission; the helper's fuel is charged to the
/// enclosing decision and turn budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HelperLimits {
    /// Target p99 CPU per helper call, in microseconds.
    pub cpu_p99_micros: u64,
    /// Aggregate helper CPU per decision, in microseconds.
    pub aggregate_micros_per_decision: u64,
    /// Maximum guest memory, in bytes.
    pub max_guest_memory_bytes: u64,
}

impl ResourceLimitProfile {
    /// The shipped default profile `norn.limits/1`: ruling 2026-09-27 §1 v1
    /// qualification targets, pending measurement.
    #[must_use]
    pub fn norn_limits_v1() -> Self {
        Self {
            id: NORN_LIMITS_PROFILE_ID.to_owned(),
            version: NORN_LIMITS_V1_VERSION,
            decide: DecideBudget {
                cpu_p99_micros: 2_000,
                fuel_envelope_micros: 5_000,
                // Provisional: not calibrated. T3-BUDGET-1 calibrates against
                // the 5 ms envelope on both hosts and publishes a new version.
                fuel_units: 1_000_000,
            },
            turn: TurnBudget {
                cpu_p99_micros: 10_000,
                // Provisional: the turn/decide CPU ratio (10 ms / 2 ms) applied
                // to the decide fuel. Uncalibrated until T3-BUDGET-1.
                fuel_units: 5_000_000,
            },
            slice: SliceLimits {
                max_canonical_bytes: 256 * KIB,
                max_records: 1_000,
                max_relation_depth: 3,
                qualification_ceiling_bytes: MIB,
            },
            memory: MemoryLimits {
                max_turn_working_bytes: 16 * MIB,
                live_footprint_target_bytes: 64 * MIB,
            },
            envelope: EnvelopeLimits::V1,
            helper: HelperLimits {
                cpu_p99_micros: 100,
                aggregate_micros_per_decision: 500,
                max_guest_memory_bytes: MIB,
            },
        }
    }

    /// The profile as a Norn1 canonical JSON value. Field names equal the
    /// serde field names, so this is the same document `serde_json` produces,
    /// in canonical form (tested).
    #[must_use]
    pub fn to_canonical_value(&self) -> JsonValue {
        let Self {
            id,
            version,
            decide,
            turn,
            slice,
            memory,
            envelope,
            helper,
        } = self;
        object([
            ("id", JsonValue::String(id.clone())),
            ("version", number(u64::from(*version))),
            (
                "decide",
                object([
                    ("cpu_p99_micros", number(decide.cpu_p99_micros)),
                    ("fuel_envelope_micros", number(decide.fuel_envelope_micros)),
                    ("fuel_units", number(decide.fuel_units)),
                ]),
            ),
            (
                "turn",
                object([
                    ("cpu_p99_micros", number(turn.cpu_p99_micros)),
                    ("fuel_units", number(turn.fuel_units)),
                ]),
            ),
            (
                "slice",
                object([
                    ("max_canonical_bytes", number(slice.max_canonical_bytes)),
                    ("max_records", number(u64::from(slice.max_records))),
                    (
                        "max_relation_depth",
                        number(u64::from(slice.max_relation_depth)),
                    ),
                    (
                        "qualification_ceiling_bytes",
                        number(slice.qualification_ceiling_bytes),
                    ),
                ]),
            ),
            (
                "memory",
                object([
                    (
                        "max_turn_working_bytes",
                        number(memory.max_turn_working_bytes),
                    ),
                    (
                        "live_footprint_target_bytes",
                        number(memory.live_footprint_target_bytes),
                    ),
                ]),
            ),
            (
                "envelope",
                object([
                    ("max_output_bytes", number(envelope.max_output_bytes)),
                    ("max_events", number(u64::from(envelope.max_events))),
                    ("max_commands", number(u64::from(envelope.max_commands))),
                    ("max_timers", number(u64::from(envelope.max_timers))),
                ]),
            ),
            (
                "helper",
                object([
                    ("cpu_p99_micros", number(helper.cpu_p99_micros)),
                    (
                        "aggregate_micros_per_decision",
                        number(helper.aggregate_micros_per_decision),
                    ),
                    (
                        "max_guest_memory_bytes",
                        number(helper.max_guest_memory_bytes),
                    ),
                ]),
            ),
        ])
    }

    /// Norn1 canonical JSON bytes of the profile.
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalJsonError`] if `norn-canonical` rejects the value
    /// (not expected: the profile is integers and one string).
    pub fn canonical_json(&self) -> Result<Vec<u8>, CanonicalJsonError> {
        norn_canonical::serialize(&self.to_canonical_value())
    }

    /// Lower-case hex content hash under [`LIMIT_PROFILE_HASH_PROFILE`].
    ///
    /// # Errors
    ///
    /// As [`ResourceLimitProfile::canonical_json`].
    pub fn content_sha256(&self) -> Result<String, CanonicalJsonError> {
        let bytes = self.canonical_json()?;
        Ok(LIMIT_PROFILE_HASH_PROFILE.hash_canonical_bytes(&bytes))
    }

    /// The reference receipts record: id, version, and content hash.
    ///
    /// # Errors
    ///
    /// As [`ResourceLimitProfile::canonical_json`].
    pub fn reference(&self) -> Result<LimitProfileRef, CanonicalJsonError> {
        Ok(LimitProfileRef {
            id: self.id.clone(),
            version: self.version,
            sha256: self.content_sha256()?,
        })
    }
}

/// The identity of the limit profile a turn or transition was admitted under,
/// as recorded in receipts (ruling 2026-09-27 §1/§4).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LimitProfileRef {
    /// Profile family identifier.
    pub id: String,
    /// Profile version.
    pub version: u32,
    /// Lower-case hex Norn1 content hash of the profile.
    pub sha256: String,
}

impl LimitProfileRef {
    /// The pinned reference of the shipped default `norn.limits/1`, without
    /// hashing at runtime (the pin is checked against the computed hash in
    /// tests).
    #[must_use]
    pub fn norn_limits_v1() -> Self {
        Self {
            id: NORN_LIMITS_PROFILE_ID.to_owned(),
            version: NORN_LIMITS_V1_VERSION,
            sha256: NORN_LIMITS_V1_SHA256.to_owned(),
        }
    }
}

fn number(value: u64) -> JsonValue {
    JsonValue::Number(value.to_string())
}

fn object<const N: usize>(entries: [(&str, JsonValue); N]) -> JsonValue {
    JsonValue::Object(
        entries
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect::<BTreeMap<_, _>>(),
    )
}
