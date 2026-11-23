//! Typed ActionIR root and its MCP tool shape.
//!
//! Canonical ActionIR is an MCP-compatible tool (name, title, description,
//! closed input/output schemas) plus Norn fields: scope, typed boolean
//! preconditions, the whitelisted effect program, idempotency, ledger
//! category, compensation, access, and the declared write set (rulings
//! 2026-08-27 §2 and 2026-09-25 §16; #2417). MCP is a projection of this root
//! ([`McpTool`]); canonical IR is never reconstructed from an MCP tool.
//!
//! The envelope's [`crate::ActionPayload`] stays the lenient wire root (unknown
//! or malformed members become `norn-checker` diagnostics); the checker decodes
//! its members into these types.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::members::{IrVersion01, present_non_null};
use crate::rule::JsonObject;
use crate::{ActionAggregateInput, ActionEffectProgram, SliceDeclaration, ZenExpression};

/// Typed payload of an `action` artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionRoot {
    /// Stable action id; the projected MCP tool `name`.
    pub action_id: String,
    /// Human-readable display name; the MCP tool `title`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Model-facing description; the MCP tool `description`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Scope dimensions the caller must supply (fail closed).
    pub scope_required: Vec<String>,
    /// Whether repeating the action with the same input has no further effect.
    pub idempotent: bool,
    /// Whether the action appends a ledger event.
    pub ledgered: bool,
    /// Ledger category of the event the action appends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ledger_category: Option<String>,
    /// Closed action input schema, using the shared `number` + `nornType`
    /// Decimal refinement with scale 0..=18. Transports carry these leaves as
    /// exact JSON strings; the kernel decodes them before authorization or effects.
    pub input_schema: JsonObject,
    /// Closed JSON Schema of the action output.
    pub output_schema: JsonObject,
    /// Boolean ZEN Expression preconditions over the input and the declared
    /// slice; all must hold before the action's effects are prepared.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preconditions: Vec<ZenExpression>,
    /// Bounded object reads used by preconditions, resolved in the action's
    /// commit against the versions they read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slice: Vec<SliceDeclaration>,
    /// Bounded exact folds over stored objects (#1105), read by preconditions
    /// by name and resolved in the action's commit against the matched set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aggregates: Vec<ActionAggregateInput>,
    /// Ordered input derivations applied before validation and writing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub derive: Vec<ActionInputDerivation>,
    /// Outbound commands enqueued atomically with this action call.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<ActionCommand>,
    /// TXN-RULE-1: one pinned synchronous, facts-only RuleIR writer in the
    /// action's atomic closure. Hosts without this execution profile refuse.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "ActionRuleRef")]
    pub synchronous_rule: Option<ActionRuleRef>,
    /// The whitelisted effect program; the closed [`ActionEffectProgram`]
    /// enum is the effect whitelist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_program: Option<ActionEffectProgram>,
    /// One bounded quantity operation committed with the business effect.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "ActionQuantityEffect")]
    pub quantity_effect: Option<ActionQuantityEffect>,
    /// Typed links to captured or proposed objects in the same checked commit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub create_links: Vec<ActionCreateLink>,
    /// Declared write set (#2417): every entity kind the action may change and
    /// how. The checker verifies effect targets are a subset.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub writes: Vec<ActionWrite>,
    /// Compensating action for reversal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compensation: Option<ActionCompensation>,
    /// Access definition that authorizes the action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<AccessRef>,
    /// Closed UI-facing effect declarations that are not derivable from the
    /// action's local write set or outbound commands.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "ActionUiEffects")]
    pub ui_effects: Option<ActionUiEffects>,
    /// Reserved extension slots keyed by `<name>/<semver>`; `null` is rejected.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "JsonObject")]
    pub extensions: Option<JsonObject>,
}

/// One deterministic ZEN derivation of an action input field.
///
/// Derivations run in list order. Every target must be a declared
/// `inputSchema` property; repeated targets are permitted and let a later
/// derivation refine an earlier result. When `input_required` is false, the
/// caller may omit the target before derivation. The derivation must still
/// produce a value accepted by the final input schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionInputDerivation {
    /// The input field written by this derivation.
    pub field: String,
    /// ZEN Expression evaluated against the input, including prior derivations.
    pub expression: ZenExpression,
    /// Whether the caller must supply the target field before derivation.
    /// Defaults to `true` and is omitted from the canonical representation.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub input_required: bool,
}

/// Closed reference to the admitted synchronous writer of an action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionRuleRef {
    /// Exact RuleIR artifact identity.
    pub artifact_id: String,
    /// Exact artifact version; no latest-version resolution.
    pub version: String,
}

impl<'de> Deserialize<'de> for ActionRuleRef {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Pin {
            artifact_id: String,
            version: String,
        }
        // TXN-RULE-1: schema and decoder agree on the object-only wire form.
        let pin: Pin = crate::members::deserialize_object(deserializer)?;
        Ok(Self {
            artifact_id: pin.artifact_id,
            version: pin.version,
        })
    }
}

fn default_true() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

impl ActionRoot {
    /// The closed effects a UI must handle for this action.
    ///
    /// Effects with trustworthy sources in the action definition are derived:
    /// outbound commands are external and asynchronous and may have an unknown
    /// outcome, idempotency follows the action's declared flag, and deleted
    /// writes are destructive. Other effects come from the optional, versioned
    /// UI effect declaration.
    #[must_use]
    pub fn effective_ui_effects(&self) -> BTreeSet<ActionUiEffect> {
        let mut effects = BTreeSet::new();
        if !self.commands.is_empty() {
            effects.insert(ActionUiEffect::External);
            effects.insert(ActionUiEffect::Async);
            effects.insert(ActionUiEffect::Error {
                code: "OutcomeUnknown".to_owned(),
            });
        }
        if self.idempotent {
            effects.insert(ActionUiEffect::Idempotent);
        }
        if self
            .writes
            .iter()
            .any(|write| write.change == WriteChange::Deleted)
        {
            effects.insert(ActionUiEffect::Destructive);
        }
        if let Some(declared) = &self.ui_effects {
            if declared.irreversible {
                effects.insert(ActionUiEffect::Irreversible);
            }
            if declared.destructive {
                effects.insert(ActionUiEffect::Destructive);
            }
            effects.extend(
                declared
                    .money
                    .iter()
                    .cloned()
                    .map(|currency_ref| ActionUiEffect::Money { currency_ref }),
            );
            effects.extend(
                declared
                    .error_codes
                    .iter()
                    .cloned()
                    .map(|code| ActionUiEffect::Error { code }),
            );
            effects.extend(
                declared
                    .result_codes
                    .iter()
                    .cloned()
                    .map(|code| ActionUiEffect::Result { code }),
            );
        }
        effects
    }
}

/// Versioned, closed declarations for UI-facing effects that cannot be
/// inferred from an action's commands, write set, or idempotency flag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionUiEffects {
    /// Version of the closed UI effect declaration vocabulary.
    pub effects_version: IrVersion01,
    /// Whether the action has an effect that cannot be reversed.
    #[serde(default, skip_serializing_if = "is_false")]
    pub irreversible: bool,
    /// Whether the action may destroy existing business data.
    #[serde(default, skip_serializing_if = "is_false")]
    pub destructive: bool,
    /// Currency identities used by monetary effects.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub money: Vec<String>,
    /// Declared error codes that callers must handle.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub error_codes: Vec<String>,
    /// Declared result codes that callers must handle.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub result_codes: Vec<String>,
}

fn is_false(value: &bool) -> bool {
    !value
}

/// One element of the closed effect vocabulary consumed by UI laws and MCP
/// projection.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ActionUiEffect {
    /// The action dispatches work to an external system.
    External,
    /// The action has an effect that cannot be reversed.
    Irreversible,
    /// The action may destroy existing business data.
    Destructive,
    /// The action handles a monetary amount in this currency.
    Money {
        /// Exact identity of the currency used by the monetary effect.
        currency_ref: String,
    },
    /// The action completes asynchronously.
    Async,
    /// Repeating the action with the same input has no further effect.
    Idempotent,
    /// The action can fail with this declared error code.
    Error {
        /// Exact identity of the error code.
        code: String,
    },
    /// The action can complete with this declared result code.
    Result {
        /// Exact identity of the result code.
        code: String,
    },
}

/// One declarative outbound command produced by an action call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionCommand {
    /// The source system connector to perform this command against.
    pub connector: String,
    /// The closed operation to perform.
    pub operation: ActionCommandOperation,
    /// The source document type, for example `Sales Order`.
    pub doctype: String,
    /// ZEN Expression yielding the source document name. Required for submit,
    /// update, and cancel operations; optional for create.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<ZenExpression>,
    /// Source fields whose values are derived from the action input.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, ZenExpression>,
    /// Expected source version for a guarded update. The evaluated value is
    /// carried as `expectedModified` to the connector perform contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_modified: Option<ZenExpression>,
    /// Owner-scoped child-table replacements declared by this command.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub child_merges: Vec<ActionCommandChildMerge>,
}

/// A declarative owner-scoped complete desired set for one source child table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionCommandChildMerge {
    /// Parent field holding the source child rows.
    pub field: String,
    /// Child row field that records Norn ownership.
    pub ownership_field: String,
    /// ZEN Expression yielding the owner marker written to each desired row.
    pub ownership_value: ZenExpression,
    /// Child row field used to match an existing owned row.
    pub row_key_field: String,
    /// ZEN Expression yielding the complete desired row array.
    pub rows: ZenExpression,
}

/// Closed set of outbound command operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ActionCommandOperation {
    /// Submit an existing source document.
    Submit,
    /// Create a source document.
    Create,
    /// Update an existing source document.
    Update,
    /// Cancel an existing source document.
    Cancel,
}

/// One entry of an action's declared write set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionWrite {
    pub entity_kind: String,
    pub change: WriteChange,
}

/// How an action changes an entity kind in its write set.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum WriteChange {
    Created,
    Modified,
    Deleted,
}

/// The compensating action that reverses this one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionCompensation {
    pub action_id: String,
}

/// Reference to the access definition that authorizes an action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccessRef {
    pub artifact_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// An MCP tool definition: the `Tool` shape of the Model Context Protocol
/// schema, revision 2026-07-28 (`schema/2026-07-28/schema.ts`, `interface
/// Tool` and `interface ToolAnnotations`).
///
/// Only the members an ActionIR projection populates are modelled: `name`,
/// `title`, `description`, `inputSchema`, `outputSchema`, `annotations`, and
/// `_meta` (which carries the lifecycle transition a tool fires). `icons` is
/// omitted (Norn has no source for it).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpTool {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema object; MCP requires `type: "object"` at the root.
    pub input_schema: JsonObject,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<JsonObject>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<McpToolAnnotations>,
    /// MCP `_meta`: Norn keys use the `norn/` prefix.
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<JsonObject>,
}

/// MCP `ToolAnnotations` (revision 2026-07-28). All members are hints.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpToolAnnotations {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only_hint: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destructive_hint: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotent_hint: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_world_hint: Option<bool>,
}

/// A closed quantity intent. Ledger/template identities come from the admitted
/// definition; callers supply only schema-checked values and optimistic fences.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionQuantityEffect {
    /// Fixed cell-local ledger selected by the installer.
    pub ledger_id: String,
    /// Input field carrying the complete v2 balance-version read set. This
    /// transport field is excluded from an upsert's business property writes.
    pub balance_versions_field: String,
    /// The admitted operation; configuration, opening posts and partial
    /// resolution are intentionally outside the ordinary action vocabulary.
    pub operation: ActionQuantityOperation,
}

/// Ordinary quantity-backed actions can reserve or fully post a reservation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ActionQuantityOperation {
    /// Reserve the pinned template with exact string-valued parameters.
    Reserve {
        /// Input field naming the reservation.
        reservation_id_field: String,
        /// Fixed template in the configured ledger's admitted pack.
        template_id: String,
        /// Pack parameter to exact input field, including its pinned scale.
        parameters: BTreeMap<String, ActionQuantityParameter>,
        /// Exclusive deadline offset from the trusted cell clock.
        ttl_ms: crate::WholeNumber,
    },
    /// Post all remaining legs of the named reservation.
    PostReservation {
        /// Input field naming the reservation.
        reservation_id_field: String,
        /// Input field naming the immutable posting.
        posting_id_field: String,
    },
}

/// Exact parameter binding; no float or caller-selected scale crosses this boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionQuantityParameter {
    /// Input field containing the canonical unscaled integer string.
    pub field: String,
    /// Scale expected by the pinned template parameter.
    pub scale: crate::WholeNumber,
}

/// Declare a typed link inside the action's checked proposal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionCreateLink {
    /// Optional source written by the same synchronous proposal (TXN-ATOMIC-1).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::members::present_non_null"
    )]
    #[schemars(with = "ActionLinkSource")]
    pub source: Option<ActionLinkSource>,
    /// Link declared on the source object type.
    pub link: String,
    /// Fixed concrete target type.
    pub target_type: String,
    /// Required input field naming a captured or proposed target.
    pub target_id_field: String,
}

/// A declared input-key source for a link within one action proposal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionLinkSource {
    /// Concrete type of an object written by the synchronous closure.
    pub object_type: String,
    /// Required string input field containing its identity.
    pub id_field: String,
}

impl<'de> Deserialize<'de> for ActionLinkSource {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Source {
            object_type: String,
            id_field: String,
        }
        // TXN-CLOSED-1: the decoder retains the schema's object-only source binding.
        let source: Source = crate::members::deserialize_object(deserializer)?;
        Ok(Self {
            object_type: source.object_type,
            id_field: source.id_field,
        })
    }
}

/// Maximum UTF-8 byte length of [`TraceContext::trace_id`].
pub const MAX_TRACE_ID_BYTES: usize = 256;

/// Maximum UTF-8 byte length of [`TraceContext::intent_ref`].
pub const MAX_INTENT_REF_BYTES: usize = 512;

/// Why a caller-supplied [`TraceContext`] is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TraceContextError {
    /// `traceId` is empty or whitespace-only.
    BlankTraceId,
    /// `traceId` exceeds [`MAX_TRACE_ID_BYTES`].
    TraceIdTooLong {
        /// Actual UTF-8 byte length.
        actual_bytes: usize,
        /// Permitted UTF-8 byte length.
        max_bytes: usize,
    },
    /// `traceId` contains control characters or leading/trailing whitespace.
    InvalidTraceId,
    /// `intentRef` is empty or whitespace-only.
    BlankIntentRef,
    /// `intentRef` exceeds [`MAX_INTENT_REF_BYTES`].
    IntentRefTooLong {
        /// Actual UTF-8 byte length.
        actual_bytes: usize,
        /// Permitted UTF-8 byte length.
        max_bytes: usize,
    },
    /// `intentRef` contains control characters or leading/trailing whitespace.
    InvalidIntentRef,
    /// `sessionDigest` is not a canonical `sha256:<64 lower-case hex>` digest.
    InvalidSessionDigest,
}

impl TraceContextError {
    /// Kernel input field path naming the invalid trace member.
    #[must_use]
    pub const fn field(&self) -> &'static str {
        match self {
            Self::BlankTraceId | Self::TraceIdTooLong { .. } | Self::InvalidTraceId => {
                "trace.traceId"
            }
            Self::BlankIntentRef | Self::IntentRefTooLong { .. } | Self::InvalidIntentRef => {
                "trace.intentRef"
            }
            Self::InvalidSessionDigest => "trace.sessionDigest",
        }
    }

    /// RFC 6901 JSON pointer into an action request body for this error.
    #[must_use]
    pub const fn pointer(&self) -> &'static str {
        match self {
            Self::BlankTraceId | Self::TraceIdTooLong { .. } | Self::InvalidTraceId => {
                "/trace/traceId"
            }
            Self::BlankIntentRef | Self::IntentRefTooLong { .. } | Self::InvalidIntentRef => {
                "/trace/intentRef"
            }
            Self::InvalidSessionDigest => "/trace/sessionDigest",
        }
    }
}

impl std::fmt::Display for TraceContextError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BlankTraceId => f.write_str("trace.traceId must be non-empty"),
            Self::TraceIdTooLong {
                actual_bytes,
                max_bytes,
            } => write!(
                f,
                "trace.traceId is {actual_bytes} bytes; the limit is {max_bytes} bytes"
            ),
            Self::InvalidTraceId => {
                f.write_str("trace.traceId must not contain control characters or outer whitespace")
            }
            Self::BlankIntentRef => f.write_str("trace.intentRef must be non-empty"),
            Self::IntentRefTooLong {
                actual_bytes,
                max_bytes,
            } => write!(
                f,
                "trace.intentRef is {actual_bytes} bytes; the limit is {max_bytes} bytes"
            ),
            Self::InvalidIntentRef => f.write_str(
                "trace.intentRef must not contain control characters or outer whitespace",
            ),
            Self::InvalidSessionDigest => f.write_str(
                "trace.sessionDigest must be a sha256:<64 lower-case hex> content digest",
            ),
        }
    }
}

impl std::error::Error for TraceContextError {}

/// Optional provenance trace context attached to an action call and persisted
/// on its committed receipt.
///
/// Strictly optional and additive: callers without trace context are unaffected,
/// and raw transcript text is never stored (`sessionDigest` admits only a
/// content-addressed SHA-256 digest).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TraceContext {
    /// Opaque correlation identifier tying multi-step causes and effects
    /// together (1 to 256 UTF-8 bytes, no control characters or outer whitespace).
    #[schemars(length(min = 1, max = 256))]
    pub trace_id: String,
    /// Optional reference to the goal, prompt, commitment, or decision that
    /// initiated the action (1 to 512 UTF-8 bytes, no control characters or
    /// outer whitespace).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String", length(min = 1, max = 512))]
    pub intent_ref: Option<String>,
    /// Optional content-addressed SHA-256 digest (`sha256:<64 lower-case hex>`)
    /// of an off-ledger agent or user session transcript. Raw transcript text
    /// is never stored in the ledger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "crate::function::Digest")]
    pub session_digest: Option<crate::function::Digest>,
}

fn is_forbidden_trace_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
        )
}

impl TraceContext {
    /// Validate every field fail-closed against bounded length, printable text,
    /// and canonical SHA-256 digest rules.
    ///
    /// # Errors
    ///
    /// Returns [`TraceContextError`] on the first invalid member.
    pub fn validate(&self) -> Result<(), TraceContextError> {
        if self.trace_id.trim().is_empty() {
            return Err(TraceContextError::BlankTraceId);
        }
        if self.trace_id.len() > MAX_TRACE_ID_BYTES {
            return Err(TraceContextError::TraceIdTooLong {
                actual_bytes: self.trace_id.len(),
                max_bytes: MAX_TRACE_ID_BYTES,
            });
        }
        if self.trace_id.trim() != self.trace_id
            || self.trace_id.chars().any(is_forbidden_trace_char)
        {
            return Err(TraceContextError::InvalidTraceId);
        }
        if let Some(intent_ref) = &self.intent_ref {
            if intent_ref.trim().is_empty() {
                return Err(TraceContextError::BlankIntentRef);
            }
            if intent_ref.len() > MAX_INTENT_REF_BYTES {
                return Err(TraceContextError::IntentRefTooLong {
                    actual_bytes: intent_ref.len(),
                    max_bytes: MAX_INTENT_REF_BYTES,
                });
            }
            if intent_ref.trim() != intent_ref || intent_ref.chars().any(is_forbidden_trace_char) {
                return Err(TraceContextError::InvalidIntentRef);
            }
        }
        if let Some(digest) = &self.session_digest
            && !digest.is_well_formed()
        {
            return Err(TraceContextError::InvalidSessionDigest);
        }
        Ok(())
    }

    /// Encode this trace context as a canonical [`crate::RuntimeValue`] object,
    /// omitting absent optional members.
    #[must_use]
    pub fn to_runtime_value(&self) -> crate::RuntimeValue {
        let mut fields = BTreeMap::from([(
            "traceId".to_owned(),
            crate::RuntimeValue::String(self.trace_id.clone()),
        )]);
        if let Some(intent_ref) = &self.intent_ref {
            fields.insert(
                "intentRef".to_owned(),
                crate::RuntimeValue::String(intent_ref.clone()),
            );
        }
        if let Some(session_digest) = &self.session_digest {
            fields.insert(
                "sessionDigest".to_owned(),
                crate::RuntimeValue::String(session_digest.0.clone()),
            );
        }
        crate::RuntimeValue::Object(fields)
    }
}

impl<'de> Deserialize<'de> for TraceContext {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct WireTraceContext {
            trace_id: String,
            #[serde(default, deserialize_with = "crate::members::present_non_null")]
            intent_ref: Option<String>,
            #[serde(default, deserialize_with = "crate::members::present_non_null")]
            session_digest: Option<crate::function::Digest>,
        }
        let wire: WireTraceContext = crate::members::deserialize_object(deserializer)?;
        Ok(Self {
            trace_id: wire.trace_id,
            intent_ref: wire.intent_ref,
            session_digest: wire.session_digest,
        })
    }
}
