//! `norn-dataplane`: raw-observation contract, dlt profile, Iceberg writer glue.
//!
//! Ring 1 — native driver and host perimeter (see `AGENTS.md` for the allowed
//! and forbidden dependencies). The crate is IO-free: it defines contracts and
//! pure functions; HTTP clients, catalogs, and clocks live in host code.
//!
//! - [`raw_observation`]: the append-only `raw_observation` contract (ruling
//!   2026-09-25 §8, T12-DP-1). Raw is received evidence, never a source
//!   mirror; deletes and redactions are versioned observations.
//! - [`converge`]: the pure "latest observation wins" view. Duplicate
//!   deliveries converge; delivery order does not matter.
//! - [`iceberg`]: the Iceberg table specification for raw observations. The
//!   catalog/writer wiring is later work.
//! - [`frappe`] and [`shopify`]: the webhook fast path (T12-DP-4). Signatures
//!   are verified (HMAC-SHA256) before a body is parsed; deliveries become
//!   sealed observations with provenance for the cell's observe inbox.
//! - [`sweep`]: the Frappe completeness sweep, a pure planner whose HTTP
//!   requests are executed by the host through `perform`.
//!
//! Observations are history, not authorization: nothing in this crate grants
//! or implies permission to act.

pub mod converge;
pub mod frappe;
pub mod iceberg;
pub mod raw_observation;
pub mod shopify;
pub mod sweep;
pub mod webhook;

pub use converge::{Applied, Convergence, CurrentRecord};
pub use frappe::{FrappeSource, ingest_frappe_webhook};
pub use raw_observation::{
    ObservationDraft, ObservationError, ObservationKind, ObservationSubmission,
    RAW_OBSERVATION_CONTRACT, REDACTED, RawObservation, RedactionPolicy, RedactionRecord,
    parse_exact_json, raw_observation_schema,
};
pub use shopify::{ShopifyHeaders, ShopifySource, ingest_shopify_webhook};
pub use sweep::{
    FrappeSweep, SweepCommand, SweepCursor, SweepDocumentCommand, SweepFeed, SweepPage,
};
pub use webhook::IngestError;
