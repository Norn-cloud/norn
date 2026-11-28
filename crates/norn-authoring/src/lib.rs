//! Native Rust TypeScript authoring compiler and formatter embedding `tsrs`.
//!
//! Slice 1 establishes the foundational Ring-1 authoring surface:
//!
//! - [`Profile`]: embedded, structurally validated `norn-effect` profile tables
//!   and ambient `@norn/authoring` type declarations.
//! - [`BoundedVfs`] and [`VfsLimits`]: a symlink-free, read-only, quota-bounded
//!   in-memory [`tsrs_vfs::FS`] serving bundled `lib.*.d.ts` files, virtual
//!   `/@norn/authoring/index.d.ts`, and caller-supplied TypeScript sources.
//! - [`AuthoringProgram`] and [`SymbolIdentity`]: bounded `tsrs` compilation,
//!   syntactic and semantic diagnostic collection, and alias-following symbol
//!   identity resolution via `tsrs_checker`.
//! - [`format_source`]: faithful port of `tools/norn-ts/src/fmt.ts` upholding
//!   `FMT-SEMANTIC-1` (never executes authored source and never deletes unused
//!   imports).
//! - [`ProfileChecker`]: faithful Rust port of `tools/norn-ts/src/check.ts`
//!   enforcing the `norn-effect` positive grammar, symbol identity checks,
//!   import closure checks, and `NORN-FLOW-*` diagnostics.
//! - [`extract_relative_module_specifiers`]: `tsrs_parser`-backed static
//!   relative module specifier extraction for `.ts` and `.tsx` sources.

mod checker;
mod compiler;
mod error;
mod fmt;
mod imports;
mod profile;
mod program;
mod vfs;
mod vfs_mount;

pub use checker::{FileReport, Position, ProfileChecker, ProfileDiagnostic, SymbolUse};
pub use compiler::{
    CanonicalError, CompiledExample, CompilerLimits, CurrencySource, DashboardsCompileOutcome,
    DecimalType, DefinitionSource, DefinitionsCompileOptions, DefinitionsCompileOutcome,
    DefinitionsCompileOutput, DependencyPin, EnumValue, EnvelopeOptions, ExampleExpectation,
    FLOW_MAX_RECURS, FLOW_MAX_ROWS, FLOW_MAX_STEPS, FLOW_MAX_TIMER_MILLIS,
    FLOW_SOURCE_MAP_DRAFT_FORMAT, FLOW_SOURCE_MAP_FORMAT, FlowCompileOptions, FlowCompileOutcome,
    FlowCompileOutput, FlowDef, FlowDisplay, FlowLimits, FlowMeta, FlowSourceMapAnnotation,
    FlowSourceMapArtifactRef, FlowSourceMapDraft, FlowSourceMapEdge, FlowSourceMapEdgeKind,
    FlowSourceMapFlow, FlowSourceMapGraph, FlowSourceMapStep, FlowSourceMapStepKind, FlowStepLabel,
    FnRef, InputGroup, LawMetadata, LawStatement, LimitCheck, LimitsEnvelope, LimitsProfile,
    ObjectSchema, PENDING_SOURCE_SHA256, PropertyAdditivity, PropertyType, QuestionsCompileOutcome,
    RetainedDefinitionDoc, RetryDelay, RetryPolicy, Rounding, SchemaLabels, SourceLocation,
    StatementError, StepLimitKind, StepLimits, TaggedErrorDecl, TextFormat, TimezonePolicy,
    UnitRef, ValueSchema, ValueSchemasCompileOutcome, canonical_json, compact_canonical_json,
    compile_dashboards_program, compile_dashboards_vfs, compile_definitions_program,
    compile_definitions_vfs, compile_flow_program, compile_flow_vfs, compile_questions_program,
    compile_questions_vfs, compile_value_schemas_program, flow_limits, from_statement,
    lexical_bounds, may_read_actor, norn_canonical_hash, norn1_sha256, parse_sentence,
    parse_statement, render_flow_limits, render_sentence, serialize_statement, to_canonical_json,
    to_statement,
};
pub use error::AuthoringError;
pub use fmt::{format_source, format_source as format_ts, norn_format_settings};
pub use imports::extract_relative_module_specifiers;
pub use profile::{
    AdmittedSymbol, AllowedImports, DiagnosticDoc, INTRINSICS_DTS, PROFILE_JSON, PackagePin,
    Profile, ProfilePins, RejectedConstruct, RejectedModule, RejectedSymbol, SymbolContext,
    SymbolUseKind,
};
pub use program::{
    AuthoringProgram, DiagnosticCategory, DiagnosticSummary, SymbolIdentity,
    default_compiler_options,
};
pub use vfs::{
    BoundedVfs, INTRINSICS_VIRTUAL_PATH, PINNED_EFFECT_DIST_PREFIX, PINNED_EFFECT_VIRTUAL_ROOT,
    VfsLimits, normalize_virtual_path,
};
