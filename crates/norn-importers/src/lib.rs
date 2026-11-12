//! `norn-importers`: readers for systems that declare their schema in their
//! own format, behind the `norn-formats` [`SchemaImporter`] contract.
//!
//! Ring 1; every reader is pure (source bytes and pins in, typed output out;
//! no network, file system or clock). The one binary, `norn-import-read`, is
//! the only IO: it reads the files a manifest pins. See `README.md`.
//!
//! - [`ofbiz`]: Apache OFBiz entity-engine XML (entity models, entity groups,
//!   seed data) to `SourceSchemaIR`.
//!
//! [`SchemaImporter`]: norn_formats::SchemaImporter
#![forbid(unsafe_code)]

pub mod ofbiz;
mod xml;

pub use ofbiz::{OfbizConfig, OfbizFile, OfbizReader, Role};
