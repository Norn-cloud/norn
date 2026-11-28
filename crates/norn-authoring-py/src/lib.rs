//! Native Rust Python authoring compiler, AST sandbox, expression lowerer,
//! declarative schema elaborator, and formatter for Norn (`norn-authoring-py`).
//!
//! Embeds Astral's `ruff_python_parser`, `ruff_python_ast`, and
//! `ruff_python_formatter` (`0.0.23`, MIT) to statically validate Python
//! authoring modules, lower expression callbacks directly to `norn-zen/7`
//! declarative IR and lower `tools/norn-py` schema modules directly to
//! canonical `EntityIR` v2, `Markings`, and `LawIR` (`irVersion: "0.1"`)
//! without executing `CPython`, and format Python authoring sources
//! deterministically in-process.

mod budget;
pub mod canonical;
pub mod decimal;
pub mod error;
pub mod expr;
mod expr_lower;
mod expr_validate;
pub mod fmt;
mod line_table;
pub mod sandbox;
pub mod schema;

pub use canonical::{
    DependencyPin, EnvelopeHeader, EnvelopeOptions, JsonValue, MAX_JSON_DEPTH, NORN1_DOMAIN_TAG,
    build_envelope, canonical_json, canonical_norn1_bytes, canonical_norn1_sha256,
    norn1_sha256_bytes,
};
pub use decimal::{MAX_DECIMAL_DIGITS, MAX_DECIMAL_EXPONENT, format_exact_decimal};
pub use error::{
    CODE_CANONICAL, CODE_DEPLOYMENT_ENDPOINT, CODE_DEPLOYMENT_SCOPE, CODE_ELABORATION,
    CODE_EXPRESSION, CODE_FLOAT_LITERAL, CODE_FLOW_LITERAL, CODE_FORBIDDEN_ATTR,
    CODE_FORBIDDEN_CALL, CODE_FORBIDDEN_CONTROL_FLOW, CODE_FORBIDDEN_IMPORT, CODE_FORBIDDEN_NAME,
    CODE_FORBIDDEN_SYNTAX, CODE_FORMAT, CODE_INTERFACE_LIFECYCLE, CODE_IO, CODE_PACK_CONFLICT,
    CODE_PACK_CYCLE, CODE_PACK_DUPLICATE, CODE_PACK_MISSING_DEP, CODE_RAW_EXPR, CODE_RAW_IR,
    CODE_RAW_SECRET, CODE_SCHEMA, CODE_SYNTAX, CODE_TYPE_MISMATCH, CODE_UNBOUNDED,
    CODE_UNKNOWN_FIELD, CODE_UNSAFE_ANNOTATION, PyAuthoringError, SourceSpan,
};
pub use expr::{
    ALLOWED_CALLBACK_FUNCS, ALLOWED_CALLBACK_METHODS, BinaryOp, CompareOp, LogicalOp, LowerContext,
    ZenExpr, ZenLiteral, lower_callback_source, lower_callback_source_with_limits,
    lower_module_callbacks, render_zen_string, validate_callback_source,
    validate_callback_source_with_limits,
};
pub use fmt::{
    FormatConfig, FormatQuoteStyle, format_python_source, format_python_source_with, is_formatted,
};
pub use sandbox::{
    ALLOWED_IMPORT_ROOTS, FORBIDDEN_ATTRS, FORBIDDEN_NAMES, MAX_AST_DEPTH, MAX_AST_NODES,
    MAX_SOURCE_BYTES, PreflightReport, SandboxLimits, allowed_import_symbols, is_forbidden_attr,
    is_forbidden_name, preflight_source, preflight_source_with_limits,
};
pub use schema::{
    CellHostKind, DeploymentSpec, ElaborationResult, FieldDefault, FieldMeta, FnRef,
    InvariantClassification, KeySpec, LawModality, LawSeverity, LinkSpec, MAX_ANNOTATION_DEPTH,
    MarkingDecl, MarkingSeverity, MarkingsCatalog, MaskSpec, Multiplicity, PackCategory,
    PackConflictPolicy, PackManifest, PropertyTypeSpec, RoleKind, Rounding, ScopeRootKind,
    SecretRef, SettingValue, TextFormat, TimezonePolicy, UnitRef, UnitSystem, compose_packs,
    elaborate_file, elaborate_source, elaborate_source_with_limits, is_valid_endpoint_key,
    is_valid_object_type_name, is_valid_worker_endpoint_url, read_regular_file_bounded,
    validate_annotation_source,
};
