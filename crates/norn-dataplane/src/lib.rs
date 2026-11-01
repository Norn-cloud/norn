//! Pure contracts at the dataplane boundary. Observations grant no authority.

pub mod raw_observation;

pub use raw_observation::{
    ObservationDraft, ObservationError, ObservationKind, ObservationSubmission,
    RAW_OBSERVATION_CONTRACT, REDACTED, RawObservation, RedactionPolicy, RedactionRecord,
    parse_exact_json, raw_observation_schema,
};

pub mod converge;
pub use converge::{Applied, Convergence, ConvergenceError, CurrentRecord};

pub mod frappe;
pub mod webhook;
pub use frappe::{FRAPPE_SIGNATURE_HEADER, FrappeSource, frappe_event_kind, ingest_frappe_webhook};
pub use webhook::IngestError;

pub mod shopify;
pub use shopify::{ShopifyHeaders, ShopifySource, ingest_shopify_webhook};

pub mod sweep;
pub use sweep::{
    FrappeSweep, SweepCommand, SweepCursor, SweepDocumentCommand, SweepFeed, SweepPage,
};
