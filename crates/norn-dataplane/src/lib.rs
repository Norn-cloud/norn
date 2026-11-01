//! Pure contracts at the dataplane boundary. Observations grant no authority.

pub mod raw_observation;

pub use raw_observation::{
    ObservationError, ObservationKind, RAW_OBSERVATION_CONTRACT, REDACTED, RedactionPolicy,
    RedactionRecord, parse_exact_json,
};
