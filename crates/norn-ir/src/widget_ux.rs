//! Versioned, typed authoring contract carried by `WidgetRoot.ux` (#3173).
//!
//! The UX contract is an authoring and projection description. It names exact
//! entity and action artifacts, query bounds, fields, conditions, and action
//! placements; it never grants permissions or carries executable callbacks.
//! Consistency and reference checks belong to `norn-checker`.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::value::RawValue;

use crate::members::{IrVersion01, WholeNumber, present_non_null};
use crate::{JsonValue, ZenExpression};

/// Exact version of the first closed Widget UX contract.
pub const WIDGET_UX_V1_VERSION: &str = "0.1";

/// The typed UX contract nested in a legacy `widget` payload.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WidgetUx {
    /// Version of this nested UX contract, independent of the legacy widget
    /// envelope members.
    pub ux_version: IrVersion01,
    /// Optional application shell that composes pages and navigation.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "WidgetApp")]
    pub app: Option<WidgetApp>,
    /// Named pages owned by this widget UX contract.
    pub pages: Vec<WidgetPage>,
    /// Exact object reads used by pages, fields, and actions.
    pub data_bindings: Vec<WidgetDataBinding>,
    /// Typed Norn action calls available to placements and forms.
    pub action_bindings: Vec<WidgetUxActionBinding>,
    /// Field presentation keyed by stable field ids and property references.
    pub fields: Vec<WidgetFieldPresentation>,
    /// Forms submit through an action binding, never a raw CRUD handle.
    pub forms: Vec<WidgetForm>,
    /// Where a declared action binding appears in a page.
    pub action_placements: Vec<WidgetActionPlacement>,
    /// Closed error/result handlers supplied by the application shell.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "WidgetShellHandlers")]
    pub shell_handlers: Option<WidgetShellHandlers>,
    /// Typed navigation destinations and capability requirements.
    pub navigation_items: Vec<WidgetNavigationItem>,
    /// Bounded changes to semantic ids of composed baseline components.
    pub overrides: Vec<WidgetOverride>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WidgetUxWire {
    ux_version: IrVersion01,
    #[serde(default, deserialize_with = "present_non_null")]
    app: Option<WidgetApp>,
    pages: Vec<WidgetPage>,
    data_bindings: Vec<WidgetDataBinding>,
    action_bindings: Vec<WidgetUxActionBinding>,
    fields: Vec<WidgetFieldPresentation>,
    forms: Vec<WidgetForm>,
    action_placements: Vec<WidgetActionPlacement>,
    #[serde(default, deserialize_with = "present_non_null")]
    shell_handlers: Option<WidgetShellHandlers>,
    navigation_items: Vec<WidgetNavigationItem>,
    overrides: Vec<WidgetOverride>,
}

impl From<WidgetUxWire> for WidgetUx {
    fn from(wire: WidgetUxWire) -> Self {
        Self {
            ux_version: wire.ux_version,
            app: wire.app,
            pages: wire.pages,
            data_bindings: wire.data_bindings,
            action_bindings: wire.action_bindings,
            fields: wire.fields,
            forms: wire.forms,
            action_placements: wire.action_placements,
            shell_handlers: wire.shell_handlers,
            navigation_items: wire.navigation_items,
            overrides: wire.overrides,
        }
    }
}

impl<'de> Deserialize<'de> for WidgetUx {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        if is_serde_json_value_deserializer::<D>() {
            let value = serde_json::Value::deserialize(deserializer)?;
            guard_widget_ux_numeric_value(&value).map_err(serde::de::Error::custom)?;
            let wire: WidgetUxWire =
                serde_json::from_value(value).map_err(serde::de::Error::custom)?;
            Ok(Self::from(wire))
        } else if is_serde_json_text_deserializer::<D>() && deserializer.is_human_readable() {
            let raw = Box::<RawValue>::deserialize(deserializer)?;
            guard_widget_ux_numeric_roundtrip(raw.get()).map_err(serde::de::Error::custom)?;
            let wire: WidgetUxWire =
                serde_json::from_str(raw.get()).map_err(serde::de::Error::custom)?;
            Ok(Self::from(wire))
        } else {
            WidgetUxWire::deserialize(deserializer).map(Self::from)
        }
    }
}

/// Reject UX JSON numbers whose existing `JsonValue` representation changes
/// their exact runtime value.
pub(crate) fn guard_widget_ux_numeric_roundtrip(raw: &str) -> Result<(), String> {
    let exact = crate::RuntimeValue::from_json_text(raw).map_err(|error| error.to_string())?;
    let definition: JsonValue = serde_json::from_str(raw).map_err(|error| error.to_string())?;
    let converted = crate::RuntimeValue::from_ir(&definition).map_err(|error| error.to_string())?;
    if exact != converted {
        return Err(
            "Widget UX number changes value through the definition JSON representation".into(),
        );
    }
    Ok(())
}

/// Apply the same value-preservation check to a constructed serde_json value.
pub(crate) fn guard_widget_ux_numeric_value(value: &serde_json::Value) -> Result<(), String> {
    let exact = crate::RuntimeValue::from_json(value.clone()).map_err(|error| error.to_string())?;
    let definition: JsonValue =
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
    let converted = crate::RuntimeValue::from_ir(&definition).map_err(|error| error.to_string())?;
    if exact != converted {
        return Err(
            "Widget UX number changes value through the definition JSON representation".into(),
        );
    }
    Ok(())
}

fn is_serde_json_text_deserializer<D>() -> bool {
    std::any::type_name::<D>().contains("serde_json::de::Deserializer<")
}

fn is_serde_json_value_deserializer<D>() -> bool {
    std::any::type_name::<D>().contains("serde_json::value::Value")
}

/// A pinned reference to an artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArtifactRef {
    /// Artifact identity.
    pub id: String,
    /// Exact artifact version.
    pub version: String,
}

/// A property on a resolved data binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PropertyRef {
    /// Data binding whose object type owns the property.
    pub binding_id: String,
    /// Property id on the binding's object type.
    pub property_id: String,
}

/// Locale-keyed user-facing text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct LocaleText(pub BTreeMap<String, String>);

/// The application shell for a set of pages.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WidgetApp {
    /// Stable app id.
    pub app_id: String,
    /// The page opened when the app has no more specific route.
    pub entry_page_id: String,
    /// Pages this app may route to.
    pub page_refs: Vec<WidgetPageRef>,
    /// Typed path-to-page routes.
    pub routes: Vec<WidgetRoute>,
    /// Navigation items in display order.
    pub navigation_item_ids: Vec<String>,
}

/// A reference to a page in this UX contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WidgetPageRef {
    /// Page id declared in `WidgetUx.pages`.
    pub page_id: String,
}

/// One route to a page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WidgetRoute {
    /// Stable route id.
    pub route_id: String,
    /// Route pattern interpreted by the host router.
    pub path: String,
    /// Page selected by this route.
    pub page_id: String,
    /// Route parameters mapped to declared page context parameters.
    pub parameters: Vec<WidgetRouteParameter>,
}

/// A route parameter mapped to one typed page context parameter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WidgetRouteParameter {
    /// Name captured from the route pattern.
    pub route_parameter: String,
    /// Declared parameter on the destination page.
    pub context_parameter_id: String,
}

/// One named page with typed context and a stable-id component tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WidgetPage {
    /// Stable page id. “Page” is an interaction surface, not computed data.
    pub page_id: String,
    /// Human-readable page name.
    pub name: String,
    /// Typed values supplied by routes or the host when opening this page.
    pub context_params: Vec<WidgetContextParam>,
    /// Root of the page component tree.
    pub root: WidgetNode,
    /// What this page is in the UI ontology (UI-VIEW-1): a semantic view
    /// kind bound to the domain concept it presents. Renderer-neutral.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "WidgetView")]
    pub view: Option<WidgetView>,
    /// Declared interaction statechart of this page (UI-MACHINE-1). The
    /// renderer executes only this machine; the checker proves it has no
    /// absorbing non-terminal state and that every pending state handles
    /// every server outcome.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "WidgetMachine")]
    pub machine: Option<WidgetMachine>,
}

/// A page's place in the UI ontology (UI-VIEW-1).
///
/// A view is a semantic, typed UI concept, never a visual one. Its four
/// contracts are the page's typed data bindings (data), its `machine`
/// (interaction), the `behaviors` on its nodes (accessibility), and the
/// capability catalog derived by [`WidgetUx::renderer_capabilities`]
/// (what a renderer must implement to draw it). Renderers are compile
/// targets of this contract; swapping one never touches the page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WidgetView {
    /// Semantic kind of the view.
    pub kind: WidgetViewKind,
    /// The domain concept the view presents.
    pub subject: WidgetViewSubject,
}

/// Closed semantic view kinds.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum WidgetViewKind {
    /// Many objects of one type.
    Collection,
    /// One object.
    Record,
    /// Input to one action.
    Form,
    /// The answer to one question or transform.
    Question,
    /// A dashboard definition's tiles.
    Dashboard,
    /// One run of a flow, step by step.
    Run,
    /// Decisions waiting on the viewer.
    DecisionInbox,
    /// Where one object's values came from.
    Lineage,
}

/// The domain concept a view presents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum WidgetViewSubject {
    /// An object type (entity artifact).
    ObjectType { object: ArtifactRef },
    /// An action artifact.
    Action { action: ArtifactRef },
    /// A retained question definition.
    Question { definition_id: String },
    /// A transform.
    Transform { transform_id: String },
    /// A retained dashboard definition.
    Dashboard { definition_id: String },
    /// A retained flow definition.
    Flow { definition_id: String },
    /// The viewer's decisions.
    Decisions,
}

/// Accessibility behaviours a node requires, named as UI SDK's behaviour
/// primitives (`@ui-sdk/core`). They say what the control must do for
/// keyboard, focus and assistive technology, never how it looks.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum WidgetBehavior {
    /// Focus is contained while open and restored on close.
    FocusScope,
    /// Arrow keys move focus among items; one tab stop.
    RovingFocus,
    /// Typing moves to the first matching item.
    Typeahead,
    /// Items can be selected and the selection is announced.
    Selection,
    /// Opens anchored to its invoker.
    Popup,
    /// Escape and outside press dismiss it.
    Dismiss,
    /// Items can be reordered or moved with pointer and keyboard.
    DragAndDrop,
    /// Changes are announced politely.
    LiveRegion,
}

impl WidgetUx {
    /// The renderer capability catalog this contract requires (UI-VIEW-1):
    /// every view kind, component kind, behaviour, effect, event source and
    /// outcome its pages use, as `<family>:<name>` strings. A renderer that
    /// lacks one cannot draw the contract and must refuse it.
    #[must_use]
    pub fn renderer_capabilities(&self) -> std::collections::BTreeSet<String> {
        fn name<T: Serialize>(value: &T) -> String {
            let value = serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
            value
                .as_str()
                .or_else(|| value.get("kind").and_then(serde_json::Value::as_str))
                .unwrap_or_default()
                .to_owned()
        }
        fn walk(node: &WidgetNode, out: &mut std::collections::BTreeSet<String>) {
            out.insert(format!("component:{}", name(&node.component)));
            for behavior in &node.behaviors {
                out.insert(format!("behavior:{}", name(behavior)));
            }
            for child in &node.children {
                walk(child, out);
            }
        }
        let mut out = std::collections::BTreeSet::new();
        for page in &self.pages {
            walk(&page.root, &mut out);
            if let Some(view) = &page.view {
                out.insert(format!("view:{}", name(&view.kind)));
            }
            let Some(machine) = &page.machine else {
                continue;
            };
            for event in &machine.events {
                out.insert(format!("event:{}", name(&event.source)));
                if let WidgetMachineEventSource::Outcome { outcome, .. } = &event.source {
                    out.insert(format!("outcome:{}", name(outcome)));
                }
            }
            for transition in &machine.transitions {
                for effect in &transition.effects {
                    match effect {
                        WidgetMachineEffect::Start { call, .. } => {
                            out.insert(format!("effect:start:{}", name(call)));
                        }
                        other @ (WidgetMachineEffect::Cancel
                        | WidgetMachineEffect::Navigate { .. }) => {
                            out.insert(format!("effect:{}", name(other)));
                        }
                    }
                }
            }
        }
        out
    }
}

/// A page's interaction statechart (UI-MACHINE-1).
///
/// Client state is the typed `fields` record, changed only by the `assign`
/// of a transition that a declared event fires. Effects are closed: call a
/// declared action binding, read a declared data binding, call a declared
/// host operation, cancel the awaited call, or navigate. The machine grants
/// no authority: the server decides every call and read it names.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WidgetMachine {
    /// Version of the machine contract.
    pub machine_version: IrVersion01,
    /// State the page opens in; it must have the `idle` role.
    pub initial_state_id: String,
    /// Typed client-state fields.
    pub fields: Vec<WidgetMachineField>,
    /// Declared events and where each comes from.
    pub events: Vec<WidgetMachineEvent>,
    /// Declared states.
    pub states: Vec<WidgetMachineState>,
    /// Declared transitions.
    pub transitions: Vec<WidgetMachineTransition>,
}

/// One typed client-state field, or one typed member of an event payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WidgetMachineField {
    /// Stable field id, unique within its record.
    pub field_id: String,
    /// Scalar type.
    #[serde(rename = "type")]
    pub value_type: WidgetValueType,
    /// Whether the value is always present (otherwise it may be null).
    pub required: bool,
    /// Initial value of a state field. Required fields need one; payload
    /// members never carry one.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "JsonValue")]
    pub initial: Option<JsonValue>,
}

/// One declared event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WidgetMachineEvent {
    /// Stable event id.
    pub event_id: String,
    /// Where the event comes from.
    pub source: WidgetMachineEventSource,
    /// Typed payload members carried by a user or subscription event.
    /// Outcome events carry the fixed payload of their outcome.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub payload: Vec<WidgetMachineField>,
}

/// Closed event sources. Every source is an input: a person, the server, a
/// subscription, or a deadline the machine itself declared.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum WidgetMachineEventSource {
    /// The page was opened by the host.
    PageOpened,
    /// A person used the control at `node_id` in this page's tree.
    User { node_id: String },
    /// The server answered the call or read the current state awaits.
    Outcome {
        outcome: WidgetCallOutcome,
        /// For `refused` only: the refusal category matched. Omitted, it
        /// matches every category.
        #[serde(
            default,
            deserialize_with = "present_non_null",
            skip_serializing_if = "Option::is_none"
        )]
        #[schemars(with = "WidgetRefusalCategory")]
        category: Option<WidgetRefusalCategory>,
    },
    /// A subscribed data binding delivered a new snapshot.
    Subscription { binding_id: String },
    /// Fires `after_ms` after the source state of its transition is entered.
    Timer { after_ms: WholeNumber },
}

/// Every way a call or read the page awaits can end.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum WidgetCallOutcome {
    /// The server committed the call or answered the read.
    Accepted,
    /// The server refused with a coded refusal.
    Refused,
    /// The call waits for an approval decision (HTTP 202, APPROVAL-1).
    PendingApproval,
    /// The read's answer or snapshot moved under it (HTTP 409
    /// `core.projection.answer_moved`, `object.snapshot_moved`).
    AnswerMoved,
    /// No answer arrived from the server (network failure, 5xx without a
    /// coded refusal, route not served).
    NetworkFailure,
    /// The state's declared deadline passed first.
    Timeout,
    /// The page cancelled the call.
    Cancelled,
}

/// Refusal categories, as the server's `norn/refusal@2` documents them.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum WidgetRefusalCategory {
    /// The target is not visible to the caller.
    Visibility,
    /// The caller lacks permission.
    Permission,
    /// A precondition or law refused the call.
    Precondition,
    /// The call conflicts with current state.
    Conflict,
    /// The input is invalid.
    Invalid,
    /// A limit was reached.
    Limit,
    /// The server failed internally.
    Internal,
}

/// The role of a state. Roles drive the checker's laws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WidgetStateRole {
    /// Waiting for a person or a subscription.
    Idle,
    /// Waiting for the server to answer the call or read in `awaits`.
    Pending,
    /// Showing a failure the person can recover from.
    Error,
    /// Done; the page has nothing left to do. No transition leaves it.
    Terminal,
}

/// One declared state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WidgetMachineState {
    /// Stable state id.
    pub state_id: String,
    /// Role of the state.
    pub role: WidgetStateRole,
    /// The call or read a `pending` state waits for.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "WidgetMachineCall")]
    pub awaits: Option<WidgetMachineCall>,
    /// Deadline of a `pending` state; when it passes the runtime cancels the
    /// call and raises the `timeout` outcome.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "WholeNumber")]
    pub deadline_ms: Option<WholeNumber>,
}

/// A server call or read named by a pending state or an effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum WidgetMachineCall {
    /// Call the declared action binding.
    Action { binding_id: String },
    /// Read the declared data binding.
    Read { binding_id: String },
    /// Call a host operation by its `OpenAPI` operation id.
    Operation { operation_id: String },
}

/// One declared transition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WidgetMachineTransition {
    /// Stable transition id.
    pub transition_id: String,
    /// Source state.
    pub from: String,
    /// Triggering event.
    pub on: String,
    /// Target state.
    pub target: String,
    /// Boolean ZEN guard over `input.fields` and `input.event`. A guarded
    /// transition refines; it never counts as covering an outcome.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "ZenExpression")]
    pub guard: Option<ZenExpression>,
    /// New field values, each a ZEN expression over `input.fields` and
    /// `input.event`, typed as the field.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub assign: BTreeMap<String, ZenExpression>,
    /// Effects run when the transition fires, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<WidgetMachineEffect>,
}

/// Closed transition effects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum WidgetMachineEffect {
    /// Start the call or read; its outcome arrives as an `outcome` event.
    Start {
        call: WidgetMachineCall,
        /// Request members, each a ZEN expression over `input.fields` and
        /// `input.event`. An action binding maps its own inputs and takes none.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        input: BTreeMap<String, ZenExpression>,
    },
    /// Cancel the awaited call; its outcome arrives as `cancelled`.
    Cancel,
    /// Navigate to a declared navigation item.
    Navigate { item_id: String },
}

/// One declared parameter in a page's context record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WidgetContextParam {
    /// Stable parameter id.
    pub param_id: String,
    /// Scalar type used to derive the page context record for ZEN checks.
    #[serde(rename = "type")]
    pub value_type: WidgetValueType,
    /// Whether the host must supply the parameter.
    pub required: bool,
    /// Marks this required text parameter as the runtime object key for one
    /// exact entity artifact.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "ArtifactRef")]
    pub runtime_key_of: Option<ArtifactRef>,
}

/// Scalar value types allowed in page context parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WidgetValueType {
    /// Text.
    Text,
    /// Signed integer.
    Integer,
    /// Exact decimal or general numeric value.
    Number,
    /// Boolean.
    Boolean,
    /// Calendar date.
    Date,
    /// Timestamp with an explicit zone.
    Timestamp,
    /// Elapsed duration.
    Duration,
    /// Opaque JSON value; it cannot be inspected by ZEN.
    Json,
}

/// One node in the page component tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WidgetNode {
    /// Stable id unique within the UX contract.
    pub node_id: String,
    /// Optional id used by bounded customization overrides.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub semantic_id: Option<String>,
    /// Closed component kind and its typed reference.
    pub component: WidgetComponent,
    /// Effects handled by this component for actions in its subtree.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub handlers: Vec<WidgetEffectHandler>,
    /// Accessibility behaviours this node requires (UI-VIEW-1).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub behaviors: Vec<WidgetBehavior>,
    /// Child components in presentation order.
    pub children: Vec<WidgetNode>,
}

/// What a person does with a [`WidgetComponent::Control`].
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum WidgetControlInput {
    /// Presses it.
    Trigger,
    /// Picks one of the values the page's data offers.
    Choice,
    /// Types text.
    Text,
}

/// Supported page components.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum WidgetComponent {
    /// A named section or tab with explicit field inclusion and order.
    Section { section: WidgetSection },
    /// A bounded object list.
    List {
        data_binding_id: String,
        /// Fields explicitly included in each row.
        fields: Vec<PropertyRef>,
        /// Selection cardinality exposed to actions and record-value bindings.
        selection: WidgetSelectionMode,
    },
    /// One object detail view.
    Detail { data_binding_id: String },
    /// A dialog scope for in-flight actions and their outcomes.
    Dialog {
        /// Action binding whose pending invocation makes this dialog blocking.
        #[serde(
            default,
            deserialize_with = "present_non_null",
            skip_serializing_if = "Option::is_none"
        )]
        #[schemars(with = "String")]
        binding_id: Option<String>,
        /// Whether dismissal is disabled while the bound command is pending.
        blocking: bool,
        /// Declared ways to leave a terminal dialog state.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        dismissal_methods: Vec<WidgetDismissalMethod>,
    },
    /// A declared form.
    Form { form_id: String },
    /// An editable, bounded ordered child collection owned by a form.
    ChildGrid {
        /// Form whose submit action owns the parent and row mutations.
        form_id: String,
        /// Localized label used as the grid's accessible region and heading.
        label: LocaleText,
        /// Owned link id declared by the form action's pinned entity.
        collection_id: String,
        /// Owned collection ordering property copied for renderer ordering; checker proves it matches EntityIR.
        ordering_key: String,
        /// Authoritative traversal snapshot for edits; omitted for a create form.
        #[serde(
            default,
            deserialize_with = "present_non_null",
            skip_serializing_if = "Option::is_none"
        )]
        #[schemars(with = "String")]
        data_binding_id: Option<String>,
        /// Action input member populated by this grid's ordered row operations.
        row_ops_input_name: String,
        /// Maximum row count, checked against the owned collection declaration.
        max_rows: WholeNumber,
        /// Field presentations rendered for every row.
        field_ids: Vec<String>,
    },
    /// One explicitly included presentation, including action-only form inputs.
    Field { field_id: String },
    /// An action placement.
    Action { placement_id: String },
    /// A navigation item.
    Navigation { item_id: String },
    /// A control whose only effect is to raise the page machine's `user`
    /// events that name this node (UI-MACHINE-1). A `choice` or `text`
    /// control sends its value as the event payload member `value`.
    Control {
        label: LocaleText,
        input: WidgetControlInput,
    },
    /// An exact page of a pinned widget artifact composed as a baseline.
    Composition {
        widget: ArtifactRef,
        page_id: String,
    },
}

/// One closed UI-law handler provided by a component or action invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum WidgetEffectHandler {
    /// A confirmation guard for an irreversible effect.
    ConfirmGuard {
        confirmation: WidgetActionConfirmation,
    },
    /// A view that presents pending and terminal outcomes for an invocation.
    OutcomeSurface {
        states: Vec<WidgetOutcomeState>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        currency_refs: Vec<String>,
    },
    /// A view that handles declared error codes.
    ErrorBanner { codes: Vec<String> },
    /// A view that handles declared result codes.
    ResultSurface { codes: Vec<String> },
}

/// Outcome states an action surface may render.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum WidgetOutcomeState {
    /// The action is still pending.
    Pending,
    /// The action completed successfully.
    Success,
    /// The action failed with a known failure.
    Failure,
    /// The external outcome cannot currently be established.
    UnknownOutcome,
}

/// Closed dismissal methods supported by a dialog component.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum WidgetDismissalMethod {
    /// A visible close control.
    VisibleClose,
    /// Tapping the backdrop.
    Scrim,
    /// Swiping the dialog away.
    Swipe,
    /// Pressing Escape.
    Escape,
}

/// Versioned handlers supplied by the rendered application shell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WidgetShellHandlers {
    /// Version of the closed shell handler set.
    pub handlers_version: IrVersion01,
    /// Shell handlers; only error and result handlers are valid here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub handlers: Vec<WidgetEffectHandler>,
}

/// A section or tab with explicitly included fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WidgetSection {
    /// Stable section id.
    pub section_id: String,
    /// Section or tab container.
    pub kind: WidgetSectionKind,
    /// Localized heading.
    pub label: LocaleText,
    /// Explicit property inclusion in presentation order.
    pub fields: Vec<PropertyRef>,
    /// Order among sibling sections.
    pub order: WholeNumber,
}

/// Section container kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WidgetSectionKind {
    /// A normal section.
    Section,
    /// A tab.
    Tab,
}

/// Selection cardinality exposed by a list component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WidgetSelectionMode {
    /// The list exposes no selected records.
    None,
    /// At most one row may be selected; that row is the current record.
    Single,
    /// A bounded set of rows may be selected.
    Multiple,
}

/// One exact object binding and its bounded query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WidgetDataBinding {
    /// Stable binding id.
    pub binding_id: String,
    /// Exact entity artifact whose object type is read.
    pub object: ArtifactRef,
    /// Read mode and all query bounds.
    pub query: WidgetDataQuery,
}

/// Closed read query forms admitted by Widget UX.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum WidgetDataQuery {
    /// Read a bounded page of objects.
    ObjectList {
        /// Filters over declared properties.
        filters: Vec<WidgetFilter>,
        /// Sort keys, in priority order.
        sort: Vec<WidgetSort>,
        /// Required page size and hard maximum.
        page: WidgetPageBounds,
    },
    /// Read one object selected by all members of one declared identity key.
    ObjectDetail { key: Vec<WidgetKeyValue> },
    /// Traverse one declared EntityIR v2 link from an earlier binding.
    LinkTraversal {
        /// Source binding whose object owns the link.
        source_binding_id: String,
        /// Link id on the source object's EntityIR v2 definition.
        link_id: String,
        /// Maximum number of links to follow.
        max_depth: WholeNumber,
        /// Hard cap on returned objects.
        max_rows: WholeNumber,
        /// Required page size and hard maximum.
        page: WidgetPageBounds,
    },
    /// Read one object by its runtime key, supplied by a required text page
    /// parameter marked with the binding's exact `runtimeKeyOf` entity.
    ObjectRuntimeDetail {
        /// Page owning the runtime-key parameter.
        page_id: String,
        /// Required, entity-marked runtime-key parameter.
        parameter_id: String,
    },
}

/// One member of the exact EntityIR identity key selecting an object detail.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WidgetKeyValue {
    /// Property id in the bound entity's declared identity key.
    pub property_id: String,
    /// Literal or typed page context value for this key member.
    pub value: WidgetFilterValue,
}

/// One property filter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WidgetFilter {
    /// Property checked by the filter.
    pub property: PropertyRef,
    /// Closed comparison operation.
    pub operator: WidgetFilterOperator,
    /// Literal or declared page context value checked against the property type.
    pub value: WidgetFilterValue,
}

/// Supported property filter operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WidgetFilterOperator {
    /// Equal.
    Eq,
    /// Not equal.
    Neq,
    /// Less than.
    Lt,
    /// Less than or equal.
    Lte,
    /// Greater than.
    Gt,
    /// Greater than or equal.
    Gte,
    /// A member of the supplied set.
    In,
}

/// A filter value.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum WidgetFilterValue {
    /// A JSON literal, checked against the referenced property type.
    Literal { value: JsonValue },
    /// A typed parameter declared by a page.
    ContextParameter {
        /// Page declaring the parameter.
        page_id: String,
        /// Parameter declared by that page.
        parameter_id: String,
    },
    /// Value on the binding's current detail record, row, or single selection.
    /// The reserved property id `$key` denotes the authoritative runtime object key.
    Record { property: PropertyRef },
}

#[derive(Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum WidgetFilterValueWire {
    Literal {
        value: JsonValue,
    },
    ContextParameter {
        page_id: String,
        parameter_id: String,
    },
    Record {
        property: PropertyRef,
    },
}

impl From<WidgetFilterValueWire> for WidgetFilterValue {
    fn from(wire: WidgetFilterValueWire) -> Self {
        match wire {
            WidgetFilterValueWire::Literal { value } => Self::Literal { value },
            WidgetFilterValueWire::ContextParameter {
                page_id,
                parameter_id,
            } => Self::ContextParameter {
                page_id,
                parameter_id,
            },
            WidgetFilterValueWire::Record { property } => Self::Record { property },
        }
    }
}

impl<'de> Deserialize<'de> for WidgetFilterValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        if is_serde_json_value_deserializer::<D>() {
            let value = serde_json::Value::deserialize(deserializer)?;
            guard_widget_ux_numeric_value(&value).map_err(serde::de::Error::custom)?;
            let wire: WidgetFilterValueWire =
                serde_json::from_value(value).map_err(serde::de::Error::custom)?;
            Ok(Self::from(wire))
        } else if is_serde_json_text_deserializer::<D>() && deserializer.is_human_readable() {
            let raw = Box::<RawValue>::deserialize(deserializer)?;
            guard_widget_ux_numeric_roundtrip(raw.get()).map_err(serde::de::Error::custom)?;
            let wire: WidgetFilterValueWire =
                serde_json::from_str(raw.get()).map_err(serde::de::Error::custom)?;
            Ok(Self::from(wire))
        } else {
            WidgetFilterValueWire::deserialize(deserializer).map(Self::from)
        }
    }
}

/// One property sort key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WidgetSort {
    /// Property used for sorting.
    pub property: PropertyRef,
    /// Sort direction.
    pub direction: WidgetSortDirection,
}

/// Sort direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WidgetSortDirection {
    /// Ascending.
    Asc,
    /// Descending.
    Desc,
}

/// A requested page size and the hard maximum the host must enforce.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WidgetPageBounds {
    /// Requested page size, positive and no larger than `maxSize`.
    pub size: WholeNumber,
    /// Maximum page size, positive and enforced by the host.
    pub max_size: WholeNumber,
}

/// A typed action binding in Widget UX.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WidgetUxActionBinding {
    /// Stable binding id.
    pub binding_id: String,
    /// Exact ActionIR artifact to invoke.
    pub action: ArtifactRef,
    /// Action input member to ZEN expression mapping.
    pub input_mapping: BTreeMap<String, ZenExpression>,
    /// Explicit sources available to each input expression as `input.record`,
    /// `input.draft`, `input.selection`, and `input.context`. Child-row field
    /// conditions may additionally name their enclosing grid to supply `input.row`.
    pub inputs: WidgetExpressionInputs,
    /// Capability required by the UI before presenting or enabling the action.
    /// This is a requirement and never grants the capability.
    pub required_capability: String,
}

/// Explicit typed sources available under the ZEN `input` record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WidgetExpressionInputs {
    /// A data binding that supplies `input.record`.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub record_binding_id: Option<String>,
    /// A form whose explicit action input members supply `input.draft`.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub draft_form_id: Option<String>,
    /// A list binding that supplies selected records as `input.selection`.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub selection_binding_id: Option<String>,
    /// A page whose declared context parameters supply `input.context`.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub context_page_id: Option<String>,
    /// ChildGrid node whose current stable row supplies `input.row` in that
    /// grid's child-row field conditions only.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub row_grid_id: Option<String>,
}

/// A Boolean ZEN condition over explicitly named Widget UX inputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ZenCondition {
    /// ZEN source, type-checked to Boolean by `norn-checker`.
    pub expression: ZenExpression,
    /// Declared records available to the condition.
    pub inputs: WidgetExpressionInputs,
}

/// A localized presentation of one property or explicitly included form input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WidgetFieldPresentation {
    /// Stable field presentation id.
    pub field_id: String,
    /// Explicit object property or included form input presented by this field.
    pub target: WidgetFieldTarget,
    /// Localized visible label.
    pub label: LocaleText,
    /// Optional localized help text.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "LocaleText")]
    pub help: Option<LocaleText>,
    /// Control selected for the property's value type.
    pub control: WidgetFieldControl,
    /// Optional display format hint.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "WidgetFieldFormat")]
    pub format: Option<WidgetFieldFormat>,
    /// Data binding that supplies options for a select or link control.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub option_query_binding_id: Option<String>,
    /// Whether this field is visible.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "ZenCondition")]
    pub visible_when: Option<ZenCondition>,
    /// Whether this field is required.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "ZenCondition")]
    pub required_when: Option<ZenCondition>,
    /// Whether this field is read-only.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "ZenCondition")]
    pub read_only_when: Option<ZenCondition>,
}

/// The authoritative source of a field's value and type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum WidgetFieldTarget {
    /// A resolved property on an exact data binding.
    Property { property: PropertyRef },
    /// One included ActionIR input of a form, with an optional typed prefill.
    FormInput {
        /// Form whose submit action owns this input.
        form_id: String,
        /// Input member explicitly included by `WidgetForm.inputNames`.
        input_name: String,
        /// Property whose current record value initializes the local draft.
        #[serde(
            default,
            deserialize_with = "present_non_null",
            skip_serializing_if = "Option::is_none"
        )]
        #[schemars(with = "PropertyRef")]
        initial: Option<PropertyRef>,
    },
    /// One property nested in the parent-values object owned by a parent+rows form.
    ParentProperty {
        /// Form whose submit action owns the parent values.
        form_id: String,
        /// Included ActionIR object input populated by this property.
        input_name: String,
        /// Property on the action's pinned parent entity.
        property_id: String,
    },
    /// One editable property on each child row in an ordered grid.
    ChildRowProperty {
        /// Form whose submit action owns the child collection.
        form_id: String,
        /// Owned link id declared by the form action's pinned entity.
        collection_id: String,
        /// Property on the child entity reached by the grid's traversal.
        property_id: String,
    },
}

/// Closed set of field controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WidgetFieldControl {
    /// Single-line text entry.
    Text,
    /// Multi-line text entry.
    TextArea,
    /// Numeric entry.
    Number,
    /// Calendar date entry.
    Date,
    /// Zoned date and time entry.
    DateTime,
    /// Boolean checkbox.
    Checkbox,
    /// Closed option selector.
    Select,
    /// Reference selector backed by an option query.
    Link,
    /// Read-only value display.
    Display,
}

/// Closed set of display-format hints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WidgetFieldFormat {
    /// Plain text.
    Text,
    /// Integer.
    Integer,
    /// Decimal.
    Decimal,
    /// Currency.
    Currency,
    /// Percentage.
    Percent,
    /// Calendar date.
    Date,
    /// Date and time.
    DateTime,
    /// Boolean.
    Boolean,
    /// Accepted metric buckets, displayed without calculating business metrics.
    /// Only valid on display presentations bound to a json-ext property.
    MetricBuckets,
}

/// A form that submits through a declared Norn action binding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WidgetForm {
    /// Stable form id.
    pub form_id: String,
    /// Whether the form creates an object, edits one, or supplies action input.
    pub mode: WidgetFormMode,
    /// ActionIR input members explicitly included by this form. This includes
    /// action-only values, such as a reason or note, that are not entity fields.
    pub input_names: Vec<String>,
    /// Read context for typed prefill; required object detail for edit mode.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub data_binding_id: Option<String>,
    /// Action binding used to submit this form.
    pub submit_binding_id: String,
    /// Presentation and confirmation for the form's own effectful submit.
    pub submit: WidgetFormSubmit,
    /// Version field and conflict behavior for optimistic edit submission.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "WidgetVersionConflictBinding")]
    pub version_conflict_binding: Option<WidgetVersionConflictBinding>,
}

/// The single submit control owned by a form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WidgetFormSubmit {
    /// Localized submit label.
    pub label: LocaleText,
    /// Confirmation applied when submitting, after input validation.
    pub confirmation: WidgetActionConfirmation,
    /// Effects handled specifically by the submit invocation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub handlers: Vec<WidgetEffectHandler>,
}

/// Form modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WidgetFormMode {
    /// Create input submitted through an ActionIR action.
    Create,
    /// Edit input for a bound record, submitted through an ActionIR action.
    Edit,
    /// Input to an action unrelated to direct object create or edit.
    ActionInput,
}

/// Version property used to detect stale edits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WidgetVersionConflictBinding {
    /// Version or revision property on the edited object.
    pub property: PropertyRef,
    /// Action input member that receives the expected version.
    pub input_name: String,
    /// How the host reports a version conflict.
    pub on_conflict: WidgetConflictBehavior,
}

/// Optimistic edit conflict behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WidgetConflictBehavior {
    /// Reject the submission and retain the draft for review.
    RejectAndRetainDraft,
}

/// A placement of a bound action in a page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WidgetActionPlacement {
    /// Stable placement id.
    pub placement_id: String,
    /// Action binding to place.
    pub binding_id: String,
    /// Localized text shown on the action control.
    pub label: LocaleText,
    /// Opening this form defers invocation until its explicit submit.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub form_id: Option<String>,
    /// Page location that hosts the action.
    pub location: WidgetActionLocation,
    /// Order within the location.
    pub order: WholeNumber,
    /// Optional group label for related actions.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub group: Option<String>,
    /// List binding that provides a selected row or selection set.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub selection_binding_id: Option<String>,
    /// Selected values mapped to the action input members.
    pub selection_input_mapping: BTreeMap<String, ZenExpression>,
    /// Boolean condition required to enable the placement.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "ZenCondition")]
    pub enabled_when: Option<ZenCondition>,
    /// Required severity and confirmation strength for this placement.
    pub confirmation: WidgetActionConfirmation,
    /// Whether this placement exposes a retry affordance.
    #[serde(default, skip_serializing_if = "is_false")]
    pub retry: bool,
    /// Effects handled specifically by this invocation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub handlers: Vec<WidgetEffectHandler>,
}

const fn is_false(value: &bool) -> bool {
    !*value
}

/// Supported action locations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WidgetActionLocation {
    /// Page toolbar.
    Toolbar,
    /// A row in a list.
    Row,
    /// A selected set of list rows.
    Selection,
    /// Object detail surface.
    Detail,
    /// Form footer.
    FormFooter,
}

/// Action severity and confirmation strength.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WidgetActionConfirmation {
    /// Impact if the action is executed.
    pub severity: WidgetActionSeverity,
    /// User confirmation required before execution.
    pub strength: WidgetConfirmationStrength,
    /// Optional localized explanation of the action's consequences.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "LocaleText")]
    pub message: Option<LocaleText>,
    /// Required localized phrase for typed confirmation; absent otherwise.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "LocaleText")]
    pub phrase: Option<LocaleText>,
}

/// Impact category of an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WidgetActionSeverity {
    /// Safe, routine action.
    Safe,
    /// Action should be confirmed.
    Confirm,
    /// Destructive action.
    Destructive,
    /// Irreversible action.
    Irreversible,
}

/// Confirmation interaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WidgetConfirmationStrength {
    /// No confirmation.
    None,
    /// A click or equivalent explicit confirmation.
    Click,
    /// Typed confirmation phrase.
    Typed,
}

/// A typed navigation item with an authorization requirement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WidgetNavigationItem {
    /// Stable navigation item id.
    pub item_id: String,
    /// Localized item label.
    pub label: LocaleText,
    /// Closed destination kind.
    pub destination: WidgetNavigationDestination,
    /// Capability required to show or follow the item; never a grant.
    pub required_capability: String,
    /// Boolean condition required to show the item.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "ZenCondition")]
    pub visible_when: Option<ZenCondition>,
}

/// Supported navigation destinations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum WidgetNavigationDestination {
    /// A page declared in this UX contract.
    Page {
        page_id: String,
        /// Values supplied to the destination page's declared context.
        parameters: BTreeMap<String, WidgetFilterValue>,
    },
    /// A typed object destination with its exact entity and read binding.
    Object {
        object: ArtifactRef,
        data_binding_id: String,
    },
    /// A finite host-native projection over the exact bound object.
    NativeProjection {
        projection: WidgetNativeProjection,
        data_binding_id: String,
    },
}

/// Host-native projections available to Widget UX.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WidgetNativeProjection {
    /// Inspect an object and its declared properties.
    ObjectInspector,
    /// Read the object's execution history.
    History,
    /// Explain the object's decision provenance.
    Why,
    /// Inspect the capabilities required for the object.
    Permissions,
}

/// One bounded override of a composed baseline component.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WidgetOverride {
    /// Existing component semantic id; orphan targets are checker errors.
    pub target_semantic_id: String,
    /// Closed customization operation.
    pub value: WidgetOverrideValue,
}

/// Bounded override vocabulary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum WidgetOverrideValue {
    /// Replace localized label text.
    Label { value: LocaleText },
    /// Change sibling order.
    Order { value: WholeNumber },
    /// Change grouping label.
    Grouping { value: String },
    /// Add or replace a visibility condition.
    Visibility { condition: ZenCondition },
    /// Select a supported control.
    Control { control: WidgetFieldControl },
    /// Select an action placement by id.
    ActionPlacement { placement_id: String },
}
