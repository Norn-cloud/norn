//! Pure observation ingress contracts. Observations never authorize actions.

pub mod raw_observation;

pub use raw_observation::{
    ObservationError, ObservationKind, RAW_OBSERVATION_CONTRACT, REDACTED, RedactionPolicy,
    RedactionRecord, parse_exact_json,
};
