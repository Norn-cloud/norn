//! `norn-projections`: JSON Schema, SQLite DDL, Cedar schema/loss, OWL, types.
//!
//! Ring 0 — pure, portable semantic core; see `AGENTS.md` for the allowed and
//! forbidden dependencies. Projections are one-way: canonical IR is never
//! reconstructed from a projection.

pub mod cedar;
pub mod interop;
pub mod lifecycle;
pub mod lineage;
pub mod mcp;
pub mod sqlite_ddl;

pub use cedar::{
    CedarAttribute, CedarLoss, CedarPrincipalAttribute, CedarSchemaProjection, HOST_ATTRIBUTES,
    project_cedar_schema, project_cedar_schema_with_catalog,
    project_cedar_schema_with_catalog_and_principal_attributes,
    project_cedar_schema_with_principal_attributes, project_host_access_schema,
};
pub use interop::{
    ExportScope, InteropError, dcat_catalog, openlineage_run_events, prov_document,
    skos_concept_scheme, stable_uuid,
};
pub use lifecycle::{
    LIFECYCLE_DIAGRAM_DIR, LifecycleDiagram, declared_lifecycle_diagrams, lifecycle_diagram,
    lifecycle_diagram_path, lifecycle_state_diagram,
};
pub use lineage::{
    BusinessType, BusinessView, CausalChain, LineageBinding, LineageEdge, LineageEdgeKind,
    LineageError, LineageNode, LineageNodeKind, TechnicalAsset, TechnicalView, TwoViews, ViewLink,
    explain, explain_latest, project_views,
};
pub use sqlite_ddl::{
    LinkTable, ObjectTable, PropertyColumn, SqliteDdlError, SqliteDdlInput, SqliteSchema,
    ValueEncoding, project_sqlite_ddl,
};
