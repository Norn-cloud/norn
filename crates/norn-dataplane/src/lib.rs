//! Pure contracts at the dataplane boundary. Observations grant no authority.

pub mod raw_observation;

pub use raw_observation::{
    ObservationDraft, ObservationError, ObservationKind, ObservationSubmission,
    RAW_OBSERVATION_CONTRACT, REDACTED, RawObservation, RedactionPolicy, RedactionRecord,
    parse_exact_json, raw_observation_schema,
};

pub mod converge;
pub use converge::{Applied, Convergence, ConvergenceError, CurrentRecord};
