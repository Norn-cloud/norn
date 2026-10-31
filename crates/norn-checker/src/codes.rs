//! The diagnostic code registry (T1-CHK-2, #2525).
//!
//! Every code the checker can emit is listed in [`REGISTRY`] with its tier,
//! severity, message template, and the checker version that introduced it.
//! Codes are part of the checker contract: they are never renamed or reused,
//! and a retired code stays registered. `tests/code_registry.rs` scans the
//! crate source so an unregistered code (or a registered code nothing emits)
//! fails the build's tests, and [`conformance_manifest`] is the checked-in
//! `conformance/diagnostic-codes.json` consumed by other frontends (#2425).
//!
//! Tiers follow the 2026-08-27 authoring/checker ruling
//! (`knowledge/architecture/design-rulings-2026-08-27-authoring-gateway-checkers.md`).

use norn_ir::DiagnosticSeverity;

/// The envelope declares an IR version this checker does not support.
pub const IR_UNSUPPORTED_VERSION: &str = "ir.validation.unsupported_version";
/// A required envelope field is missing or blank.
pub const IR_REQUIRED_FIELD: &str = "ir.validation.required_field";
/// The envelope (scope, provenance, or JSON values) is malformed.
pub const IR_INVALID_ARTIFACT: &str = "ir.validation.invalid_artifact";
/// A declared dependency is malformed or self-referential.
pub const IR_INVALID_DEPENDENCY: &str = "ir.validation.invalid_dependency";
/// An explicit layout entry took precedence over extracted legacy geometry.
pub const LAYOUT_MERGE_CONFLICT: &str = "layout.validation.merge_conflict";
/// A definition identifier exceeds the admission profile's byte budget.
pub use crate::envelope::IDENTIFIER_TOO_LONG;

/// The checker version that introduced every code registered before the
/// registry existed.
pub const INITIAL_SINCE: &str = "0.1.0";

/// Format tag of the [`conformance_manifest`] document.
pub const MANIFEST_FORMAT: &str = "norn-checker/diagnostic-codes@1";

/// The checker tier a code belongs to (2026-08-27 ruling, "Tiers").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    /// Starlark syntax, names, advisory types, load resolution, limits.
    T0,
    /// Builder construction (arguments, enums, ranges, local ids, CEL parse).
    T1,
    /// Per-artifact IR validation.
    T2,
    /// Artifact-set and graph semantics.
    T3,
    /// BAC plan (compatibility diff, monotonicity, approvals).
    T4,
    /// Activation (re-run T2–T4 on exact bytes, verify plan and evidence).
    T5,
    /// Runtime/commit (input validation, scope, CAS, idempotency, receipts).
    T6,
    /// Gateway/edge.
    T7,
}

impl Tier {
    /// The tier label (`"T2"`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::T0 => "T0",
            Self::T1 => "T1",
            Self::T2 => "T2",
            Self::T3 => "T3",
            Self::T4 => "T4",
            Self::T5 => "T5",
            Self::T6 => "T6",
            Self::T7 => "T7",
        }
    }
}

/// One registered diagnostic code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeEntry {
    /// The stable dotted code.
    pub code: &'static str,
    /// The checker tier that emits it.
    pub tier: Tier,
    /// The severity every emission carries.
    pub severity: DiagnosticSeverity,
    /// The message shape: the emitted text with `{placeholders}` for the
    /// values each finding interpolates. Emitted messages are human-readable
    /// and may be refined; the code is the contract.
    pub message: &'static str,
    /// The checker version that introduced the code.
    pub since: &'static str,
}

const fn entry(code: &'static str, tier: Tier, message: &'static str) -> CodeEntry {
    CodeEntry {
        code,
        tier,
        severity: DiagnosticSeverity::Error,
        message,
        since: INITIAL_SINCE,
    }
}

const fn introduced(
    code: &'static str,
    tier: Tier,
    message: &'static str,
    since: &'static str,
) -> CodeEntry {
    CodeEntry {
        code,
        tier,
        severity: DiagnosticSeverity::Error,
        message,
        since,
    }
}

/// A registered code whose every emission is a warning: it never blocks a
/// plan (BAC rejects only on error severity).
const fn warning(code: &'static str, tier: Tier, message: &'static str) -> CodeEntry {
    CodeEntry {
        severity: DiagnosticSeverity::Warning,
        ..entry(code, tier, message)
    }
}

const fn warning_introduced(
    code: &'static str,
    tier: Tier,
    message: &'static str,
    since: &'static str,
) -> CodeEntry {
    CodeEntry {
        severity: DiagnosticSeverity::Warning,
        ..introduced(code, tier, message, since)
    }
}

/// Every diagnostic code the checker can emit, sorted by code.
pub static REGISTRY: &[CodeEntry] = &[
    entry(
        "action.validation.command_environment_underivable",
        Tier::T2,
        "outbound command expressions require an object input schema with a derivable expression environment",
    ),
    entry(
        "action.validation.invalid_effect_program",
        Tier::T2,
        "payload.effectProgram is invalid: {detail}",
    ),
    entry(
        "action.validation.invalid_payload",
        Tier::T2,
        "ActionIR payload is invalid at {path}: {detail}",
    ),
    entry(
        "action.validation.invalid_schema",
        Tier::T2,
        "ActionIR {schema} is not a closed schema: {detail}",
    ),
    entry(
        "artifact_set.action.create_link",
        Tier::T3,
        "the action's create-only link effect or its authorized target slice is invalid",
    ),
    entry(
        "artifact_set.action.duplicate_action_id",
        Tier::T3,
        "actionId {actionId} is already used by another action of the artifact set or the installed base",
    ),
    entry(
        "artifact_set.action.effect_kind_mismatch",
        Tier::T3,
        "the effect writes {objectKind}, but entity artifact {artifactId} version {version} defines {definition}",
    ),
    entry(
        "artifact_set.action.invalid_slice",
        Tier::T3,
        "an action slice reads an unknown key property or link",
    ),
    entry(
        "artifact_set.action.invalid_write_mode",
        Tier::T3,
        "the action write mode is not valid for its effective lifecycle",
    ),
    entry(
        "artifact_set.action.naming_series_target_mismatch",
        Tier::T3,
        "the action input does not match the target key's naming-series create or update contract",
    ),
    entry(
        "artifact_set.action.quantity_effect",
        Tier::T3,
        "the action's quantity effect or its input bindings are invalid: {detail}",
    ),
    entry(
        "artifact_set.action.target_not_writable",
        Tier::T3,
        "{target} of {objectKind} is not writable by a generic action: {reason}",
    ),
    entry(
        "artifact_set.action.undeclared_effect_write",
        Tier::T3,
        "the effect program may write {objectKind} ({change}), but the write set does not declare it",
    ),
    entry(
        "artifact_set.action.unknown_slice_kind",
        Tier::T3,
        "the action slice names an object type that is not in the artifact set or installed base",
    ),
    entry(
        "artifact_set.action.unknown_write_kind",
        Tier::T3,
        "the write set names {entityKind}, which is not an object type of the artifact set or the installed base",
    ),
    entry(
        "artifact_set.action.unresolved_effect_entity",
        Tier::T3,
        "the effect pins entity artifact {artifactId} version {version}, which is not an entity artifact of the artifact set or the installed base",
    ),
    entry(
        "artifact_set.action.write_without_effect",
        Tier::T3,
        "the write set declares {entityKind} ({change}), which the effect program never writes",
    ),
    entry(
        "artifact_set.decision.fact_literal_outside_schema",
        Tier::T3,
        "the literal {member} {value} is not admitted by the facts item schema's {member} enum {admitted}",
    ),
    introduced(
        "artifact_set.decision.invalid_source_command",
        Tier::T3,
        "a declared source command does not bind an exact observed mapping and guarded connector capability: {detail}",
        "0.2.0",
    ),
    entry(
        "artifact_set.decision.invalid_stored_property_write",
        Tier::T3,
        "the decision writes {property} of {objectType}, which violates its effective stored-property contract: {detail}",
    ),
    entry(
        "artifact_set.decision.sub_decision_bound",
        Tier::T3,
        "sub-decision {key} nests {depth} levels deep, or the closure holds {count} graphs; the Norn profile admits at most {max}",
    ),
    entry(
        "artifact_set.decision.sub_decision_target",
        Tier::T3,
        "sub-decision {key} resolves to {artifactId} version {version}, which is not a RuleIR v2 decision graph",
    ),
    entry(
        "artifact_set.decision.unknown_binding",
        Tier::T3,
        "{detail}",
    ),
    entry(
        "artifact_set.decision.unknown_fact_object_type",
        Tier::T3,
        "the decision writes {objectType}, which is not an object type of the artifact set or the installed base",
    ),
    entry(
        "artifact_set.decision.unresolved_stored_property_contract",
        Tier::T3,
        "the decision writes {objectType}, whose effective stored-property contracts cannot be resolved: {reason}",
    ),
    entry(
        "artifact_set.entity.duplicate_object_type",
        Tier::T3,
        "object type {name} is already defined by entity artifact {artifactId}",
    ),
    entry(
        "artifact_set.expression.environment_underivable",
        Tier::T3,
        "the ZEN Expression type environment of the artifact set cannot be derived: {detail}",
    ),
    entry(
        "artifact_set.flow_source_map.duplicate_flow",
        Tier::T3,
        "flow id {flowId} appears more than once in the flow source map",
    ),
    entry(
        "artifact_set.flow_source_map.duplicate_step",
        Tier::T3,
        "step id {stepId} is empty or appears more than once in the flow",
    ),
    entry(
        "artifact_set.flow_source_map.invalid_edge",
        Tier::T3,
        "edge {from} -> {to} must reference steps in the same flow",
    ),
    entry(
        "artifact_set.flow_source_map.invalid_location",
        Tier::T3,
        "source location requires a file and one-based line and column numbers",
    ),
    entry(
        "artifact_set.flow_source_map.invalid_mapping",
        Tier::T3,
        "flow source map contains an invalid or duplicate mapping: {detail}",
    ),
    entry(
        "artifact_set.flow_source_map.missing_artifact",
        Tier::T3,
        "flow artifact {artifactId} version {version} is not in the admitted set",
    ),
    entry(
        "artifact_set.flow_source_map.missing_decision_row",
        Tier::T3,
        "decision row id {rowId} is not declared by a mapped RuleIR artifact",
    ),
    entry(
        "artifact_set.flow_source_map.missing_state",
        Tier::T3,
        "state id {stateId} is not declared by a mapped entity artifact",
    ),
    entry(
        "artifact_set.flow_source_map.stale_artifact",
        Tier::T3,
        "flow artifact {artifactId} hash does not match its admitted version",
    ),
    entry(
        "artifact_set.lifecycle.delete_lifecycle_object",
        Tier::T3,
        "the decision may delete {objectType}, which has a declared lifecycle; delete then create would reset its status without a transition",
    ),
    entry(
        "artifact_set.lifecycle.direct_write",
        Tier::T3,
        "{writer} writes {property} of {objectType}, which its lifecycle owns; a status changes only through a declared transition",
    ),
    entry(
        "artifact_set.lifecycle.not_a_lifecycle",
        Tier::T3,
        "{property} of {objectType} has no declared lifecycle to transition",
    ),
    entry(
        "artifact_set.lifecycle.unconstrained_state",
        Tier::T3,
        "the decision creates {objectType} with lifecycle property {property}, which must be a string enum of initial states",
    ),
    entry(
        "artifact_set.lifecycle.undeclared_transition_literal",
        Tier::T3,
        "the expression fires transition {transition}, but {reason}",
    ),
    entry(
        "artifact_set.lifecycle.unknown_transition",
        Tier::T3,
        "{transition} is not a declared transition of the {property} lifecycle of {objectType}",
    ),
    entry(
        "artifact_set.lifecycle.unresolved_lifecycles",
        Tier::T3,
        "{writer} writes {objectType}, whose lifecycles cannot be resolved ({reason}); its status writes fail closed",
    ),
    entry(
        "artifact_set.marking.duplicate",
        Tier::T3,
        "marking id {id} is already declared in the base or candidate set, or a marking reference is repeated",
    ),
    entry(
        "artifact_set.marking.unknown",
        Tier::T3,
        "marking reference {id} has no declaration in the base or candidate set",
    ),
    entry(
        "artifact_set.resolution.conflicting_version",
        Tier::T3,
        "artifact {artifactId} is in the artifact set at versions {first} and {second}; a resolved set holds one version of each artifact",
    ),
    entry(
        "artifact_set.resolution.dependency_cycle",
        Tier::T3,
        "artifacts {artifactIds} depend on each other in a cycle; the set has no installation order",
    ),
    entry(
        "artifact_set.resolution.duplicate_artifact",
        Tier::T3,
        "artifact {artifactId} version {version} appears more than once in the artifact set",
    ),
    entry(
        "artifact_set.resolution.kind_incompatible",
        Tier::T3,
        "a {fromKind} artifact may not reference a {toKind} artifact ({artifactId})",
    ),
    entry(
        "artifact_set.resolution.kind_mismatch",
        Tier::T3,
        "{artifactId} is referenced as a {declaredKind} artifact but is a {kind} artifact",
    ),
    entry(
        "artifact_set.resolution.missing_dependency",
        Tier::T3,
        "required {kind} {artifactId} version {version} is neither in the artifact set nor installed",
    ),
    entry(
        "artifact_set.resolution.shadows_base",
        Tier::T3,
        "{artifactId} is an installed base artifact, which is read-only",
    ),
    entry(
        "artifact_set.resolution.version_mismatch",
        Tier::T3,
        "{artifactId} is pinned at version {version}, but resolves only at {available}",
    ),
    entry(
        "artifact_set.scope.incompatible_reference",
        Tier::T3,
        "{artifactId} is scoped to {dimension} {value}, which the referring artifact's scope cannot see",
    ),
    entry(
        "business_pack.validation.invalid_payload",
        Tier::T2,
        "BusinessPackIR payload is invalid at {path}: {detail}",
    ),
    entry(
        "connector.validation.invalid_payload",
        Tier::T2,
        "ConnectorIR payload is invalid at {path}: {detail}",
    ),
    entry(
        "connector.validation.unknown_resource",
        Tier::T2,
        "resourceId '{resourceId}' does not match a declared connector resource",
    ),
    introduced(
        "consistency.chase.refused",
        Tier::T3,
        "the chase exceeded its {resource} budget of {limit}; consistency is unknown",
        "0.2.0",
    ),
    introduced(
        "consistency.theory.malformed",
        Tier::T3,
        "the lowered theory is malformed: {detail}",
        "0.2.0",
    ),
    introduced(
        "consistency.theory.not_weakly_acyclic",
        Tier::T3,
        "TGDs [{rules}] create existential values on a cycle, so the chase may not terminate",
        "0.2.0",
    ),
    introduced(
        "consistency.theory.vacuous",
        Tier::T3,
        "{relation} can never hold a tuple: from {witness} the rules [{rules}] force {left} = {right}",
        "0.2.0",
    ),
    entry(
        "customization.validation.invalid_payload",
        Tier::T2,
        "CustomizationIR payload is invalid at {path}: {detail}",
    ),
    entry(
        "customization.validation.retired_boundary",
        Tier::T2,
        "executable boundary \"{boundary}\" is retired: inline custom execution is not admitted; move the code into a pool Function invoked through perform",
    ),
    entry(
        "derived.validation.incremental_not_supported",
        Tier::T2,
        "maintenance \"incremental\" is reserved for a future IVM phase and is not yet supported; use \"recompute_on_read\"",
    ),
    entry(
        "derived.validation.invalid_payload",
        Tier::T2,
        "DerivedIR payload is invalid at {path}: {detail}",
    ),
    entry(
        "derived.validation.io_in_view",
        Tier::T2,
        "calc kind \"effectful_ts\" is not allowed inside a Derived view (IO must be in an Effect artifact)",
    ),
    warning(
        "entity.deprecation.v1_payload",
        Tier::T4,
        "entity {artifactId} uses the deprecated EntityIR v1 payload form; it is accepted during the migration and lowered to EntityIR v2 on read; author new versions with entityVersion \"2.0\"",
    ),
    entry(
        "entity.validation.invalid_mutation_authority",
        Tier::T2,
        "payload.mutationAuthority is invalid: {detail}",
    ),
    entry(
        "entity.validation.invalid_payload",
        Tier::T2,
        "EntityIR payload is invalid at {path}: {detail}",
    ),
    entry(
        "entity.validation.layer_scope_mismatch",
        Tier::T3,
        "artifact scope.tenantId {detail} does not match the {layer} layer it is checked in",
    ),
    entry(
        "function.invocation.blank_value",
        Tier::T6,
        "{member} must not be blank",
    ),
    entry(
        "function.invocation.fork_perform",
        Tier::T6,
        "world {world} is a fork; only main performs, and a fork never dispatches a Function",
    ),
    entry(
        "function.invocation.freshness_shape",
        Tier::T6,
        "{freshness} freshness needs {requirement}",
    ),
    entry(
        "function.invocation.input_bound",
        Tier::T6,
        "the staged inputs hold {staged} bytes; the manifest admits {bound}",
    ),
    entry(
        "function.invocation.manifest_mismatch",
        Tier::T6,
        "the command names manifest {manifest}, but the admitted manifest is {admitted}",
    ),
    entry(
        "function.invocation.mutable_input",
        Tier::T6,
        "input {name} is a mutable URL or object key; stage the bytes and pass their digest as a blob",
    ),
    entry(
        "function.invocation.non_canonical_text",
        Tier::T6,
        "an invocation string or key is not in NFC form; hashing normalizes it, so it would share an identity with a different value",
    ),
    entry(
        "function.invocation.non_scalar_input",
        Tier::T6,
        "input {name} is not a canonical scalar; pass structured data as a blob",
    ),
    entry(
        "function.invocation.secret_input",
        Tier::T6,
        "input {name} is or names a secret; credentials are capabilities, never inputs",
    ),
    entry(
        "function.invocation.unbounded_command",
        Tier::T6,
        "the command needs {ceiling}",
    ),
    entry(
        "function.manifest.blank_value",
        Tier::T2,
        "{member} must not be blank",
    ),
    entry(
        "function.manifest.conflicting_golden_runs",
        Tier::T2,
        "golden runs produced {count} different output digests; the key is quarantined",
    ),
    entry(
        "function.manifest.dependency_closure_mismatch",
        Tier::T2,
        "the dependency closure {detail}",
    ),
    entry(
        "function.manifest.duplicate_step",
        Tier::T2,
        "pipeline step {id} is declared twice",
    ),
    entry(
        "function.manifest.embedded_secret",
        Tier::T2,
        "{member} can carry a credential; credentials are bound by the supervisor, never part of a manifest",
    ),
    entry(
        "function.manifest.inadmissible_evidence",
        Tier::T2,
        "{profile} evidence {member}: {detail}",
    ),
    entry(
        "function.manifest.missing_dependency_closure",
        Tier::T2,
        "the transitive dependency/asset closure is required",
    ),
    entry(
        "function.manifest.missing_determinism_evidence",
        Tier::T2,
        "the {profile} claim needs evidence field {member}",
    ),
    entry(
        "function.manifest.mutable_identity",
        Tier::T2,
        "{what} {value} is not a content digest (sha256:<64 hex>); a tag, URI, or object key is never an identity",
    ),
    entry(
        "function.manifest.non_canonical_text",
        Tier::T2,
        "a manifest string or key is not in NFC form; hashing normalizes it, so it would share an identity with a different value",
    ),
    entry(
        "function.manifest.profile_mismatch",
        Tier::T2,
        "{profile} {detail}",
    ),
    entry(
        "function.manifest.unbounded_resource",
        Tier::T2,
        "bound {member} must be a positive number",
    ),
    entry(
        "function.manifest.uncacheable_profile",
        Tier::T2,
        "profile {profile} is not qualified for content-based reuse; declare cache \"none\"",
    ),
    entry(
        "function.manifest.unsupported_abi",
        Tier::T2,
        "abi {abi} is not supported; expected norn.function-abi/1",
    ),
    entry(
        "function.receipt.attempt_out_of_range",
        Tier::T6,
        "attempt {attempt} is outside 1..={maxAttempts}",
    ),
    entry(
        "function.receipt.blank_value",
        Tier::T6,
        "{member} must not be blank",
    ),
    entry(
        "function.receipt.bound_exceeded",
        Tier::T6,
        "a completed attempt reports {member} {value}, beyond the manifest's bound",
    ),
    entry("function.receipt.cache_origin", Tier::T6, "{detail}"),
    entry(
        "function.receipt.dispatch_mismatch",
        Tier::T6,
        "{member} is {found}, but the dispatch pinned {expected}",
    ),
    entry(
        "function.receipt.non_canonical_text",
        Tier::T6,
        "a receipt string or key is not in NFC form; hashing normalizes it, so it would share an identity with a different value",
    ),
    entry(
        "function.receipt.output_manifest_mismatch",
        Tier::T6,
        "the output manifest {digest} does not match the outputs and summary ({computed})",
    ),
    entry(
        "function.receipt.summary_bound",
        Tier::T6,
        "the {what} holds {size} canonical bytes; at most 32768 cross the receipt",
    ),
    entry(
        "function.validation.invalid_document",
        Tier::T2,
        "the document is not a Function {record}: {detail}",
    ),
    entry(
        "graph_load.validation.excessive_depth",
        Tier::T6,
        "graph depth {depth} exceeds budget {budget}",
    ),
    entry(
        "graph_load.validation.invalid_shape",
        Tier::T6,
        "graph load shape is invalid: {detail}",
    ),
    entry(
        "graph_load.validation.missing_relation_kind",
        Tier::T6,
        "relationKind must be present",
    ),
    entry(
        "graph_load.validation.missing_scope",
        Tier::T6,
        "required scope '{key}' is missing",
    ),
    entry(
        "graph_load.validation.n_plus_one_risk",
        Tier::T6,
        "relation batch '{sourceKind}:{relationKind}' risks unbounded fan-out",
    ),
    entry(
        "graph_load.validation.over_budget",
        Tier::T6,
        "{measure} exceeds budget {budget}",
    ),
    entry(
        IDENTIFIER_TOO_LONG,
        Tier::T2,
        "{identifier} is {actualBytes} UTF-8 bytes; the admission limit is {limitBytes}; rename it before installing updated definitions",
    ),
    entry(
        "interface.validation.duplicate_link_id",
        Tier::T2,
        "required link id {link} is already a required link or property id",
    ),
    entry(
        "interface.validation.duplicate_name",
        Tier::T2,
        "interface {name} is already defined at index {index}",
    ),
    entry(
        "interface.validation.duplicate_property_id",
        Tier::T2,
        "required property id {property} is declared twice",
    ),
    entry(
        "interface.validation.invalid_inheritance",
        Tier::T3,
        "interface parent contract is repeated, missing, cyclic or incompatible",
    ),
    entry(
        "interface.validation.invalid_link_id",
        Tier::T2,
        "required link id {link} must match [a-z][a-z0-9_]* without the reserved extension separator \"__\"",
    ),
    entry(
        "interface.validation.invalid_name",
        Tier::T2,
        "interface name {name} must be dotted lower-case namespace segments and a PascalCase name, such as \"party.Party\"",
    ),
    entry(
        "interface.validation.invalid_property_id",
        Tier::T2,
        "required property id {property} must match [a-z][a-z0-9_]* without the reserved extension separator \"__\"",
    ),
    entry(
        "interface.validation.invalid_status_date",
        Tier::T2,
        "deprecated since {date}, which is not a calendar date \"YYYY-MM-DD\"",
    ),
    entry(
        "interface.validation.shadows_base",
        Tier::T3,
        "{name} is an installed base definition, which is read-only",
    ),
    entry(
        "interface.validation.tenant_interface",
        Tier::T2,
        "interface {name} is declared by a tenant layer; interfaces are registry definitions a tenant implements, never authors",
    ),
    entry(
        "interface.validation.unknown_link_target",
        Tier::T3,
        "required link {link} targets {target}, which names no object type or interface",
    ),
    entry(
        "interface.validation.unknown_replacement",
        Tier::T3,
        "replacedBy {name} must name another definition of the same kind",
    ),
    entry(
        "ir.validation.invalid_artifact",
        Tier::T2,
        "artifact envelope is malformed: {detail}",
    ),
    entry(
        "ir.validation.invalid_dependency",
        Tier::T2,
        "dependency is malformed or self-referential: {detail}",
    ),
    entry(
        "ir.validation.invalid_envelope",
        Tier::T2,
        "artifact envelope does not decode: {error}",
    ),
    entry(
        "ir.validation.kind_mismatch",
        Tier::T2,
        "envelope kind '{kind}' does not match its payload root '{payloadKind}'",
    ),
    entry(
        "ir.validation.required_field",
        Tier::T2,
        "{path} is required",
    ),
    entry(
        "ir.validation.retired_kind",
        Tier::T2,
        "artifact kind '{kind}' is retired: {reason}",
    ),
    entry(
        "ir.validation.unknown_kind",
        Tier::T2,
        "artifact kind '{kind}' is not a known kind",
    ),
    entry(
        "ir.validation.unsupported_version",
        Tier::T2,
        "unsupported IR version '{irVersion}'",
    ),
    introduced(
        "law.model.closure_refused",
        Tier::T3,
        "whole-set law dependency closure refused: {detail}",
        "0.2.0",
    ),
    introduced(
        "law.model.predicate_type",
        Tier::T3,
        "law predicate does not type-check against the effective ZEN environment: {detail}",
        "0.2.0",
    ),
    introduced(
        "law.model.unknown_binding",
        Tier::T3,
        "law slot requires an admitted ontology binding and exact visible dependency: {detail}",
        "0.2.0",
    ),
    introduced(
        "law.model.weakens_inherited",
        Tier::T3,
        "existing law identities cannot be replaced; tightening adds a conjunctive identity",
        "0.2.0",
    ),
    introduced(
        "law.validation.invalid_document",
        Tier::T2,
        "law document is invalid: {detail}",
        "0.2.0",
    ),
    introduced(
        "law.validation.unsupported_temporal",
        Tier::T2,
        "law temporal shape is unsupported: {detail}",
        "0.2.0",
    ),
    introduced(
        "layout.validation.invalid_inline_field",
        Tier::T2,
        "legacy layout field is invalid: {detail}",
        "0.2.0",
    ),
    warning_introduced(
        LAYOUT_MERGE_CONFLICT,
        Tier::T2,
        "retained or explicit layout value at {path} took precedence over conflicting inline layout",
        "0.2.0",
    ),
    warning_introduced(
        "layout.validation.orphaned_entry",
        Tier::T2,
        "layout {kind} id {id} has no matching semantic entry and was pruned",
        "0.2.0",
    ),
    entry(
        "ledger.account.duplicate",
        Tier::T2,
        "duplicate account id `{accountId}`",
    ),
    entry(
        "ledger.account.duplicate_unit",
        Tier::T2,
        "account `{accountId}` declares unit `{unitId}` more than once",
    ),
    entry(
        "ledger.account.no_units",
        Tier::T2,
        "account `{accountId}` must admit at least one unit",
    ),
    entry(
        "ledger.account.unknown_unit",
        Tier::T2,
        "account `{accountId}` references unknown unit `{unitId}`",
    ),
    entry(
        "ledger.amount.bound_overflow",
        Tier::T2,
        "runtime coefficient for parameter `{parameterId}` overflows at a declared bound",
    ),
    entry(
        "ledger.amount.constant_scale_exceeds_result",
        Tier::T2,
        "constant scale {scale} exceeds result scale {resultScale}",
    ),
    entry(
        "ledger.amount.duplicate_parameter_term",
        Tier::T2,
        "parameter `{parameterId}` appears more than once; normalize it to one coefficient",
    ),
    entry(
        "ledger.amount.normalization_overflow",
        Tier::T2,
        "constant cannot be normalized to unit precision without i128 overflow",
    ),
    entry(
        "ledger.amount.result_scale_exceeds_unit",
        Tier::T2,
        "result scale {scale} exceeds unit precision {precision}",
    ),
    entry(
        "ledger.amount.result_scale_out_of_range",
        Tier::T2,
        "result scale {scale} exceeds maximum {max}",
    ),
    entry(
        "ledger.amount.term_scale_exceeds_result",
        Tier::T2,
        "parameter and coefficient scales overflow their representation",
    ),
    entry(
        "ledger.amount.unknown_parameter",
        Tier::T2,
        "unknown or invalid parameter `{parameterId}`",
    ),
    entry(
        "ledger.certificate.claim_mismatch",
        Tier::T6,
        "certificate claims are not in the deterministic checker result",
    ),
    entry(
        "ledger.certificate.hash_mismatch",
        Tier::T6,
        "certificate identity does not match its claims",
    ),
    entry(
        "ledger.certificate.pack_mismatch",
        Tier::T6,
        "certificate is not bound to the exact supplied ledger pack",
    ),
    entry(
        "ledger.certificate.version_mismatch",
        Tier::T6,
        "certificate, IR, or checker version does not match the active checker",
    ),
    entry(
        "ledger.commit.evidence_mismatch",
        Tier::T6,
        "commit evidence does not match the prepared posting: {detail}",
    ),
    entry(
        "ledger.commit.idempotency_mismatch",
        Tier::T6,
        "idempotency lookup returned a record from another scope or key",
    ),
    entry(
        "ledger.commit.owner_version_overflow",
        Tier::T6,
        "expected owner version cannot advance without overflow",
    ),
    entry(
        "ledger.commit.partial_evidence",
        Tier::T6,
        "commit evidence is partial: {detail}",
    ),
    entry(
        "ledger.identifier.empty",
        Tier::T2,
        "identifier must be non-empty and contain no surrounding whitespace",
    ),
    entry(
        "ledger.layer.duplicate",
        Tier::T2,
        "duplicate layer id `{layer}`",
    ),
    entry(
        "ledger.leg.account_unit_mismatch",
        Tier::T2,
        "account `{accountId}` does not admit unit `{unitId}`",
    ),
    entry(
        "ledger.leg.duplicate",
        Tier::T2,
        "duplicate leg id `{legId}`",
    ),
    entry(
        "ledger.leg.empty_layer",
        Tier::T2,
        "layer must be non-empty",
    ),
    entry(
        "ledger.leg.unknown_account",
        Tier::T2,
        "unknown account `{accountId}`",
    ),
    entry(
        "ledger.leg.unknown_layer",
        Tier::T2,
        "unknown layer `{layer}`",
    ),
    entry(
        "ledger.leg.unknown_unit",
        Tier::T2,
        "unknown unit `{unitId}`",
    ),
    entry(
        "ledger.numeric.invalid_integer",
        Tier::T2,
        "`{value}` is not a signed i128 decimal integer",
    ),
    entry(
        "ledger.numeric.non_canonical_integer",
        Tier::T2,
        "`{value}` is not the canonical decimal encoding `{canonical}`",
    ),
    entry(
        "ledger.ontology.empty_artifact_id",
        Tier::T2,
        "ontology artifactId must be non-empty and canonical",
    ),
    entry(
        "ledger.ontology.empty_artifact_version",
        Tier::T2,
        "ontology artifactVersion must be non-empty and canonical",
    ),
    entry(
        "ledger.ontology.invalid_artifact_sha256",
        Tier::T2,
        "ontology artifactSha256 must be exactly 64 lowercase hexadecimal characters",
    ),
    entry(
        "ledger.ontology.non_exact_artifact_id",
        Tier::T2,
        "ontology artifactId must be one exact identity without whitespace or wildcards",
    ),
    entry(
        "ledger.ontology.non_exact_artifact_version",
        Tier::T2,
        "ontology artifactVersion must be one exact version, not a range or wildcard",
    ),
    entry("ledger.pack.empty_id", Tier::T2, "packId must be non-empty"),
    entry(
        "ledger.pack.no_accounts",
        Tier::T2,
        "at least one posting account is required",
    ),
    entry(
        "ledger.pack.no_layers",
        Tier::T2,
        "at least one declared balance layer is required",
    ),
    entry(
        "ledger.pack.no_templates",
        Tier::T2,
        "at least one transaction template is required",
    ),
    entry(
        "ledger.pack.no_units",
        Tier::T2,
        "at least one conserved unit is required",
    ),
    entry(
        "ledger.pack.unsupported_ir_version",
        Tier::T2,
        "unsupported ledger pack IR version `{irVersion}`; expected `{expected}`",
    ),
    entry(
        "ledger.parameter.duplicate",
        Tier::T2,
        "duplicate parameter id `{parameterId}`",
    ),
    entry(
        "ledger.parameter.invalid_bounds",
        Tier::T2,
        "minUnscaled must be less than or equal to maxUnscaled",
    ),
    entry(
        "ledger.parameter.scale_out_of_range",
        Tier::T2,
        "parameter scale {scale} exceeds maximum {max}",
    ),
    entry(
        "ledger.posting.certificate_coverage_mismatch",
        Tier::T6,
        "certificate set does not exactly cover the evaluated unit/layer partitions",
    ),
    entry(
        "ledger.posting.contract_version_mismatch",
        Tier::T6,
        "unsupported posting contract version `{version}`; expected `{expected}`",
    ),
    entry(
        "ledger.posting.identity_empty",
        Tier::T6,
        "{path} must be non-empty and contain no surrounding whitespace",
    ),
    entry(
        "ledger.posting.reference_mismatch",
        Tier::T6,
        "posting references do not match the installed pack seal and checker",
    ),
    entry(
        "ledger.runtime.duplicate_parameter",
        Tier::T6,
        "duplicate runtime parameter `{parameterId}`",
    ),
    entry(
        "ledger.runtime.evaluation_overflow",
        Tier::T6,
        "coefficient multiplication overflowed for parameter `{parameterId}`",
    ),
    entry(
        "ledger.runtime.missing_parameter",
        Tier::T6,
        "missing runtime parameter `{parameterId}`",
    ),
    entry(
        "ledger.runtime.parameter_out_of_bounds",
        Tier::T6,
        "runtime parameter `{parameterId}` value {value} is outside sealed bounds [{min}, {max}]",
    ),
    entry(
        "ledger.runtime.parameter_scale_mismatch",
        Tier::T6,
        "runtime parameter `{parameterId}` has scale {scale}; expected {expected}",
    ),
    entry(
        "ledger.runtime.unbalanced",
        Tier::T6,
        "runtime residual {residual} for unit `{unitId}` layer `{layer}`",
    ),
    entry(
        "ledger.runtime.unknown_parameter",
        Tier::T6,
        "unknown runtime parameter `{parameterId}`",
    ),
    entry(
        "ledger.runtime.unknown_template",
        Tier::T6,
        "unknown compiled template `{templateId}`",
    ),
    entry(
        "ledger.template.duplicate",
        Tier::T2,
        "duplicate template id `{templateId}`",
    ),
    entry(
        "ledger.template.residual_overflow",
        Tier::T2,
        "constant residual overflowed i128 while combining legs",
    ),
    entry(
        "ledger.template.too_few_legs",
        Tier::T2,
        "a conserved transaction template requires at least two legs",
    ),
    entry(
        "ledger.template.unbalanced_coefficient",
        Tier::T2,
        "template `{templateId}` has coefficient residual {coefficient} for parameter `{parameterId}`, unit `{unitId}`, layer `{layer}`",
    ),
    entry(
        "ledger.template.unbalanced_constant",
        Tier::T2,
        "template `{templateId}` has constant residual {residual} for unit `{unitId}` layer `{layer}`",
    ),
    entry(
        "ledger.unit.duplicate",
        Tier::T2,
        "duplicate unit id `{unitId}`",
    ),
    entry(
        "ledger.unit.duplicate_unit_term",
        Tier::T2,
        "ontology unit term `{term}` is bound by more than one pack-local unit",
    ),
    entry(
        "ledger.unit.empty_quantity_kind_term",
        Tier::T2,
        "quantityKindTermId must be non-empty and canonical",
    ),
    entry(
        "ledger.unit.empty_unit_term",
        Tier::T2,
        "unitTermId must be non-empty and canonical",
    ),
    entry(
        "ledger.unit.inconsistent_dimension_binding",
        Tier::T2,
        "a unit term cannot also be its quantity-kind/dimension term",
    ),
    entry(
        "ledger.unit.invalid_quantity_kind_term",
        Tier::T2,
        "quantityKindTermId must be one exact term identity without whitespace or wildcards",
    ),
    entry(
        "ledger.unit.invalid_unit_term",
        Tier::T2,
        "unitTermId must be one exact term identity without whitespace or wildcards",
    ),
    entry(
        "ledger.unit.scale_out_of_range",
        Tier::T2,
        "unit precision {precision} exceeds maximum {max}",
    ),
    entry(
        "ledger.validation.too_many_diagnostics",
        Tier::T2,
        "ledger validation stopped at its diagnostic count or byte limit",
    ),
    entry(
        "lifecycle.validation.dead_end_state",
        Tier::T2,
        "state {state} has no outgoing transition; declare one or list it as terminal",
    ),
    entry(
        "lifecycle.validation.default_not_initial",
        Tier::T2,
        "the default {state} of {property} is not an initial state",
    ),
    entry(
        "lifecycle.validation.drops_guard",
        Tier::T3,
        "transition {id} drops the inherited guard {guard}; a subtype restates every inherited guard and may only append its own",
    ),
    entry(
        "lifecycle.validation.duplicate_lifecycle",
        Tier::T2,
        "property {property} already has a lifecycle on this type",
    ),
    entry(
        "lifecycle.validation.duplicate_state",
        Tier::T2,
        "state {state} is listed twice in {member}",
    ),
    entry(
        "lifecycle.validation.duplicate_transition",
        Tier::T2,
        "{from} -> {to} is already declared by another transition",
    ),
    entry(
        "lifecycle.validation.duplicate_transition_id",
        Tier::T2,
        "transition id {id} is declared twice in this lifecycle",
    ),
    entry(
        "lifecycle.validation.empty_guard",
        Tier::T2,
        "a guard of transition {id} is blank",
    ),
    entry(
        "lifecycle.validation.empty_initial",
        Tier::T2,
        "a lifecycle declares at least one initial state",
    ),
    entry(
        "lifecycle.validation.empty_transition_source",
        Tier::T2,
        "transition {id} has no source state",
    ),
    entry(
        "lifecycle.validation.invalid_guard",
        Tier::T2,
        "guard {index} of transition {id} is not a ZEN Expression: {error}",
    ),
    entry(
        "lifecycle.validation.invalid_transition_id",
        Tier::T2,
        "transition id {id} must match [a-z][a-z0-9_]*",
    ),
    entry(
        "lifecycle.validation.path_property_violated",
        Tier::T2,
        "a declared path to {target} avoids {via}; structural trace [transition, state]: {trace}",
    ),
    entry(
        "lifecycle.validation.property_computed",
        Tier::T2,
        "lifecycle property {property} is computed; a lifecycle governs a stored property that rules write",
    ),
    entry(
        "lifecycle.validation.property_not_enum",
        Tier::T2,
        "lifecycle property {property} must be a local enum; its values are the states",
    ),
    entry(
        "lifecycle.validation.property_nullable",
        Tier::T2,
        "lifecycle property {property} is nullable; every object is in a state",
    ),
    entry(
        "lifecycle.validation.self_transition",
        Tier::T2,
        "transition {id} leaves and enters {state}; a transition changes the state",
    ),
    entry(
        "lifecycle.validation.terminal_has_transition",
        Tier::T2,
        "transition {id} leaves the terminal state {state}",
    ),
    entry(
        "lifecycle.validation.undeclared_write",
        Tier::T3,
        "{writer} writes {property} from {from} to {to}, which is not a declared transition of its lifecycle",
    ),
    entry(
        "lifecycle.validation.unknown_property",
        Tier::T2,
        "lifecycle property {property} is not a property of the type",
    ),
    entry(
        "lifecycle.validation.unknown_state",
        Tier::T2,
        "{state} is not a value of the lifecycle property's enum",
    ),
    entry(
        "lifecycle.validation.unreachable_state",
        Tier::T2,
        "state {state} is not reachable from an initial state",
    ),
    entry(
        "lifecycle.validation.widens_initial",
        Tier::T3,
        "initial state {state} is not an initial state of the inherited lifecycle",
    ),
    entry(
        "lifecycle.validation.widens_transition",
        Tier::T3,
        "transition {id} is not a transition of the inherited lifecycle; a subtype may only drop transitions",
    ),
    entry(
        "mapping.model.expression_type",
        Tier::T3,
        "{expression}: {detail} ({code})",
    ),
    entry(
        "mapping.model.interface_not_implemented",
        Tier::T3,
        "{objectType} does not implement the interface artifact {interface}",
    ),
    introduced(
        "mapping.model.invalid_entitlement_condition",
        Tier::T3,
        "condition link {link} must target {expectedTarget}, got {actualTarget}",
        "0.2.0",
    ),
    introduced(
        "mapping.model.invalid_entitlement_grantee",
        Tier::T3,
        "entitlement grantee kind and object type must match a canonical identity type",
        "0.2.0",
    ),
    introduced(
        "mapping.model.invalid_entitlement_join",
        Tier::T3,
        "entitlement join {join} has an invalid alias, key, provenance, or field type",
        "0.2.0",
    ),
    entry(
        "mapping.model.invalid_nesting",
        Tier::T3,
        "dataset {dataset} is not a declared child table of {parent} held in its field {field}",
    ),
    entry(
        "mapping.model.invalid_on_delete",
        Tier::T3,
        "onDelete {state} of {property} is not a terminal state of {objectType}'s lifecycle",
    ),
    entry(
        "mapping.model.lifecycle_member",
        Tier::T3,
        "{property} of {objectType}: a lifecycle property changes only through a lifecycle rule",
    ),
    entry(
        "mapping.model.link_target_mismatch",
        Tier::T3,
        "link {link} of {objectType} targets {target}, which does not admit {produced}",
    ),
    entry(
        "mapping.model.read_only_member",
        Tier::T3,
        "{property} of {objectType} is computed and read-only",
    ),
    entry(
        "mapping.model.source_hash_mismatch",
        Tier::T3,
        "source schema {schemaId} version {version} hashes to {actual}, not the bound {sha256}",
    ),
    entry(
        "mapping.model.undeclared_dependency",
        Tier::T3,
        "artifact reference {artifactId} is not a required, exact-version dependency",
    ),
    entry(
        "mapping.model.unencodable_member",
        Tier::T3,
        "property {property} of {objectType} has no action-input encoding",
    ),
    entry(
        "mapping.model.unknown_dataset",
        Tier::T3,
        "dataset {dataset} is not declared by the source schema",
    ),
    introduced(
        "mapping.model.unknown_entitlement_field",
        Tier::T3,
        "entitlement mapping references an undeclared or unbound source field",
        "0.2.0",
    ),
    entry(
        "mapping.model.unknown_field",
        Tier::T3,
        "dataset {dataset} has no field {field}",
    ),
    entry(
        "mapping.model.unknown_interface",
        Tier::T3,
        "{artifactId} is not an entity artifact defining an interface",
    ),
    entry(
        "mapping.model.unknown_member",
        Tier::T3,
        "{objectType} has no {what} {member}",
    ),
    entry(
        "mapping.model.unknown_object_type",
        Tier::T3,
        "{entity} is not an entity artifact defining an available object type",
    ),
    entry(
        "mapping.model.unresolved_source",
        Tier::T3,
        "source schema {schemaId} version {version} is not a source_schema artifact of the set or the base",
    ),
    entry(
        "mapping.runtime.evaluation_failed",
        Tier::T6,
        "map {map}: {detail}",
    ),
    entry(
        "mapping.runtime.invalid_key",
        Tier::T6,
        "map {map}: {expression} yielded {value}, not a non-empty string",
    ),
    entry(
        "mapping.runtime.invalid_record",
        Tier::T6,
        "map {map}: field {field} {reason}",
    ),
    warning(
        "mapping.runtime.observed_out_of_band",
        Tier::T6,
        "the observed lifecycle snapshot from {source} for {objectType}/{key}.{property} ({from} -> {to}) has no unique shortest declared path; mappingVersion={mappingVersion}, observedAt={observedAt}, reason={reason}",
    ),
    entry(
        "mapping.validation.duplicate_map",
        Tier::T2,
        "map id {id} is declared twice",
    ),
    entry(
        "mapping.validation.duplicate_member",
        Tier::T2,
        "map {id} has two rules for member {member}",
    ),
    entry(
        "mapping.validation.empty_scope",
        Tier::T2,
        "{member} must not be empty",
    ),
    introduced(
        "mapping.validation.incomplete_entitlement_snapshot",
        Tier::T2,
        "entitlement mappings require every source and join dataset in the complete snapshot",
        "0.2.0",
    ),
    entry(
        "mapping.validation.interface_out_of_scope",
        Tier::T2,
        "map {id} targets interface artifact {interface}, which is not in the mapping's interfaces",
    ),
    entry(
        "mapping.validation.invalid_document",
        Tier::T2,
        "the document is not a MappingIR document: {detail}",
    ),
    introduced(
        "mapping.validation.invalid_entitlement_map",
        Tier::T2,
        "entitlement map {map} has an invalid declaration",
        "0.2.0",
    ),
    entry(
        "mapping.validation.invalid_expression",
        Tier::T2,
        "{expression} does not compile: {detail}",
    ),
    entry(
        "mapping.validation.invalid_id",
        Tier::T2,
        "map id {id} is not a member id",
    ),
    introduced(
        "mapping.validation.invalid_identity_binding",
        Tier::T2,
        "identity binding needs a unique source key, party.Person reference, and review evidence",
        "0.2.0",
    ),
    entry(
        "mapping.validation.invalid_key",
        Tier::T3,
        "mapping id {artifactId} contains '@' or version {version} is not SemVer, so the key {key} is ambiguous",
    ),
    introduced(
        "mapping.validation.invalid_required_link",
        Tier::T6,
        "required relationship {link} has no exact target key",
        "0.2.0",
    ),
    entry(
        "mapping.validation.lifecycle_rule_shape",
        Tier::T2,
        "map {id} lifecycle {property} must specify exactly one of state or transition",
    ),
    entry(
        "mapping.validation.unknown_target_map",
        Tier::T2,
        "link {link} joins to map {map}, which the mapping does not declare",
    ),
    entry(
        "mapping.vendor.invalid_contract",
        Tier::T2,
        "vendor-qualified mapping has an invalid sparse source contract: {detail}",
    ),
    introduced(
        "metric.validation.complexity_limit",
        Tier::T3,
        "Metric dependencies and link paths are bounded to 64 levels.",
        "0.2.0",
    ),
    introduced(
        "metric.validation.currency_source_mismatch",
        Tier::T3,
        "Metric currency must match the amount type's authoritative currency source.",
        "0.2.0",
    ),
    introduced(
        "metric.validation.cycle",
        Tier::T3,
        "Ratio dependencies must be acyclic.",
        "0.2.0",
    ),
    introduced(
        "metric.validation.duplicate_metric",
        Tier::T3,
        "Metric names must be unique.",
        "0.2.0",
    ),
    introduced(
        "metric.validation.fan_out",
        Tier::T3,
        "To-many dimensions require a symmetric aggregation strategy.",
        "0.2.0",
    ),
    introduced(
        "metric.validation.grain_mismatch",
        Tier::T3,
        "Ratio operands must share their object and time grain.",
        "0.2.0",
    ),
    introduced(
        "metric.validation.import_gap",
        Tier::T3,
        "Interchange import requires explicit ontology or calculation semantics.",
        "0.2.0",
    ),
    introduced(
        "metric.validation.invalid_additivity",
        Tier::T3,
        "Additivity must match the measure algebra.",
        "0.2.0",
    ),
    introduced(
        "metric.validation.invalid_conversion",
        Tier::T3,
        "Conversion requires a target currency and a dimensionless decimal rate.",
        "0.2.0",
    ),
    introduced(
        "metric.validation.invalid_currency",
        Tier::T3,
        "Currency must bind a single-valued currency property.",
        "0.2.0",
    ),
    introduced(
        "metric.validation.invalid_dimension",
        Tier::T3,
        "Dimensions require a scalar property and compatible grain.",
        "0.2.0",
    ),
    introduced(
        "metric.validation.invalid_filter",
        Tier::T3,
        "The filter literal and operator must match the property contract.",
        "0.2.0",
    ),
    introduced(
        "metric.validation.invalid_measure",
        Tier::T3,
        "The aggregate requires a compatible property.",
        "0.2.0",
    ),
    introduced(
        "metric.validation.invalid_name",
        Tier::T3,
        "Metric names must be lower-case identifiers, optionally namespaced.",
        "0.2.0",
    ),
    introduced(
        "metric.validation.invalid_owner",
        Tier::T3,
        "Metrics require an explicit non-blank owner.",
        "0.2.0",
    ),
    introduced(
        "metric.validation.invalid_policy",
        Tier::T3,
        "Norn authorization or freshness policy is invalid.",
        "0.2.0",
    ),
    introduced(
        "metric.validation.invalid_time",
        Tier::T3,
        "The time axis must be a single-valued date or timestamp.",
        "0.2.0",
    ),
    introduced(
        "metric.validation.mixed_currency",
        Tier::T3,
        "Summing multiple currencies requires an explicit conversion.",
        "0.2.0",
    ),
    introduced(
        "metric.validation.ontology_projection",
        Tier::T3,
        "Generated semantic datasets or relationships differ from the ontology.",
        "0.2.0",
    ),
    introduced(
        "metric.validation.partial_path",
        Tier::T3,
        "Mandatory time, currency and rate inputs require total paths.",
        "0.2.0",
    ),
    introduced(
        "metric.validation.unit_mismatch",
        Tier::T3,
        "Ratio operand units and currencies must agree.",
        "0.2.0",
    ),
    introduced(
        "metric.validation.unknown_metric",
        Tier::T3,
        "The referenced metric does not exist.",
        "0.2.0",
    ),
    introduced(
        "metric.validation.unknown_object",
        Tier::T3,
        "The measure object type is unavailable.",
        "0.2.0",
    ),
    introduced(
        "metric.validation.unknown_path",
        Tier::T3,
        "Every path role and terminal property must resolve.",
        "0.2.0",
    ),
    introduced(
        "metric.validation.unsupported_semantics",
        Tier::T3,
        "Calculation requires semantics outside the admitted checker profile.",
        "0.2.0",
    ),
    entry(
        "object_type.validation.computed_key_component",
        Tier::T2,
        "key component {property} is computed; identity is stored, not derived",
    ),
    entry(
        "object_type.validation.computed_with_default",
        Tier::T2,
        "computed property {property} is read-only and cannot declare a default",
    ),
    entry(
        "object_type.validation.default_out_of_range",
        Tier::T2,
        "default {value} is outside the {range} range",
    ),
    entry(
        "object_type.validation.default_type_mismatch",
        Tier::T2,
        "default for a {kind} property must be {expected}",
    ),
    entry(
        "object_type.validation.default_unchecked",
        Tier::T2,
        "default cannot be type-checked: {reason}; omit it",
    ),
    entry(
        "object_type.validation.default_violates_constraint",
        Tier::T2,
        "default {value} violates {constraint}",
    ),
    entry(
        "object_type.validation.duplicate_interface",
        Tier::T2,
        "interface {interface} is listed twice",
    ),
    entry(
        "object_type.validation.duplicate_invariant_id",
        Tier::T2,
        "invariant id {invariant} is already declared on the type or a supertype; an invariant is never replaced",
    ),
    entry(
        "object_type.validation.duplicate_key_component",
        Tier::T2,
        "key component {property} is listed twice",
    ),
    entry(
        "object_type.validation.duplicate_key_name",
        Tier::T2,
        "key {key} is declared twice",
    ),
    entry(
        "object_type.validation.duplicate_link_id",
        Tier::T2,
        "link id {link} is declared twice, as a link or a property, in one declaration",
    ),
    entry(
        "object_type.validation.duplicate_name",
        Tier::T2,
        "object type {name} is already defined at index {index}",
    ),
    entry(
        "object_type.validation.duplicate_normalization",
        Tier::T2,
        "component {property} is normalized twice",
    ),
    entry(
        "object_type.validation.duplicate_property_id",
        Tier::T2,
        "property id {property} is declared twice",
    ),
    entry(
        "object_type.validation.empty_computed",
        Tier::T2,
        "computed expression is blank",
    ),
    entry(
        "object_type.validation.empty_identity_rule",
        Tier::T2,
        "identity rule of key {key} declares neither normalize nor matchPolicy; omit it",
    ),
    entry(
        "object_type.validation.empty_invariant_expression",
        Tier::T2,
        "the expression of invariant {invariant} is blank",
    ),
    entry(
        "object_type.validation.empty_invariant_message",
        Tier::T2,
        "the message of invariant {invariant} is blank",
    ),
    entry(
        "object_type.validation.empty_key",
        Tier::T2,
        "key {key} declares no properties",
    ),
    entry(
        "object_type.validation.empty_normalization",
        Tier::T2,
        "normalization of {property} declares no steps",
    ),
    entry(
        "object_type.validation.extension_namespace_mismatch",
        Tier::T2,
        "extension namespace {namespace} is not the tenant namespace {tenant}",
    ),
    entry(
        "object_type.validation.extension_of_local_type",
        Tier::T3,
        "extension target {type} is defined in the same layer; declare its properties on the type itself",
    ),
    entry(
        "object_type.validation.extension_shadows_member",
        Tier::T3,
        "extension {member} {id} shadows the member of the same id that {owner} declares on {type}",
    ),
    entry(
        "object_type.validation.foreign_inverse",
        Tier::T3,
        "inverse role {id} would add a member to {target}, which another layer owns; declare the link in an extension so its inverse role is namespaced, or omit the inverse",
    ),
    entry(
        "object_type.validation.inadmissible_key_kind",
        Tier::T2,
        "key component {property} is {kind}, which has no canonical equality",
    ),
    entry(
        "object_type.validation.inheritance_cycle",
        Tier::T3,
        "the extends chain of {type} returns to it",
    ),
    entry(
        "object_type.validation.interface_link_target_mismatch",
        Tier::T3,
        "link {link} does not target {target}, which interface {interface} requires, or a narrowing of it",
    ),
    entry(
        "object_type.validation.interface_link_widens_cardinality",
        Tier::T3,
        "link {link} has a wider cardinality than interface {interface} requires",
    ),
    entry(
        "object_type.validation.interface_property_type_mismatch",
        Tier::T3,
        "property {property} does not have the type interface {interface} requires, or a narrowing of it",
    ),
    entry(
        "object_type.validation.interface_property_widens_nullability",
        Tier::T3,
        "property {property} is nullable, but interface {interface} requires a value",
    ),
    entry(
        "object_type.validation.interface_requirement_conflict",
        Tier::T3,
        "interfaces {interfaces} all require member {id}, but no single member can meet them together: {reason}",
    ),
    entry(
        "object_type.validation.invalid_accounting_leg_declaration",
        Tier::T3,
        "accounting leg declaration on {type} is invalid: {reason}",
    ),
    entry(
        "object_type.validation.invalid_computed",
        Tier::T2,
        "computed expression of {property} is not a ZEN Expression: {error}",
    ),
    entry(
        "object_type.validation.invalid_extension_namespace",
        Tier::T2,
        "extension namespace {namespace} must be one lower-case alphanumeric segment ([a-z][a-z0-9]*)",
    ),
    entry(
        "object_type.validation.invalid_invariant_expression",
        Tier::T2,
        "the expression of invariant {invariant} is not a ZEN Expression: {error}",
    ),
    entry(
        "object_type.validation.invalid_invariant_id",
        Tier::T2,
        "invariant id {invariant} must match [a-z][a-z0-9_]*",
    ),
    entry(
        "object_type.validation.invalid_key_name",
        Tier::T2,
        "key name {key} must match [a-z][a-z0-9_]*",
    ),
    entry(
        "object_type.validation.invalid_link_id",
        Tier::T2,
        "{role} {id} must match [a-z][a-z0-9_]*",
    ),
    entry(
        "object_type.validation.invalid_match_policy",
        Tier::T2,
        "match policy {policy} must be dot-separated segments of [a-z][a-z0-9_]*",
    ),
    entry(
        "object_type.validation.invalid_name",
        Tier::T2,
        "object type name {name} must be dotted lower-case namespace segments and a PascalCase name, such as \"retail.Sku\"",
    ),
    entry(
        "object_type.validation.invalid_naming_series",
        Tier::T2,
        "naming series is invalid: {detail}",
    ),
    entry(
        "object_type.validation.invalid_owned_collection",
        Tier::T3,
        "owned collection {collection} is invalid: {reason}",
    ),
    entry(
        "object_type.validation.invalid_property_id",
        Tier::T2,
        "property id {property} must match [a-z][a-z0-9_]*",
    ),
    entry(
        "object_type.validation.invalid_status_date",
        Tier::T2,
        "deprecated since {date}, which is not a calendar date \"YYYY-MM-DD\"",
    ),
    entry(
        "object_type.validation.link_override_changes_acyclic",
        Tier::T3,
        "link {link} overrides the one {owner} declares and changes its acyclic constraint; an override must preserve the inherited value",
    ),
    entry(
        "object_type.validation.link_override_redeclares",
        Tier::T3,
        "link {link} overrides the one {owner} declares and redeclares its {member}, which the override inherits; omit it",
    ),
    entry(
        "object_type.validation.link_override_target_mismatch",
        Tier::T3,
        "link {link} overrides the one {owner} declares with a target that is not {target} or a narrowing of it; an override may only narrow",
    ),
    entry(
        "object_type.validation.link_override_widens_cardinality",
        Tier::T3,
        "link {link} overrides the one {owner} declares with a wider cardinality; an override may only narrow (many to one, optional to required)",
    ),
    entry(
        "object_type.validation.link_property_markings_unsupported",
        Tier::T3,
        "marked link properties require edge classification, which is not supported",
    ),
    entry(
        "object_type.validation.member_id_conflict",
        Tier::T3,
        "{member} {id} takes the id of the {other} on {type}; properties, links, and inverse roles share one member name space",
    ),
    entry(
        "object_type.validation.missing_interface_link",
        Tier::T3,
        "interface {interface} requires link {link}, which the type neither declares nor inherits",
    ),
    entry(
        "object_type.validation.missing_interface_property",
        Tier::T3,
        "interface {interface} requires property {property}, which the type neither declares nor inherits",
    ),
    entry(
        "object_type.validation.normalization_not_applicable",
        Tier::T2,
        "normalization applies to text, but component {property} is {kind}",
    ),
    entry(
        "object_type.validation.normalization_not_in_key",
        Tier::T2,
        "normalization names {property}, which is not a component of key {key}",
    ),
    entry(
        "object_type.validation.nullable_key_component",
        Tier::T2,
        "key component {property} is nullable",
    ),
    entry(
        "object_type.validation.override_computed",
        Tier::T3,
        "property {property} overrides the one {owner} declares; a computed property is never overridden, and an override is never computed",
    ),
    entry(
        "object_type.validation.override_type_mismatch",
        Tier::T3,
        "property {property} overrides the one {owner} declares with a type that is not the same or narrower; an override may only narrow",
    ),
    entry(
        "object_type.validation.override_widens_nullability",
        Tier::T3,
        "property {property} is nullable, but the property {owner} declares is not; an override may only narrow",
    ),
    entry(
        "object_type.validation.redundant_key",
        Tier::T2,
        "key {key} has the same components, normalization, and match policy as key {other}; component order does not change uniqueness",
    ),
    entry(
        "object_type.validation.reserved_member_separator",
        Tier::T2,
        "{member} id {id} contains \"__\", which is reserved for extension members (\"<namespace>__<name>\")",
    ),
    entry(
        "object_type.validation.shadows_base",
        Tier::T3,
        "{name} is an installed base definition, which is read-only; extend it or add a subtype instead",
    ),
    entry(
        "object_type.validation.tenant_type_not_subtype",
        Tier::T2,
        "tenant object type {type} must extend a type: a tenant adds subtypes of installed types, never new base types",
    ),
    entry(
        "object_type.validation.tenant_type_outside_namespace",
        Tier::T2,
        "tenant object type {type} must be named in the tenant namespace, such as {example}",
    ),
    entry(
        "object_type.validation.unknown_extension_target",
        Tier::T3,
        "extension target {type} names no object type",
    ),
    entry(
        "object_type.validation.unknown_interface",
        Tier::T3,
        "implements {interface}, which names no interface",
    ),
    entry(
        "object_type.validation.unknown_key_property",
        Tier::T2,
        "key component {property} names no property of the object type",
    ),
    entry(
        "object_type.validation.unknown_link_target",
        Tier::T3,
        "link target {target} names no object type or interface",
    ),
    entry(
        "object_type.validation.unknown_replacement",
        Tier::T3,
        "replacedBy {name} must name another definition of the same kind",
    ),
    entry(
        "object_type.validation.unknown_supertype",
        Tier::T3,
        "extends {type}, which names no object type",
    ),
    entry(
        "object_type.validation.unnamespaced_member",
        Tier::T2,
        "added {member} {id} must be named \"{namespace}__<name>\" in its layer's extension namespace, so it cannot shadow a base member",
    ),
    entry(
        "object_type.validation.unsatisfiable_constraints",
        Tier::T3,
        "the constraints that apply to property {property} together admit no value: {reason}",
    ),
    warning(
        "ontology.lint.deep_hierarchy",
        Tier::T3,
        "object type {name} has {depth} supertypes ({chain}), more than {max}; prefer interfaces or composition",
    ),
    entry(
        "ontology.lint.duplicate_definition",
        Tier::T3,
        "{kind} {name} has exactly the structure of {kind} {other} (artifact {artifactId}); reuse or extend it instead of redefining it",
    ),
    warning(
        "ontology.lint.incomplete_provenance",
        Tier::T3,
        "the provenance of this {source} artifact is missing {members}",
    ),
    warning(
        "ontology.lint.mega_object",
        Tier::T3,
        "object type {name} declares {count} properties and links, more than {max}; split it into the concepts it combines",
    ),
    warning(
        "ontology.lint.plural_type_name",
        Tier::T3,
        "{kind} {name} has a plural local name; name the concept for one instance",
    ),
    warning(
        "ontology.lint.redundant_member_prefix",
        Tier::T3,
        "member {id} of {name} repeats the type name as a prefix; the type already qualifies it",
    ),
    warning(
        "ontology.lint.source_mirroring",
        Tier::T3,
        "object type {name} mirrors source entity {entity} of {schemaId}: {shared} of its {members} members match the {fields} fields of {entity} by name; model the business concept and map the source onto it",
    ),
    entry(
        "ontology.lint.unknown_mapping_target",
        Tier::T3,
        "the mapping targets {target}, which the artifact set and the installed base do not declare",
    ),
    entry(
        "ontology.lint.unmapped_required_member",
        Tier::T3,
        "required {memberKind} {id} of {target} is neither mapped nor declared as a gap",
    ),
    warning(
        "ontology.lint.unmapped_source_field",
        Tier::T3,
        "source fields of {entity} in {schemaId} are neither mapped nor explicitly ignored: {fields}",
    ),
    entry(
        "ontology.lint.untraceable_provenance",
        Tier::T3,
        "this {source} artifact names no provenance {member}, so its origin cannot be traced",
    ),
    warning(
        "ontology.lint.versioned_name",
        Tier::T3,
        "{kind} {name} carries the version or copy marker {marker}; version the artifact, not the name",
    ),
    warning(
        "ontology.lint.weak_relationship",
        Tier::T3,
        "property {id} of {name} refers to {target} by a scalar value, and no link to {target} exists; model the relationship as a link",
    ),
    entry(
        "ontology_instance.validation.invalid_payload",
        Tier::T6,
        "ontology instance does not conform to its term at {path}: {detail}",
    ),
    entry(
        "ontology_pack.validation.invalid_payload",
        Tier::T2,
        "ontology pack is invalid at {path}: {detail}",
    ),
    entry(
        "ontology_registry.validation.invalid_payload",
        Tier::T3,
        "ontology registry is inconsistent at {path}: {detail}",
    ),
    entry(
        "policy.validation.invalid_payload",
        Tier::T2,
        "PolicyIR payload is invalid at {path}: {detail}",
    ),
    entry(
        "property_type.validation.bound_exceeds_scale",
        Tier::T2,
        "{member} {bound} has more fraction digits than the declared scale {scale}",
    ),
    entry(
        "property_type.validation.bound_not_integer",
        Tier::T2,
        "{member} {bound} must be a signed 64-bit integer without a fraction",
    ),
    entry(
        "property_type.validation.bound_out_of_range",
        Tier::T2,
        "{member} {bound} is outside the ZEN Expression decimal range: {error}",
    ),
    entry(
        "property_type.validation.constraint_not_applicable",
        Tier::T2,
        "constraint {member} does not apply to a {kind} property",
    ),
    entry(
        "property_type.validation.duplicate_enum_value",
        Tier::T2,
        "enum value {value} is declared twice",
    ),
    entry(
        "property_type.validation.duplicate_member",
        Tier::T2,
        "record member {member} is declared twice",
    ),
    entry(
        "property_type.validation.duplicate_name",
        Tier::T2,
        "property type {name} is already defined at index {index}",
    ),
    entry(
        "property_type.validation.empty_enum",
        Tier::T2,
        "enum declares no values",
    ),
    entry(
        "property_type.validation.empty_record",
        Tier::T2,
        "record must declare members",
    ),
    entry(
        "property_type.validation.invalid_bound",
        Tier::T2,
        "{member} {bound} must be an exact decimal literal such as \"-12.50\"",
    ),
    entry(
        "property_type.validation.invalid_enum_value",
        Tier::T2,
        "enum value must be non-blank",
    ),
    entry(
        "property_type.validation.invalid_member",
        Tier::T2,
        "record member must match [a-z][a-z0-9_]*",
    ),
    entry(
        "property_type.validation.invalid_name",
        Tier::T2,
        "property type name {name} must be dot-separated segments of [a-z][a-z0-9_]*",
    ),
    entry(
        "property_type.validation.invalid_pattern",
        Tier::T2,
        "pattern {pattern} does not compile: {error}",
    ),
    entry(
        "property_type.validation.invalid_unit",
        Tier::T2,
        "unit code {unit} must be non-blank without whitespace",
    ),
    entry(
        "property_type.validation.inverted_range",
        Tier::T2,
        "{lower} is greater than {upper}",
    ),
    entry(
        "property_type.validation.ref_cycle",
        Tier::T2,
        "ref chain from {name} returns to itself",
    ),
    entry(
        "property_type.validation.scale_out_of_range",
        Tier::T2,
        "decimal scale {scale} exceeds the maximum of {max}",
    ),
    entry(
        "property_type.validation.unknown_ref",
        Tier::T2,
        "ref {name} names no property type in the catalog or registry",
    ),
    introduced(
        "question.validation.comparison_cycle",
        Tier::T3,
        "Question comparisons must be acyclic.",
        "0.2.0",
    ),
    introduced(
        "question.validation.duplicate_question",
        Tier::T3,
        "Question names must be unique.",
        "0.2.0",
    ),
    introduced(
        "question.validation.duplicate_selection",
        Tier::T3,
        "Metric and dimension selections must be unique.",
        "0.2.0",
    ),
    introduced(
        "question.validation.empty_metrics",
        Tier::T3,
        "Questions require at least one metric.",
        "0.2.0",
    ),
    introduced(
        "question.validation.fan_out_rollup",
        Tier::T3,
        "Overlapping fan-out buckets cannot be rolled up as scalars.",
        "0.2.0",
    ),
    introduced(
        "question.validation.invalid_freshness",
        Tier::T3,
        "Freshness requires a positive age or a valid UTC instant.",
        "0.2.0",
    ),
    introduced(
        "question.validation.invalid_name",
        Tier::T3,
        "Question names must be lower-case identifiers, optionally namespaced.",
        "0.2.0",
    ),
    introduced(
        "question.validation.invalid_rollup",
        Tier::T3,
        "Roll-ups must name selected axes and use an admitted reduction.",
        "0.2.0",
    ),
    introduced(
        "question.validation.invalid_window",
        Tier::T3,
        "The time window must be an ordered half-open UTC interval.",
        "0.2.0",
    ),
    introduced(
        "question.validation.mixed_axis_rollup",
        Tier::T3,
        "Simultaneous time and non-time reductions require an ordered reduction contract.",
        "0.2.0",
    ),
    introduced(
        "question.validation.non_additive_rollup",
        Tier::T3,
        "Non-additive metrics cannot be re-aggregated.",
        "0.2.0",
    ),
    introduced(
        "question.validation.semi_additive_time",
        Tier::T3,
        "Semi-additive metrics require their declared time reduction.",
        "0.2.0",
    ),
    introduced(
        "question.validation.snapshot_aggregation",
        Tier::T3,
        "Snapshot aggregation requires a series grain and ordered reductions; this contract refuses it.",
        "0.2.0",
    ),
    introduced(
        "question.validation.unknown_comparison",
        Tier::T3,
        "Comparison questions must exist and have compatible selections.",
        "0.2.0",
    ),
    entry(
        "relation.validation.invalid_payload",
        Tier::T2,
        "RelationIR payload is invalid at {path}: {detail}",
    ),
    entry(
        "relation.validation.v2_relation_is_link",
        Tier::T2,
        "RelationIR has no v2 form: declare an EntityIR v2 relation as a link in the source entity's payload.objectType.links",
    ),
    entry(
        "rule.validation.invalid_binding",
        Tier::T2,
        "a decide rule's triggers or slice are invalid: {detail}",
    ),
    entry(
        "rule.validation.invalid_decision_contract",
        Tier::T2,
        "a decide graph's {schema} declares exactly {members}: {detail}",
    ),
    entry(
        "rule.validation.invalid_fact_change",
        Tier::T2,
        "a decide graph's facts item schema is not the canonical fact change: {detail}",
    ),
    entry(
        "rule.validation.invalid_payload",
        Tier::T2,
        "RuleIR payload is invalid at {path}: {detail}",
    ),
    entry(
        "rule.validation.invalid_schema",
        Tier::T2,
        "RuleIR {schema} is not a closed schema: {detail}",
    ),
    introduced(
        "rule.validation.invalid_source_command",
        Tier::T2,
        "a declared source command or activation contract is invalid: {detail}",
        "0.2.0",
    ),
    entry(
        "rule.validation.jdm_custom_node_rejected",
        Tier::T2,
        "customNode is not admitted by the Norn profile: inline custom nodes are rejected (D33); use a pool Function",
    ),
    entry(
        "rule.validation.jdm_cycle",
        Tier::T2,
        "nodes {nodeIds} form a cycle; a decision graph is acyclic",
    ),
    entry(
        "rule.validation.jdm_dangling_edge",
        Tier::T2,
        "edge {edgeId} names node {nodeId}, which the graph does not have",
    ),
    entry(
        "rule.validation.jdm_duplicate_id",
        Tier::T2,
        "{what} id {id} is not unique",
    ),
    entry(
        "rule.validation.jdm_function_node_rejected",
        Tier::T2,
        "functionNode is not admitted by the Norn profile: JavaScript is forbidden in the deterministic core",
    ),
    entry(
        "rule.validation.jdm_hit_policy_rejected",
        Tier::T2,
        "hit policy {policy} is not admitted by the Norn profile: JDM admits first and collect; other DMN hit policies are translated or rejected at DMN ingress (T3-DEC-4)",
    ),
    entry(
        "rule.validation.jdm_invalid_edge",
        Tier::T2,
        "edge {edgeId} {problem}",
    ),
    entry(
        "rule.validation.jdm_invalid_expression",
        Tier::T2,
        "{source} is not a ZEN {form}: {detail}",
    ),
    entry(
        "rule.validation.jdm_io_node_count",
        Tier::T2,
        "the graph has {count} {inputOrOutput} nodes; a decision graph has exactly one",
    ),
    entry(
        "rule.validation.jdm_recursive_sub_decision",
        Tier::T2,
        "sub-decision {key} names this rule; a decision graph cannot call itself",
    ),
    entry(
        "rule.validation.jdm_switch_default",
        Tier::T2,
        "switch {nodeId} {problem}",
    ),
    entry(
        "rule.validation.jdm_switch_unconnected_branch",
        Tier::T2,
        "branch {statementId} of switch {nodeId} has no outgoing edge, so an event that takes it reaches no node",
    ),
    entry(
        "rule.validation.jdm_unknown_column",
        Tier::T2,
        "row {rowId} has a cell for {columnId}, which is not a column of the table",
    ),
    entry(
        "rule.validation.jdm_unknown_node_type",
        Tier::T2,
        "node type {type} is not a JDM node type; the Norn profile admits inputNode, outputNode, expressionNode, decisionTableNode, switchNode, and decisionNode",
    ),
    entry(
        "rule.validation.jdm_unreachable_node",
        Tier::T2,
        "node {nodeId} {problem}; every node of a decision graph lies on a path from input to output",
    ),
    entry(
        "rule.validation.jdm_unresolved_import",
        Tier::T2,
        "import {import} does not resolve: the Norn profile has no importable policy documents; declare value types in EntityIR v2",
    ),
    entry(
        "rule.validation.jdm_unresolved_sub_decision",
        Tier::T2,
        "sub-decision {key} is not a required rule dependency of this artifact; the key is the artifactId of a RuleIR v2 rule declared in dependencies",
    ),
    entry(
        "sealing.extension_point.capability_missing",
        Tier::T3,
        "operation lacks the capabilities its extension point grants: {capabilities}",
    ),
    entry(
        "sealing.extension_point.contract_violation",
        Tier::T3,
        "overlay payload violates the extension-point contract: {reason}",
    ),
    entry(
        "sealing.extension_point.kind_mismatch",
        Tier::T3,
        "extension-point contribution kind does not match the operation",
    ),
    entry(
        "sealing.extension_point.unresolved",
        Tier::T3,
        "{operation} references an extension point not exposed by its seal",
    ),
    entry(
        "sealing.input.duplicate_extension_point",
        Tier::T3,
        "extension-point definitions must contain at most one entry per ID",
    ),
    entry(
        "sealing.input.duplicate_seal",
        Tier::T3,
        "seal annotations must contain at most one entry per node",
    ),
    entry(
        "sealing.monotonic.unseal_forbidden",
        Tier::T3,
        "downstream overlays cannot unseal an upstream seal",
    ),
    entry(
        "sealing.overlay.base_mutation_denied",
        Tier::T3,
        "base node '{nodeId}' is immutable: {reason}",
    ),
    entry(
        "sealing.overlay.sealed_mutation",
        Tier::T3,
        "sealed base nodes cannot be mutated without an exposed extension point",
    ),
    entry(
        "sealing.overlay.tenant_namespace_mismatch",
        Tier::T3,
        "additive customization node and scope must belong to the declared tenant namespace",
    ),
    entry(
        "sealing.overlay.tenant_namespace_required",
        Tier::T3,
        "additive tenant customization requires a tenant namespace",
    ),
    entry(
        "sealing.runtime.capability_denied",
        Tier::T6,
        "custom code receives no handle to sealed nodes unless an extension point grants capabilities",
    ),
    entry(
        "sealing.upgrade.monotonic_violation",
        Tier::T4,
        "base upgrades cannot weaken or remove an existing seal without kernel migration authority",
    ),
    entry(
        "setting.validation.invalid_payload",
        Tier::T2,
        "SettingIR payload is invalid at {path}: {detail}",
    ),
    entry(
        "source_schema.validation.blank_value",
        Tier::T2,
        "the value must not be blank",
    ),
    entry(
        "source_schema.validation.constraint_shape",
        Tier::T2,
        "constraint {id}: {detail}",
    ),
    entry(
        "source_schema.validation.duplicate_code",
        Tier::T2,
        "code set {id} is empty or lists {value} twice",
    ),
    entry(
        "source_schema.validation.duplicate_field",
        Tier::T2,
        "dataset {dataset} declares field {field} (or ordinal {ordinal}) twice",
    ),
    entry(
        "source_schema.validation.duplicate_id",
        Tier::T2,
        "{list} declares id {id} twice",
    ),
    entry(
        "source_schema.validation.invalid_document",
        Tier::T2,
        "the document is not a SourceSchemaIR document: {detail}",
    ),
    entry(
        "source_schema.validation.invalid_evidence",
        Tier::T2,
        "evidence {uri} needs a non-blank uri and a lower-case hex SHA-256",
    ),
    entry(
        "source_schema.validation.invalid_source_identity",
        Tier::T2,
        "source identity components must not contain control characters",
    ),
    entry(
        "source_schema.validation.invalid_timestamp",
        Tier::T2,
        "{value} is not an RFC 3339 timestamp with an explicit offset",
    ),
    entry(
        "source_schema.validation.profile_counts",
        Tier::T2,
        "profile measures are inconsistent: {detail}",
    ),
    entry(
        "source_schema.validation.promoted_inference",
        Tier::T2,
        "{entry} has origin {origin}, which its list does not admit; an inference is never a declaration",
    ),
    entry(
        "source_schema.validation.sample_bound",
        Tier::T2,
        "sample set exceeds its bound: {detail}",
    ),
    entry(
        "source_schema.validation.unknown_dataset",
        Tier::T2,
        "dataset {dataset} is not declared",
    ),
    entry(
        "source_schema.validation.unknown_field",
        Tier::T2,
        "dataset {dataset} has no field {field}",
    ),
    introduced(
        "widget.interaction.control_missing_for_transition",
        Tier::T2,
        "a declared navigation item or action binding has no control that offers it: {detail}",
        "0.2.0",
    ),
    introduced(
        "widget.interaction.control_without_legal_transition",
        Tier::T2,
        "a navigation control leads to a page the app does not declare: {detail}",
        "0.2.0",
    ),
    introduced(
        "widget.interaction.dialog_depth_exceeds_verified_bound",
        Tier::T2,
        "dialog nesting exceeds the stacked dialogs the renderer interaction statechart is verified for: {detail}",
        "0.2.0",
    ),
    introduced(
        "widget.interaction.dialog_no_keyboard_close",
        Tier::T2,
        "a dialog can only be dismissed by pointer; it needs a visible close or Escape: {detail}",
        "0.2.0",
    ),
    introduced(
        "widget.interaction.dialog_no_restore_target",
        Tier::T2,
        "a dialog has no focus restore target (action control inside it, named enclosing region or page): {detail}",
        "0.2.0",
    ),
    introduced(
        "widget.interaction.keyboard_unreachable_control",
        Tier::T2,
        "an interactive control is not keyboard reachable: {detail}",
        "0.2.0",
    ),
    introduced(
        "widget.interaction.nullable_without_clear",
        Tier::T2,
        "a field bound to a nullable input cannot be cleared to null (no clear affordance, or always required): {detail}",
        "0.2.0",
    ),
    introduced(
        "widget.interaction.unnamed_control",
        Tier::T2,
        "an interactive control has no accessible name: {detail}",
        "0.2.0",
    ),
    introduced(
        "widget.interaction.unreachable_page",
        Tier::T2,
        "an app page has no control that reaches it from the entry page: {detail}",
        "0.2.0",
    ),
    entry(
        "widget.validation.invalid_payload",
        Tier::T2,
        "WidgetIR payload is invalid at {path}: {detail}",
    ),
    entry(
        "widget.validation.missing_dependency",
        Tier::T2,
        "widget references '{artifactId}' without declaring it as a dependency",
    ),
    entry(
        "widget.validation.unsafe_action_binding",
        Tier::T2,
        "action binding must target a Norn action or process command, not a raw mutation",
    ),
];

/// The registry entry for `code`, if it is registered.
#[must_use]
pub fn lookup(code: &str) -> Option<&'static CodeEntry> {
    REGISTRY
        .binary_search_by(|entry| entry.code.cmp(code))
        .ok()
        .and_then(|index| REGISTRY.get(index))
}

/// The serialized severity label (`"error"`), matching the IR wire form.
#[must_use]
pub const fn severity_str(severity: &DiagnosticSeverity) -> &'static str {
    match severity {
        DiagnosticSeverity::Error => "error",
        DiagnosticSeverity::Warning => "warning",
        DiagnosticSeverity::Info => "info",
    }
}

/// The conformance manifest: every registered code with its tier, severity,
/// message template, and `since` version, in registry order. Checked in as
/// `crates/norn-checker/conformance/diagnostic-codes.json`.
#[must_use]
pub fn conformance_manifest() -> serde_json::Value {
    let codes: Vec<serde_json::Value> = REGISTRY
        .iter()
        .map(|entry| {
            serde_json::json!({
                "code": entry.code,
                "tier": entry.tier.as_str(),
                "severity": severity_str(&entry.severity),
                "message": entry.message,
                "since": entry.since,
            })
        })
        .collect();
    serde_json::json!({
        "format": MANIFEST_FORMAT,
        "checkerVersion": env!("CARGO_PKG_VERSION"),
        "codes": codes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_sorted_unique_and_well_formed() {
        for pair in REGISTRY.windows(2) {
            if let [left, right] = pair {
                assert!(left.code < right.code, "{} !< {}", left.code, right.code);
            }
        }
        for entry in REGISTRY {
            let segments: Vec<&str> = entry.code.split('.').collect();
            assert_eq!(
                segments.len(),
                3,
                "{} is not <area>.<category>.<detail>",
                entry.code
            );
            assert!(
                segments.iter().all(|segment| !segment.is_empty()
                    && segment
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')),
                "{} is not lower snake case",
                entry.code
            );
            assert!(!entry.message.is_empty(), "{} has no message", entry.code);
            assert_eq!(lookup(entry.code), Some(entry));
        }
        assert_eq!(lookup("ir.validation.not_a_code"), None);
    }

    #[test]
    fn envelope_constants_are_registered() {
        for code in [
            IR_UNSUPPORTED_VERSION,
            IR_REQUIRED_FIELD,
            IR_INVALID_ARTIFACT,
            IR_INVALID_DEPENDENCY,
        ] {
            assert!(lookup(code).is_some(), "{code}");
        }
    }
}
