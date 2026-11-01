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
use crate::{ActionEffectProgram, SliceDeclaration, ZenExpression};

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
    /// Closed action input schema (an object schema). Its `decimal` leaf has
    /// an integer `scale` in 0..=28 and accepts JSON strings only. The kernel
    /// decodes those strings into exact decimals before authorization or effects.
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
    /// Ordered input derivations applied before validation and writing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub derive: Vec<ActionInputDerivation>,
    /// Outbound commands enqueued atomically with this action call.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<ActionCommand>,
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
    /// Declarative outgoing links to authorized records in the declared slice.
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

/// Add an outgoing link alongside an ordinary action's business write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionCreateLink {
    /// Link declared on the effect's business object type.
    pub link: String,
    /// Fixed concrete target type.
    pub target_type: String,
    /// Input field naming an existing authorized declared-slice object.
    pub target_id_field: String,
}
