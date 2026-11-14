//! `norn-formats`: one reader per format family, from source bytes to
//! `SourceSchemaIR`, with a loss report for every source element; files only.
//!
//! Ring 1 — native driver and host perimeter; see `README.md` for the allowed
//! and forbidden dependencies. Every reader is a pure function from source
//! bytes and typed configuration to typed artifacts and diagnostics: no
//! network, no file system, no clock. Readers know formats, never standards:
//! a standard or system (UBL, ISO 20022, Peppol, a tax authority's schema) is
//! a declarative pack that pins its originals and maps them to the ontology.
//!
//! - [`contract`]: the shared [`SchemaImporter`] trait, its typed output, and
//!   the [`LossReport`] every reader fills from a source inventory.
//! - [`xsd`]: the XML Schema reader.
//! - [`pack`]: the format-independent projection a pack's generated
//!   `Norn.sourceSchema` module carries.
//! - [`type_tables`]: deterministic type tables as `RuleIR` v2 decision
//!   tables evaluated through `norn-decisions`.
//! - [`codes`]: the reader diagnostic code registry.

pub mod codes;
pub mod contract;
pub mod pack;
pub mod type_tables;
/// XML Schema reader.
pub mod xsd;

mod xsd_profile;
mod xsd_projection;

pub use contract::{
    ArtifactLayer, Disposition, DropReason, Facet, ImportProvenance, LossEntry, LossReport,
    NornHome, SchemaImport, SchemaImporter, SourceElement,
};
