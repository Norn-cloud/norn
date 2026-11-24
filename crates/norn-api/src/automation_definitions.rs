//! AUTOMATION-1: retained `automation` definitions — declarative When / If /
//! Then rules admitted and run exclusively on the server.
//!
//! Payload shape:
//! - `title`: 1..120 chars
//! - `description?`: 0..280 chars
//! - `enabled`: bool
//! - `objectType`: declared object type in the cell's model
//! - `when`:
//!   - `{ kind: "event", event: "created" | "updated" | "action", actionId? }`
//!   - `{ kind: "lifecycle_entered", property, state }`
//!   - `{ kind: "stale", afterMs, property?, state? }`
//! - `if?`: `{ expression: string }` — checked by `norn-expr` against `self`
//!   of `objectType` and required to produce `bool`
//! - `then`:
//!   - `{ kind: "propose", actionId, input?, question, risk, decider: { roles, law? }, expiresInMs? }`
//!   - `{ kind: "run", actionId, input? }`

use std::collections::{BTreeMap, BTreeSet};

use norn_cell::engine::SqlEngine;
use norn_ir::{ActionEffectProgram, ActionRoot, PropertyType};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::definition_routes::{MAX_TITLE_CHARS, codes};
use crate::host::DevHost;
use crate::refusal::{Refusal, RefusalDiagnostic};

/// Maximum characters in an automation description.
pub const MAX_DESCRIPTION_CHARS: usize = 280;
/// Maximum characters in a guard expression.
pub const MAX_EXPRESSION_CHARS: usize = 1024;
/// Maximum characters in a proposal question.
pub const MAX_QUESTION_CHARS: usize = 240;
/// Minimum staleness threshold (1 second).
pub const MIN_STALE_MS: u64 = 1_000;
/// Maximum staleness threshold (365 days).
pub const MAX_STALE_MS: u64 = 365 * 24 * 60 * 60 * 1000;
/// Maximum expiry for a proposed Inbox decision (7 days).
pub const MAX_EXPIRES_IN_MS: u64 = norn_cell::decision_ledger::MAX_EXPIRES_IN_MS;

/// Stable `definition.automation_*` refusal codes.
pub mod automation_codes {
    /// The automation payload is malformed or fails validation.
    pub const INVALID: &str = "definition.automation_invalid";
    /// The guard expression failed `norn-expr` type or profile checking.
    pub const GUARD_INVALID: &str = "definition.automation_guard_invalid";
    /// The referenced action is not admitted for this object type.
    pub const ACTION_UNKNOWN: &str = "definition.automation_action_unknown";
    /// A decider role is not in the cell's declared role table.
    pub const ROLE_UNKNOWN: &str = "definition.automation_role_unknown";
}

/// Decoded automation definition payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AutomationPayload {
    /// Human-readable title.
    pub title: String,
    /// Optional description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Whether this automation is active and fires on triggers.
    pub enabled: bool,
    /// The declared object type this automation observes.
    pub object_type: String,
    /// When the automation triggers.
    pub when: AutomationWhen,
    /// Optional ZEN boolean guard evaluated over `self`.
    #[serde(rename = "if", default, skip_serializing_if = "Option::is_none")]
    pub guard: Option<AutomationGuard>,
    /// What happens when `when` and `if` hold.
    pub then: AutomationThen,
}

/// Trigger specification (`when`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AutomationWhen {
    /// Trigger on an object lifecycle/action event.
    #[serde(rename_all = "camelCase")]
    Event {
        /// `created`, `updated`, or `action`.
        event: String,
        /// Required when `event == "action"`: the admitted action id.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action_id: Option<String>,
    },
    /// Trigger when a lifecycle property enters a specific state.
    #[serde(rename_all = "camelCase")]
    LifecycleEntered {
        /// Declared lifecycle property on `objectType`.
        property: String,
        /// Target lifecycle state name.
        state: String,
    },
    /// Trigger when a record has remained unchanged for at least `afterMs`.
    #[serde(rename_all = "camelCase")]
    Stale {
        /// Staleness duration in milliseconds.
        after_ms: u64,
        /// Optional enum/lifecycle property that must match `state`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        property: Option<String>,
        /// Optional state value for `property`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        state: Option<String>,
    },
}

/// Optional boolean guard expression (`if`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AutomationGuard {
    /// Boolean ZEN expression evaluated over `self`.
    pub expression: String,
}

/// Who may decide a proposed Inbox item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AutomationDecider {
    /// Declared roles from the cell's grant table.
    pub roles: Vec<String>,
    /// Optional decider law.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub law: Option<String>,
}

/// What the automation does (`then`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AutomationThen {
    /// Propose an action into the Inbox for a human decision.
    #[serde(rename_all = "camelCase")]
    Propose {
        /// Admitted action id targeting `objectType`.
        action_id: String,
        /// Static arguments merged with the matched record's target id field.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        input: BTreeMap<String, Value>,
        /// Question shown in the Inbox.
        question: String,
        /// `low`, `medium`, or `high`.
        risk: String,
        /// Roles allowed to decide the proposal.
        decider: AutomationDecider,
        /// Optional expiry in milliseconds (default 7 days).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expires_in_ms: Option<u64>,
    },
    /// Run an action directly under the cell's normal admission (laws and
    /// authority always apply).
    #[serde(rename_all = "camelCase")]
    Run {
        /// Admitted action id targeting `objectType`.
        action_id: String,
        /// Static arguments merged with the matched record's target id field.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        input: BTreeMap<String, Value>,
    },
}

impl AutomationThen {
    /// The admitted action id this `then` branch targets.
    #[must_use]
    pub fn action_id(&self) -> &str {
        match self {
            Self::Propose { action_id, .. } | Self::Run { action_id, .. } => action_id,
        }
    }

    /// Static input arguments declared on the `then` branch.
    #[must_use]
    pub const fn input(&self) -> &BTreeMap<String, Value> {
        match self {
            Self::Propose { input, .. } | Self::Run { input, .. } => input,
        }
    }
}

const TOP_MEMBERS: [&str; 7] = [
    "title",
    "description",
    "enabled",
    "objectType",
    "when",
    "if",
    "then",
];

fn err(code: &str, message: impl Into<String>, pointer: impl Into<String>) -> RefusalDiagnostic {
    RefusalDiagnostic::error(code, message, pointer)
}

/// Check a retained `kind: "automation"` payload against the cell's active
/// model, admitted actions, `norn-expr` type environment, and role table.
///
/// # Errors
/// A host failure reading the cell's model or active definitions.
pub(crate) fn check<E: SqlEngine>(
    host: &DevHost<E>,
    value: &Value,
    roles: &[&str],
    findings: &mut Vec<RefusalDiagnostic>,
) -> Result<(), Refusal> {
    let Some(object) = value.as_object() else {
        findings.push(err(
            automation_codes::INVALID,
            "an automation definition must be a JSON object",
            "",
        ));
        return Ok(());
    };
    for key in object
        .keys()
        .filter(|key| !TOP_MEMBERS.contains(&key.as_str()))
    {
        findings.push(err(
            automation_codes::INVALID,
            format!("unknown member {key:?}"),
            format!("/{key}"),
        ));
    }
    check_text(
        object.get("title"),
        "/title",
        (1, MAX_TITLE_CHARS),
        "title",
        codes::TILE_TITLE_INVALID,
        findings,
    );
    if object.contains_key("description") {
        check_text(
            object.get("description"),
            "/description",
            (0, MAX_DESCRIPTION_CHARS),
            "description",
            automation_codes::INVALID,
            findings,
        );
    }
    if !object.get("enabled").is_some_and(Value::is_boolean) {
        findings.push(err(
            automation_codes::INVALID,
            "enabled must be true or false",
            "/enabled",
        ));
    }
    let Some(object_type) = object
        .get("objectType")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
    else {
        findings.push(err(
            automation_codes::INVALID,
            "objectType must name a declared object type",
            "/objectType",
        ));
        return Ok(());
    };
    let (model_snapshot, properties, lifecycles, actions) = {
        let cell = host.cell()?;
        let reader = cell.store().read();
        if !reader.model().has_type(object_type) {
            findings.push(err(
                automation_codes::INVALID,
                format!("objectType {object_type:?} is not declared in this cell"),
                "/objectType",
            ));
            return Ok(());
        }
        let snapshot = reader.model().definitions_snapshot();
        let properties = reader
            .model()
            .effective_property_types(object_type)
            .map_err(Refusal::host)?;
        let lifecycles = declared_lifecycle_states(reader.model(), object_type);
        let actions = actions_map(host, &reader, object_type)?;
        (snapshot, properties, lifecycles, actions)
    };
    check_when(
        object.get("when"),
        object_type,
        &properties,
        &lifecycles,
        &actions,
        findings,
    );
    if let Some(guard) = object.get("if") {
        check_if(guard, &model_snapshot, object_type, findings);
    }
    check_then(object.get("then"), object_type, &actions, roles, findings);
    Ok(())
}

/// Deterministic plain-words restatement of an automation payload (D59).
///
/// # Errors
/// The payload cannot be parsed as an [`AutomationPayload`].
pub fn restate(canonical: &str) -> Result<String, String> {
    let payload: AutomationPayload =
        serde_json::from_str(canonical).map_err(|error| error.to_string())?;
    let when_text = match &payload.when {
        AutomationWhen::Event { event, action_id } => match action_id {
            Some(action_id) => {
                format!("when action {action_id} runs on {}", payload.object_type)
            }
            None => format!("when {} is {event}", payload.object_type),
        },
        AutomationWhen::LifecycleEntered { property, state } => {
            format!("when {} enters {property} = {state}", payload.object_type)
        }
        AutomationWhen::Stale {
            after_ms,
            property,
            state,
        } => {
            let duration = format_duration(*after_ms);
            match (property, state) {
                (Some(property), Some(state)) => format!(
                    "when {} stays in {property} = {state} for {duration}",
                    payload.object_type
                ),
                _ => format!("when {} is unchanged for {duration}", payload.object_type),
            }
        }
    };
    let if_text = payload
        .guard
        .as_ref()
        .map_or_default(|guard| format!(", if {}", guard.expression.trim()));
    let then_text = match &payload.then {
        AutomationThen::Propose {
            action_id,
            risk,
            decider,
            ..
        } => format!(
            "propose {action_id} to {} ({risk} risk)",
            decider.roles.join(", ")
        ),
        AutomationThen::Run { action_id, .. } => {
            format!("run {action_id} under the cell's normal laws and authority")
        }
    };
    let state_text = if payload.enabled {
        "enabled"
    } else {
        "disabled"
    };
    Ok(format!(
        "Automation \"{}\" ({state_text}): {when_text}{if_text}, then {then_text}.",
        payload.title.trim()
    ))
}

fn format_duration(ms: u64) -> String {
    const HOUR: u64 = 3_600_000;
    const DAY: u64 = 24 * HOUR;
    if ms >= DAY && ms.is_multiple_of(DAY) {
        let days = ms / DAY;
        return format!("{days} {}", if days == 1 { "day" } else { "days" });
    }
    if ms >= HOUR && ms.is_multiple_of(HOUR) {
        let hours = ms / HOUR;
        return format!("{hours} {}", if hours == 1 { "hour" } else { "hours" });
    }
    if ms >= 60_000 && ms.is_multiple_of(60_000) {
        let minutes = ms / 60_000;
        return format!("{minutes} min");
    }
    format!("{} s", ms / 1_000)
}

fn check_text(
    value: Option<&Value>,
    at: &str,
    (min, max): (usize, usize),
    label: &str,
    code: &str,
    findings: &mut Vec<RefusalDiagnostic>,
) {
    let ok = value
        .and_then(Value::as_str)
        .map(str::trim)
        .is_some_and(|text| {
            let chars = text.chars().count();
            (min..=max).contains(&chars) && !text.chars().any(char::is_control)
        });
    if !ok {
        findings.push(err(
            code,
            format!("{label} must be {min} to {max} characters without control characters"),
            at,
        ));
    }
}

fn check_when(
    value: Option<&Value>,
    object_type: &str,
    properties: &[(String, Option<PropertyType>)],
    lifecycles: &BTreeMap<String, BTreeSet<String>>,
    actions: &BTreeMap<String, ActionRoot>,
    findings: &mut Vec<RefusalDiagnostic>,
) {
    let Some(when) = value.and_then(Value::as_object) else {
        findings.push(err(
            automation_codes::INVALID,
            "when must be a trigger object",
            "/when",
        ));
        return;
    };
    match when.get("kind").and_then(Value::as_str) {
        Some("event") => check_when_event(when, object_type, actions, findings),
        Some("lifecycle_entered") => {
            check_allowed_keys(when, "/when", &["kind", "property", "state"], findings);
            let prop = when.get("property").and_then(Value::as_str);
            let state = when.get("state").and_then(Value::as_str);
            match prop.and_then(|name| lifecycles.get(name)) {
                Some(states) => {
                    if state.is_none_or(|s| !states.contains(s)) {
                        findings.push(err(
                            automation_codes::INVALID,
                            "state must be a declared state of that lifecycle",
                            "/when/state",
                        ));
                    }
                }
                None => findings.push(err(
                    automation_codes::INVALID,
                    format!("property must name a declared lifecycle of {object_type}"),
                    "/when/property",
                )),
            }
        }
        Some("stale") => check_when_stale(when, object_type, properties, lifecycles, findings),
        _ => findings.push(err(
            automation_codes::INVALID,
            "when.kind must be \"event\", \"lifecycle_entered\", or \"stale\"",
            "/when/kind",
        )),
    }
}

fn check_when_event(
    when: &serde_json::Map<String, Value>,
    object_type: &str,
    actions: &BTreeMap<String, ActionRoot>,
    findings: &mut Vec<RefusalDiagnostic>,
) {
    check_allowed_keys(when, "/when", &["kind", "event", "actionId"], findings);
    match when.get("event").and_then(Value::as_str) {
        Some("created" | "updated") => {
            if when.get("actionId").is_some_and(|v| !v.is_null()) {
                findings.push(err(
                    automation_codes::INVALID,
                    "actionId is only allowed when event is \"action\"",
                    "/when/actionId",
                ));
            }
        }
        Some("action") => match when.get("actionId").and_then(Value::as_str) {
            Some(action_id) if actions.contains_key(action_id) => {}
            Some(action_id) => findings.push(err(
                automation_codes::ACTION_UNKNOWN,
                format!("action {action_id:?} is not an admitted action targeting {object_type}"),
                "/when/actionId",
            )),
            None => findings.push(err(
                automation_codes::INVALID,
                "actionId is required when event is \"action\"",
                "/when/actionId",
            )),
        },
        _ => findings.push(err(
            automation_codes::INVALID,
            "event must be \"created\", \"updated\", or \"action\"",
            "/when/event",
        )),
    }
}

fn check_when_stale(
    when: &serde_json::Map<String, Value>,
    object_type: &str,
    properties: &[(String, Option<PropertyType>)],
    lifecycles: &BTreeMap<String, BTreeSet<String>>,
    findings: &mut Vec<RefusalDiagnostic>,
) {
    check_allowed_keys(
        when,
        "/when",
        &["kind", "afterMs", "property", "state"],
        findings,
    );
    let after_ok = when
        .get("afterMs")
        .and_then(Value::as_u64)
        .is_some_and(|ms| (MIN_STALE_MS..=MAX_STALE_MS).contains(&ms));
    if !after_ok {
        findings.push(err(
            automation_codes::INVALID,
            format!("afterMs must be between {MIN_STALE_MS} and {MAX_STALE_MS}"),
            "/when/afterMs",
        ));
    }
    let prop = when.get("property").and_then(Value::as_str);
    let state = when.get("state").and_then(Value::as_str);
    match (prop, state) {
        (None, None) => {}
        (Some(prop_name), Some(state_val)) => {
            let valid = lifecycles
                .get(prop_name)
                .is_some_and(|states| states.contains(state_val))
                || properties.iter().any(|(name, ty)| {
                    name == prop_name
                        && matches!(ty, Some(PropertyType::Enum { values }) if values.iter().any(|v| v.value == state_val))
                });
            if !valid {
                findings.push(err(
                    automation_codes::INVALID,
                    format!(
                        "property {prop_name:?} and state {state_val:?} must match a declared lifecycle or enum on {object_type}"
                    ),
                    "/when/state",
                ));
            }
        }
        _ => findings.push(err(
            automation_codes::INVALID,
            "stale property and state must be given together or both omitted",
            "/when/property",
        )),
    }
}

fn check_if(
    value: &Value,
    snapshot: &norn_cell::ModelDefinitionSnapshot,
    object_type: &str,
    findings: &mut Vec<RefusalDiagnostic>,
) {
    let Some(guard) = value.as_object() else {
        findings.push(err(
            automation_codes::GUARD_INVALID,
            "if must be an object with an expression string",
            "/if",
        ));
        return;
    };
    check_allowed_keys(guard, "/if", &["expression"], findings);
    let Some(expression) = guard
        .get("expression")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty() && text.chars().count() <= MAX_EXPRESSION_CHARS)
    else {
        findings.push(err(
            automation_codes::GUARD_INVALID,
            format!("expression must be 1 to {MAX_EXPRESSION_CHARS} characters"),
            "/if/expression",
        ));
        return;
    };
    if let Err(errors) = norn_checker::guard_expr::check_record_guard(
        &snapshot.object_types,
        &snapshot.interfaces,
        &snapshot.property_types,
        object_type,
        expression,
    ) {
        for message in errors {
            findings.push(err(
                automation_codes::GUARD_INVALID,
                message,
                "/if/expression",
            ));
        }
    }
}

fn check_then(
    value: Option<&Value>,
    object_type: &str,
    actions: &BTreeMap<String, ActionRoot>,
    roles: &[&str],
    findings: &mut Vec<RefusalDiagnostic>,
) {
    let Some(then) = value.and_then(Value::as_object) else {
        findings.push(err(
            automation_codes::INVALID,
            "then must be an object",
            "/then",
        ));
        return;
    };
    let kind = then.get("kind").and_then(Value::as_str);
    match kind {
        Some("propose") => {
            check_allowed_keys(
                then,
                "/then",
                &[
                    "kind",
                    "actionId",
                    "input",
                    "question",
                    "risk",
                    "decider",
                    "expiresInMs",
                ],
                findings,
            );
            check_then_action(then, object_type, actions, findings);
            check_text(
                then.get("question"),
                "/then/question",
                (1, MAX_QUESTION_CHARS),
                "question",
                automation_codes::INVALID,
                findings,
            );
            if !matches!(
                then.get("risk").and_then(Value::as_str),
                Some("low" | "medium" | "high")
            ) {
                findings.push(err(
                    automation_codes::INVALID,
                    "risk must be \"low\", \"medium\", or \"high\"",
                    "/then/risk",
                ));
            }
            check_decider(then.get("decider"), roles, findings);
            if then.get("expiresInMs").is_some_and(|v| {
                v.as_u64()
                    .is_none_or(|ms| !(MIN_STALE_MS..=MAX_EXPIRES_IN_MS).contains(&ms))
            }) {
                findings.push(err(
                    automation_codes::INVALID,
                    format!("expiresInMs must be between {MIN_STALE_MS} and {MAX_EXPIRES_IN_MS}"),
                    "/then/expiresInMs",
                ));
            }
        }
        Some("run") => {
            check_allowed_keys(then, "/then", &["kind", "actionId", "input"], findings);
            check_then_action(then, object_type, actions, findings);
        }
        _ => findings.push(err(
            automation_codes::INVALID,
            "then.kind must be \"propose\" or \"run\"",
            "/then/kind",
        )),
    }
}

fn check_then_action(
    then: &serde_json::Map<String, Value>,
    object_type: &str,
    actions: &BTreeMap<String, ActionRoot>,
    findings: &mut Vec<RefusalDiagnostic>,
) {
    let action = if let Some(action_id) = then.get("actionId").and_then(Value::as_str) {
        if let Some(action) = actions.get(action_id) {
            Some(action)
        } else {
            findings.push(err(
                automation_codes::ACTION_UNKNOWN,
                format!("action {action_id:?} is not an admitted action targeting {object_type}"),
                "/then/actionId",
            ));
            None
        }
    } else {
        findings.push(err(
            automation_codes::INVALID,
            "actionId must name an admitted action",
            "/then/actionId",
        ));
        None
    };
    if let Some(input) = then.get("input") {
        let Some(map) = input.as_object() else {
            findings.push(err(
                automation_codes::INVALID,
                "input must be a JSON object",
                "/then/input",
            ));
            return;
        };
        if let Some(action) = action {
            let derived = norn_kernel::actor_derived_fields(action);
            for key in map.keys() {
                if derived.contains(key) {
                    findings.push(err(
                        automation_codes::INVALID,
                        format!("input field {key:?} is derived from the actor by the server"),
                        format!("/then/input/{key}"),
                    ));
                }
            }
        }
    }
}

fn check_decider(value: Option<&Value>, roles: &[&str], findings: &mut Vec<RefusalDiagnostic>) {
    let Some(decider) = value.and_then(Value::as_object) else {
        findings.push(err(
            automation_codes::INVALID,
            "decider must be {roles: [...], law?}",
            "/then/decider",
        ));
        return;
    };
    check_allowed_keys(decider, "/then/decider", &["roles", "law"], findings);
    if decider.get("law").is_some_and(|v| !v.is_string()) {
        findings.push(err(
            automation_codes::INVALID,
            "decider.law must be a string",
            "/then/decider/law",
        ));
    }
    let named: Vec<&str> = decider
        .get("roles")
        .and_then(Value::as_array)
        .map_or_default(|items| items.iter().filter_map(Value::as_str).collect());
    if named.is_empty() {
        findings.push(err(
            automation_codes::INVALID,
            "decider.roles must name at least one role",
            "/then/decider/roles",
        ));
        return;
    }
    for (index, role) in named.iter().enumerate() {
        if !roles.contains(role) {
            findings.push(err(
                automation_codes::ROLE_UNKNOWN,
                format!("role {role:?} is not in this cell's role table"),
                format!("/then/decider/roles/{index}"),
            ));
        }
    }
}

fn check_allowed_keys(
    object: &serde_json::Map<String, Value>,
    prefix: &str,
    allowed: &[&str],
    findings: &mut Vec<RefusalDiagnostic>,
) {
    for key in object.keys().filter(|key| !allowed.contains(&key.as_str())) {
        findings.push(err(
            automation_codes::INVALID,
            format!("unknown member {key:?}"),
            format!("{prefix}/{key}"),
        ));
    }
}

fn declared_lifecycle_states(
    model: &norn_cell::Model,
    object_type: &str,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut map: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for def in model
        .object_types()
        .filter(|def| def.name.as_str() == object_type)
    {
        for lifecycle in &def.lifecycles {
            let entry = map
                .entry(lifecycle.property.as_str().to_owned())
                .or_default();
            for state in &lifecycle.initial {
                entry.insert(state.clone());
            }
            for state in &lifecycle.terminal {
                entry.insert(state.clone());
            }
            for transition in &lifecycle.transitions {
                for from in &transition.from {
                    entry.insert(from.clone());
                }
                entry.insert(transition.to.clone());
            }
        }
    }
    map
}

pub(crate) fn actions_map<E: SqlEngine>(
    host: &DevHost<E>,
    reader: &norn_cell::StoreReader<'_, E>,
    object_type: &str,
) -> Result<BTreeMap<String, ActionRoot>, Refusal> {
    let Some((_, retained)) = host.retained(reader)? else {
        return Ok(BTreeMap::new());
    };
    let mut map = BTreeMap::new();
    for (_, action) in &retained.actions {
        let target = match &action.effect_program {
            Some(
                ActionEffectProgram::UpsertInputObject { object_kind, .. }
                | ActionEffectProgram::FireTransition { object_kind, .. }
                | ActionEffectProgram::MutateParentWithRows { object_kind, .. },
            ) => Some(object_kind.as_str()),
            None => None,
        };
        if target == Some(object_type) {
            map.insert(action.action_id.clone(), action.clone());
        }
    }
    Ok(map)
}
