//! Pure admission checks for the `automation` retained-definition kind
//! (`AUTOMATION-1`).
//!
//! Validates structural shape, target object type, trigger semantics (`event`,
//! `lifecycle_entered`, `stale`), record-level ZEN `guard` expressions via
//! [`crate::guard_expr::check_record_guard`], target action compatibility, and
//! proposal decider bounds. Static action input validation runs against
//! `norn-kernel`'s closed action input validator at the host admission seam.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::{ActionRoot, InterfaceDef, ObjectTypeDef, Property, PropertyType, PropertyTypeDef};
use serde_json::Value;

/// Longest automation title, in characters after trimming.
pub const MAX_TITLE_CHARS: usize = 120;
/// Longest automation description, in characters.
pub const MAX_DESCRIPTION_CHARS: usize = 500;
/// Longest ZEN guard expression, in characters.
pub const MAX_GUARD_CHARS: usize = 2_000;
/// Longest proposal question, in characters.
pub const MAX_QUESTION_CHARS: usize = 500;
/// Shortest `stale` duration: one minute.
pub const MIN_STALE_DURATION_MS: u64 = 60_000;
/// Longest `stale` duration: 365 days.
pub const MAX_STALE_DURATION_MS: u64 = 365 * 24 * 60 * 60 * 1_000;
/// Longest proposal expiry: seven days.
pub const MAX_EXPIRES_IN_MS: u64 = 7 * 24 * 60 * 60 * 1_000;

const TOP_MEMBERS: [&str; 7] = [
    "title",
    "description",
    "enabled",
    "objectType",
    "when",
    "guard",
    "then",
];

const THEN_MEMBERS: [&str; 7] = [
    "actionId",
    "mode",
    "input",
    "decider",
    "question",
    "risk",
    "expiresInMs",
];

/// Classification of one admission issue on an automation payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomationIssueKind {
    /// Structural shape, member, range, or mode defect.
    Invalid,
    /// `objectType` is not declared in the cell's model.
    ObjectTypeUnknown,
    /// `when.actionId` or `then.actionId` is not an admitted action.
    ActionUnknown,
    /// An action's target kind does not accept the automation's `objectType`.
    ActionTargetMismatch,
    /// `when.property` is not a declared lifecycle property of `objectType`.
    LifecycleUnknown,
    /// `when.state` is not a declared state of that lifecycle.
    LifecycleStateUnknown,
    /// `when.property` for `stale` is not a declared `timestamp` or `date` property.
    StalePropertyInvalid,
    /// `guard` failed ZEN expression compilation, type, or profile checking.
    GuardInvalid,
    /// A `then.decider.roles` entry is not in the cell's declared role table.
    RoleUnknown,
}

/// One admission finding on an automation payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationIssue {
    /// Defect class.
    pub kind: AutomationIssueKind,
    /// Human-readable message.
    pub message: String,
    /// RFC 6901 JSON pointer into the payload.
    pub pointer: String,
}

/// Model, actions, and role table an automation payload is admitted against.
#[derive(Debug, Clone, Copy)]
pub struct AutomationCheckContext<'a> {
    /// Declared object types of the cell's active model.
    pub object_types: &'a [ObjectTypeDef],
    /// Declared interfaces of the cell's active model.
    pub interfaces: &'a [InterfaceDef],
    /// Declared shared property types of the cell's active model.
    pub property_types: &'a [PropertyTypeDef],
    /// Admitted actions of the cell's active definitions.
    pub actions: &'a [ActionRoot],
    /// Declared roles of the cell's role table.
    pub roles: &'a [&'a str],
}

/// Resolved target of a structurally valid automation header, used by callers
/// to run static input validation and author-grant verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedAutomationTarget {
    /// Declared `objectType` watched by the automation.
    pub object_type: String,
    /// Admitted `then.actionId` invoked by the automation.
    pub action_id: String,
}

fn issue(kind: AutomationIssueKind, message: impl Into<String>, pointer: &str) -> AutomationIssue {
    AutomationIssue {
        kind,
        message: message.into(),
        pointer: pointer.to_owned(),
    }
}

fn pointer_token(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

/// Returns the lineage of `object_type` (nearest first), or `None` if undeclared.
fn lineage<'a>(
    object_types: &'a [ObjectTypeDef],
    object_type: &str,
) -> Option<Vec<&'a ObjectTypeDef>> {
    let by_name: BTreeMap<&str, &ObjectTypeDef> = object_types
        .iter()
        .map(|def| (def.name.as_str(), def))
        .collect();
    let mut current = *by_name.get(object_type)?;
    let mut chain = Vec::new();
    let mut seen = BTreeSet::new();
    while seen.insert(current.name.as_str()) {
        chain.push(current);
        let Some(parent) = current.extends.as_ref() else {
            break;
        };
        let Some(next) = by_name.get(parent.as_str()) else {
            break;
        };
        current = *next;
    }
    Some(chain)
}

fn is_a(object_types: &[ObjectTypeDef], subtype: &str, ancestor: &str) -> bool {
    if subtype == ancestor {
        return true;
    }
    lineage(object_types, subtype).is_some_and(|chain| {
        chain
            .iter()
            .any(|object_type| object_type.name.as_str() == ancestor)
    })
}

fn effective_property<'a>(chain: &[&'a ObjectTypeDef], id: &str) -> Option<&'a Property> {
    chain
        .iter()
        .find_map(|def| def.properties.iter().find(|prop| prop.id.as_str() == id))
}

fn resolve_property_type<'a>(
    property_type: &'a PropertyType,
    property_types: &'a [PropertyTypeDef],
    depth: usize,
) -> Option<&'a PropertyType> {
    if depth > 16 {
        return None;
    }
    match property_type {
        PropertyType::Ref { name, .. } => {
            let target = property_types.iter().find(|def| def.name == *name)?;
            resolve_property_type(
                &target.property_type,
                property_types,
                depth.saturating_add(1),
            )
        }
        PropertyType::Text { .. }
        | PropertyType::Integer { .. }
        | PropertyType::Decimal { .. }
        | PropertyType::DecimalBinding { .. }
        | PropertyType::Boolean {}
        | PropertyType::Timestamp { .. }
        | PropertyType::Date {}
        | PropertyType::Duration {}
        | PropertyType::Enum { .. }
        | PropertyType::Record { .. }
        | PropertyType::JsonExt { .. } => Some(property_type),
    }
}

fn lifecycle_states(
    chain: &[&ObjectTypeDef],
    interfaces: &[InterfaceDef],
    property_types: &[PropertyTypeDef],
    property: &str,
) -> Option<BTreeSet<String>> {
    let mut found_lifecycle = false;
    let mut states = BTreeSet::new();
    let mut collect = |lifecycle: &norn_ir::Lifecycle| {
        if lifecycle.property.as_str() == property {
            found_lifecycle = true;
            states.extend(lifecycle.initial.iter().cloned());
            states.extend(lifecycle.terminal.iter().cloned());
            for transition in &lifecycle.transitions {
                states.extend(transition.from.iter().cloned());
                states.insert(transition.to.clone());
            }
        }
    };
    for def in chain {
        for lifecycle in &def.lifecycles {
            collect(lifecycle);
        }
    }
    let iface_by_name: BTreeMap<&str, &InterfaceDef> = interfaces
        .iter()
        .map(|iface| (iface.name.as_str(), iface))
        .collect();
    let mut seen_ifaces = BTreeSet::new();
    for def in chain {
        for iface_name in &def.implements {
            let Some(&iface) = iface_by_name.get(iface_name.as_str()) else {
                continue;
            };
            for owner in iface.contract_owners(|name| iface_by_name.get(name.as_str()).copied()) {
                if seen_ifaces.insert(owner.name.as_str()) {
                    for lifecycle in &owner.lifecycles {
                        collect(lifecycle);
                    }
                }
            }
        }
    }
    if !found_lifecycle {
        return None;
    }
    if let Some(prop) = effective_property(chain, property)
        && let Some(PropertyType::Enum { values }) =
            resolve_property_type(&prop.property_type, property_types, 0)
    {
        states.extend(values.iter().map(|entry| entry.value.clone()));
    }
    Some(states)
}

/// Check an automation payload against `ctx`.
///
/// Returns every admission finding alongside the resolved `objectType` and
/// `then.actionId` target when both named an admitted type and action.
#[must_use]
pub fn check_automation(
    payload: &Value,
    ctx: &AutomationCheckContext<'_>,
) -> (Vec<AutomationIssue>, Option<CheckedAutomationTarget>) {
    let mut issues = Vec::new();
    let Some(object) = payload.as_object() else {
        return (
            vec![issue(
                AutomationIssueKind::Invalid,
                "an automation payload must be a JSON object",
                "",
            )],
            None,
        );
    };
    check_top_metadata(object, &mut issues);
    let resolved_type = resolve_object_type(object, ctx, &mut issues);
    check_when(
        object.get("when"),
        resolved_type
            .as_ref()
            .map(|(name, chain)| (*name, chain.as_slice())),
        ctx,
        &mut issues,
    );
    check_guard(
        object.get("guard"),
        resolved_type.as_ref().map(|(name, _)| *name),
        ctx,
        &mut issues,
    );
    let action_id = check_then(
        object.get("then"),
        resolved_type.as_ref().map(|(name, _)| *name),
        ctx,
        &mut issues,
    );
    let target = resolved_type
        .zip(action_id)
        .map(|((object_type, _), action_id)| CheckedAutomationTarget {
            object_type: object_type.to_owned(),
            action_id,
        });
    (issues, target)
}

/// Return the trimmed title admitted by the automation metadata checker.
#[must_use]
pub fn validated_title(title: &str) -> Option<&str> {
    let title = title.trim();
    ((1..=MAX_TITLE_CHARS).contains(&title.chars().count()) && !title.chars().any(char::is_control))
        .then_some(title)
}

fn check_top_metadata(object: &serde_json::Map<String, Value>, issues: &mut Vec<AutomationIssue>) {
    for key in object
        .keys()
        .filter(|key| !TOP_MEMBERS.contains(&key.as_str()))
    {
        issues.push(issue(
            AutomationIssueKind::Invalid,
            format!("an automation has no member {key:?}"),
            &format!("/{}", pointer_token(key)),
        ));
    }
    let title_ok = object
        .get("title")
        .and_then(Value::as_str)
        .and_then(validated_title)
        .is_some();
    if !title_ok {
        issues.push(issue(
            AutomationIssueKind::Invalid,
            format!(
                "title must be 1 to {MAX_TITLE_CHARS} characters after trimming, without control characters"
            ),
            "/title",
        ));
    }
    if let Some(description) = object.get("description") {
        let description_ok = description.as_str().is_some_and(|text| {
            text.chars().count() <= MAX_DESCRIPTION_CHARS
                && !text.chars().any(|c| c.is_control() && c != '\n')
        });
        if !description_ok {
            issues.push(issue(
                AutomationIssueKind::Invalid,
                format!(
                    "description must be a string of at most {MAX_DESCRIPTION_CHARS} characters"
                ),
                "/description",
            ));
        }
    }
    if !object.get("enabled").is_some_and(Value::is_boolean) {
        issues.push(issue(
            AutomationIssueKind::Invalid,
            "enabled must be a boolean",
            "/enabled",
        ));
    }
}

fn resolve_object_type<'a>(
    object: &'a serde_json::Map<String, Value>,
    ctx: &AutomationCheckContext<'a>,
    issues: &mut Vec<AutomationIssue>,
) -> Option<(&'a str, Vec<&'a ObjectTypeDef>)> {
    match object.get("objectType").and_then(Value::as_str) {
        Some(object_type) if !object_type.starts_with("norn.") => {
            if let Some(chain) = lineage(ctx.object_types, object_type) {
                Some((object_type, chain))
            } else {
                issues.push(issue(
                    AutomationIssueKind::ObjectTypeUnknown,
                    format!("objectType {object_type:?} is not declared in this cell"),
                    "/objectType",
                ));
                None
            }
        }
        Some(object_type) => {
            issues.push(issue(
                AutomationIssueKind::ObjectTypeUnknown,
                format!("objectType {object_type:?} cannot target an internal system type"),
                "/objectType",
            ));
            None
        }
        None => {
            issues.push(issue(
                AutomationIssueKind::Invalid,
                "objectType must name a declared object type",
                "/objectType",
            ));
            None
        }
    }
}

fn check_guard(
    guard: Option<&Value>,
    object_type: Option<&str>,
    ctx: &AutomationCheckContext<'_>,
    issues: &mut Vec<AutomationIssue>,
) {
    let Some(guard_val) = guard else {
        return;
    };
    match guard_val.as_str() {
        Some(source) if !source.trim().is_empty() && source.chars().count() <= MAX_GUARD_CHARS => {
            if let Some(object_type) = object_type
                && let Err(messages) = crate::guard_expr::check_record_guard(
                    ctx.object_types,
                    ctx.interfaces,
                    ctx.property_types,
                    object_type,
                    source,
                )
            {
                for message in messages {
                    issues.push(issue(AutomationIssueKind::GuardInvalid, message, "/guard"));
                }
            }
        }
        Some(_) | None => {
            issues.push(issue(
                AutomationIssueKind::GuardInvalid,
                format!(
                    "guard must be a non-blank ZEN expression of at most {MAX_GUARD_CHARS} characters"
                ),
                "/guard",
            ));
        }
    }
}

fn check_when(
    when: Option<&Value>,
    resolved_type: Option<(&str, &[&ObjectTypeDef])>,
    ctx: &AutomationCheckContext<'_>,
    issues: &mut Vec<AutomationIssue>,
) {
    let Some(when) = when.and_then(Value::as_object) else {
        issues.push(issue(
            AutomationIssueKind::Invalid,
            "when must be a trigger object",
            "/when",
        ));
        return;
    };
    let Some(kind) = when.get("kind").and_then(Value::as_str) else {
        issues.push(issue(
            AutomationIssueKind::Invalid,
            "when.kind must be event, lifecycle_entered, or stale",
            "/when/kind",
        ));
        return;
    };
    match kind {
        "event" => check_when_event(when, resolved_type, ctx, issues),
        "lifecycle_entered" => check_when_lifecycle(when, resolved_type, ctx, issues),
        "stale" => check_when_stale(when, resolved_type, ctx, issues),
        other => issues.push(issue(
            AutomationIssueKind::Invalid,
            format!(
                "when.kind {other:?} is not supported; expected event, lifecycle_entered, or stale"
            ),
            "/when/kind",
        )),
    }
}

fn check_when_event(
    when: &serde_json::Map<String, Value>,
    resolved_type: Option<(&str, &[&ObjectTypeDef])>,
    ctx: &AutomationCheckContext<'_>,
    issues: &mut Vec<AutomationIssue>,
) {
    for key in when
        .keys()
        .filter(|key| !matches!(key.as_str(), "kind" | "event" | "actionId"))
    {
        issues.push(issue(
            AutomationIssueKind::Invalid,
            format!("event trigger has no member {key:?}"),
            &format!("/when/{}", pointer_token(key)),
        ));
    }
    match when.get("event").and_then(Value::as_str) {
        Some("created" | "updated") => {
            if when.contains_key("actionId") {
                issues.push(issue(
                    AutomationIssueKind::Invalid,
                    "actionId is valid only when when.event is \"action\"",
                    "/when/actionId",
                ));
            }
        }
        Some("action") => match when.get("actionId").and_then(Value::as_str) {
            Some(action_id) => {
                let found = ctx
                    .actions
                    .iter()
                    .find(|action| action.action_id == action_id);
                match found {
                    None => issues.push(issue(
                        AutomationIssueKind::ActionUnknown,
                        format!("trigger action {action_id:?} is not admitted in this cell"),
                        "/when/actionId",
                    )),
                    // #1704: a trigger action must be callable, as
                    // `then.actionId` must: an action with no effect program
                    // never commits, so the automation could never fire.
                    Some(action) if action.effect_program.is_none() => issues.push(issue(
                        AutomationIssueKind::ActionTargetMismatch,
                        format!("trigger action {action_id:?} has no executable effect program"),
                        "/when/actionId",
                    )),
                    Some(action) => {
                        if let Some((object_type, _)) = resolved_type
                            && let Some(program) = &action.effect_program
                        {
                            let target_kind = program.target().object_kind;
                            if !is_a(ctx.object_types, object_type, target_kind)
                                && !is_a(ctx.object_types, target_kind, object_type)
                            {
                                issues.push(issue(
                                    AutomationIssueKind::ActionTargetMismatch,
                                    format!(
                                        "trigger action {action_id:?} targets {target_kind:?}, which does not match {object_type:?}"
                                    ),
                                    "/when/actionId",
                                ));
                            }
                        }
                    }
                }
            }
            None => issues.push(issue(
                AutomationIssueKind::Invalid,
                "actionId is required when when.event is \"action\"",
                "/when/actionId",
            )),
        },
        Some(_) | None => issues.push(issue(
            AutomationIssueKind::Invalid,
            "when.event must be created, updated, or action",
            "/when/event",
        )),
    }
}

fn check_when_lifecycle(
    when: &serde_json::Map<String, Value>,
    resolved_type: Option<(&str, &[&ObjectTypeDef])>,
    ctx: &AutomationCheckContext<'_>,
    issues: &mut Vec<AutomationIssue>,
) {
    for key in when
        .keys()
        .filter(|key| !matches!(key.as_str(), "kind" | "property" | "state"))
    {
        issues.push(issue(
            AutomationIssueKind::Invalid,
            format!("lifecycle_entered trigger has no member {key:?}"),
            &format!("/when/{}", pointer_token(key)),
        ));
    }
    let property = when.get("property").and_then(Value::as_str);
    let state = when.get("state").and_then(Value::as_str);
    if property.is_none_or(|prop| prop.trim().is_empty()) {
        issues.push(issue(
            AutomationIssueKind::Invalid,
            "when.property must name a lifecycle property",
            "/when/property",
        ));
    }
    if state.is_none_or(|st| st.trim().is_empty()) {
        issues.push(issue(
            AutomationIssueKind::Invalid,
            "when.state must name a lifecycle state",
            "/when/state",
        ));
    }
    if let (Some(property), Some((object_type, chain))) = (property, resolved_type) {
        match lifecycle_states(chain, ctx.interfaces, ctx.property_types, property) {
            None => issues.push(issue(
                AutomationIssueKind::LifecycleUnknown,
                format!("property {property:?} has no declared lifecycle on {object_type:?}"),
                "/when/property",
            )),
            Some(states) => {
                if let Some(state) = state
                    && !states.contains(state)
                {
                    issues.push(issue(
                        AutomationIssueKind::LifecycleStateUnknown,
                        format!(
                            "state {state:?} is not declared by lifecycle {property:?} on {object_type:?}"
                        ),
                        "/when/state",
                    ));
                }
            }
        }
    }
}

fn check_when_stale(
    when: &serde_json::Map<String, Value>,
    resolved_type: Option<(&str, &[&ObjectTypeDef])>,
    ctx: &AutomationCheckContext<'_>,
    issues: &mut Vec<AutomationIssue>,
) {
    for key in when
        .keys()
        .filter(|key| !matches!(key.as_str(), "kind" | "property" | "durationMs"))
    {
        issues.push(issue(
            AutomationIssueKind::Invalid,
            format!("stale trigger has no member {key:?}"),
            &format!("/when/{}", pointer_token(key)),
        ));
    }
    let property = when.get("property").and_then(Value::as_str);
    if property.is_none_or(|prop| prop.trim().is_empty()) {
        issues.push(issue(
            AutomationIssueKind::Invalid,
            "when.property must name a timestamp or date property",
            "/when/property",
        ));
    } else if let (Some(property), Some((object_type, chain))) = (property, resolved_type) {
        let is_temporal = effective_property(chain, property)
            .and_then(|prop| resolve_property_type(&prop.property_type, ctx.property_types, 0))
            .is_some_and(|ty| matches!(ty, PropertyType::Timestamp { .. } | PropertyType::Date {}));
        if !is_temporal {
            issues.push(issue(
                AutomationIssueKind::StalePropertyInvalid,
                format!(
                    "property {property:?} on {object_type:?} must be a declared timestamp or date property"
                ),
                "/when/property",
            ));
        }
    }
    let duration_ok = when
        .get("durationMs")
        .and_then(Value::as_u64)
        .is_some_and(|ms| (MIN_STALE_DURATION_MS..=MAX_STALE_DURATION_MS).contains(&ms));
    if !duration_ok {
        issues.push(issue(
            AutomationIssueKind::Invalid,
            format!(
                "when.durationMs must be an integer from {MIN_STALE_DURATION_MS} ms (1 minute) to {MAX_STALE_DURATION_MS} ms (365 days)"
            ),
            "/when/durationMs",
        ));
    }
}

fn check_then(
    then: Option<&Value>,
    object_type: Option<&str>,
    ctx: &AutomationCheckContext<'_>,
    issues: &mut Vec<AutomationIssue>,
) -> Option<String> {
    let Some(then) = then.and_then(Value::as_object) else {
        issues.push(issue(
            AutomationIssueKind::Invalid,
            "then must be an action specification object",
            "/then",
        ));
        return None;
    };
    for key in then
        .keys()
        .filter(|key| !THEN_MEMBERS.contains(&key.as_str()))
    {
        issues.push(issue(
            AutomationIssueKind::Invalid,
            format!("then has no member {key:?}"),
            &format!("/then/{}", pointer_token(key)),
        ));
    }
    let resolved_action = check_then_action(then, object_type, ctx, issues);
    if let Some(input) = then.get("input")
        && !input.is_object()
    {
        issues.push(issue(
            AutomationIssueKind::Invalid,
            "then.input must be a JSON object",
            "/then/input",
        ));
    }
    match then.get("mode").and_then(Value::as_str) {
        Some("auto") => {
            for forbidden in ["decider", "question", "risk", "expiresInMs"] {
                if then.contains_key(forbidden) {
                    issues.push(issue(
                        AutomationIssueKind::Invalid,
                        format!("then.{forbidden} is only allowed when then.mode is \"propose\""),
                        &format!("/then/{forbidden}"),
                    ));
                }
            }
        }
        Some("propose") => check_propose_fields(then, ctx.roles, issues),
        Some(_) | None => issues.push(issue(
            AutomationIssueKind::Invalid,
            "then.mode must be \"auto\" or \"propose\"",
            "/then/mode",
        )),
    }
    resolved_action
}

fn check_then_action(
    then: &serde_json::Map<String, Value>,
    object_type: Option<&str>,
    ctx: &AutomationCheckContext<'_>,
    issues: &mut Vec<AutomationIssue>,
) -> Option<String> {
    let Some(action_id) = then.get("actionId").and_then(Value::as_str) else {
        issues.push(issue(
            AutomationIssueKind::Invalid,
            "then.actionId must name an admitted action",
            "/then/actionId",
        ));
        return None;
    };
    let Some(action) = ctx
        .actions
        .iter()
        .find(|action| action.action_id == action_id)
    else {
        issues.push(issue(
            AutomationIssueKind::ActionUnknown,
            format!("action {action_id:?} is not admitted in this cell"),
            "/then/actionId",
        ));
        return None;
    };
    match &action.effect_program {
        None => issues.push(issue(
            AutomationIssueKind::ActionTargetMismatch,
            format!("action {action_id:?} has no executable effect program"),
            "/then/actionId",
        )),
        Some(program) => {
            let target_kind = program.target().object_kind;
            if let Some(object_type) = object_type
                && !is_a(ctx.object_types, object_type, target_kind)
            {
                issues.push(issue(
                    AutomationIssueKind::ActionTargetMismatch,
                    format!(
                        "action {action_id:?} targets {target_kind:?}, which does not accept {object_type:?}"
                    ),
                    "/then/actionId",
                ));
            }
        }
    }
    Some(action_id.to_owned())
}

fn check_propose_decider(
    decider: Option<&Value>,
    roles: &[&str],
    issues: &mut Vec<AutomationIssue>,
) {
    let Some(decider) = decider.and_then(Value::as_object) else {
        issues.push(issue(
            AutomationIssueKind::Invalid,
            "then.decider is required when then.mode is \"propose\"",
            "/then/decider",
        ));
        return;
    };
    for key in decider.keys() {
        if key == "law" {
            issues.push(issue(
                AutomationIssueKind::Invalid,
                "decider.law is not supported for automations",
                "/then/decider/law",
            ));
        } else if key != "roles" {
            issues.push(issue(
                AutomationIssueKind::Invalid,
                format!("then.decider has no member {key:?}"),
                &format!("/then/decider/{}", pointer_token(key)),
            ));
        }
    }
    match decider.get("roles").and_then(Value::as_array) {
        Some(entries) if !entries.is_empty() => {
            for (index, entry) in entries.iter().enumerate() {
                match entry.as_str() {
                    Some(role) if roles.contains(&role) => {}
                    Some(role) => issues.push(issue(
                        AutomationIssueKind::RoleUnknown,
                        format!("{role:?} is not a role of this cell's role table"),
                        &format!("/then/decider/roles/{index}"),
                    )),
                    None => issues.push(issue(
                        AutomationIssueKind::Invalid,
                        "decider role must be a string",
                        &format!("/then/decider/roles/{index}"),
                    )),
                }
            }
        }
        Some(_) | None => issues.push(issue(
            AutomationIssueKind::Invalid,
            "decider roles must be a non-empty array of role names",
            "/then/decider/roles",
        )),
    }
}

fn check_propose_fields(
    then: &serde_json::Map<String, Value>,
    roles: &[&str],
    issues: &mut Vec<AutomationIssue>,
) {
    check_propose_decider(then.get("decider"), roles, issues);

    let question_ok = then
        .get("question")
        .and_then(Value::as_str)
        .is_some_and(|question| {
            !question.trim().is_empty()
                && question.chars().count() <= MAX_QUESTION_CHARS
                && !question.chars().any(|c| c.is_control() && c != '\n')
        });
    if !question_ok {
        issues.push(issue(
            AutomationIssueKind::Invalid,
            format!("then.question must be 1 to {MAX_QUESTION_CHARS} characters"),
            "/then/question",
        ));
    }

    if let Some(risk) = then.get("risk")
        && !matches!(risk.as_str(), Some("low" | "medium" | "high"))
    {
        issues.push(issue(
            AutomationIssueKind::Invalid,
            "then.risk must be low, medium, or high",
            "/then/risk",
        ));
    }

    if let Some(expires) = then.get("expiresInMs")
        && expires
            .as_u64()
            .is_none_or(|ms| ms == 0 || ms > MAX_EXPIRES_IN_MS)
    {
        issues.push(issue(
            AutomationIssueKind::Invalid,
            "then.expiresInMs must be 1 ms to seven days",
            "/then/expiresInMs",
        ));
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "unit tests"
)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_model() -> (Vec<ObjectTypeDef>, Vec<ActionRoot>) {
        let item: ObjectTypeDef = serde_json::from_value(json!({
            "name": "demo.Application",
            "properties": [
                {
                    "id": "status",
                    "type": {
                        "kind": "enum",
                        "values": [
                            { "value": "open" },
                            { "value": "stalled" },
                            { "value": "assisted" }
                        ]
                    }
                },
                { "id": "score", "type": { "kind": "integer" } },
                { "id": "updated_at", "type": { "kind": "timestamp", "timezone": "utc" } }
            ],
            "lifecycles": [
                {
                    "property": "status",
                    "initial": ["open"],
                    "terminal": ["assisted"],
                    "transitions": [
                        { "id": "stall", "from": ["open"], "to": "stalled" },
                        { "id": "assist", "from": ["stalled"], "to": "assisted" }
                    ]
                }
            ]
        }))
        .unwrap();
        let action: ActionRoot = serde_json::from_value(json!({
            "actionId": "demo.app.offerAssistance",
            "scopeRequired": [],
            "idempotent": true,
            "ledgered": false,
            "outputSchema": { "type": "object" },
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "required": ["id"],
                "properties": { "id": { "type": "string" } }
            },
            "effectProgram": {
                "kind": "fire_transition",
                "objectIdField": "id",
                "objectKind": "demo.Application",
                "entityArtifactId": "demo.Application.entity",
                "entityArtifactVersion": "0.1.0",
                "lifecycle": "status",
                "transition": "assist"
            }
        }))
        .unwrap();
        (vec![item], vec![action])
    }

    #[test]
    fn admits_valid_propose_automation_and_refuses_decider_law() {
        let (object_types, actions) = sample_model();
        let ctx = AutomationCheckContext {
            object_types: &object_types,
            interfaces: &[],
            property_types: &[],
            actions: &actions,
            roles: &["admin", "member"],
        };
        let valid = json!({
            "title": "Offer help to stalled applications",
            "enabled": true,
            "objectType": "demo.Application",
            "when": {
                "kind": "lifecycle_entered",
                "property": "status",
                "state": "stalled"
            },
            "guard": "self.score < 60",
            "then": {
                "actionId": "demo.app.offerAssistance",
                "mode": "propose",
                "decider": { "roles": ["admin"] },
                "question": "Offer assistance?",
                "risk": "low"
            }
        });
        let (issues, target) = check_automation(&valid, &ctx);
        assert_eq!(issues, vec![]);
        assert_eq!(
            target,
            Some(CheckedAutomationTarget {
                object_type: "demo.Application".to_owned(),
                action_id: "demo.app.offerAssistance".to_owned(),
            })
        );

        let mut with_law = valid;
        with_law["then"]["decider"]["law"] = json!("demo.law");
        let (issues, _) = check_automation(&with_law, &ctx);
        assert!(
            issues
                .iter()
                .any(|item| item.pointer == "/then/decider/law"),
            "{issues:?}"
        );
    }

    #[test]
    fn refuses_bad_guard_unknown_lifecycle_state_and_non_temporal_stale_property() {
        let (object_types, actions) = sample_model();
        let ctx = AutomationCheckContext {
            object_types: &object_types,
            interfaces: &[],
            property_types: &[],
            actions: &actions,
            roles: &["admin"],
        };
        let bad = json!({
            "title": "Bad rule",
            "enabled": true,
            "objectType": "demo.Application",
            "when": {
                "kind": "lifecycle_entered",
                "property": "status",
                "state": "nonexistent"
            },
            "guard": "self.score + 1",
            "then": {
                "actionId": "demo.app.offerAssistance",
                "mode": "auto",
                "question": "forbidden in auto mode"
            }
        });
        let (issues, _) = check_automation(&bad, &ctx);
        assert!(
            issues
                .iter()
                .any(|item| item.kind == AutomationIssueKind::LifecycleStateUnknown)
        );
        assert!(
            issues
                .iter()
                .any(|item| item.kind == AutomationIssueKind::GuardInvalid)
        );
        assert!(issues.iter().any(|item| item.pointer == "/then/question"));

        let bad_stale = json!({
            "title": "Bad stale",
            "enabled": true,
            "objectType": "demo.Application",
            "when": {
                "kind": "stale",
                "property": "score",
                "durationMs": 60_000
            },
            "then": {
                "actionId": "demo.app.offerAssistance",
                "mode": "auto"
            }
        });
        let (issues, _) = check_automation(&bad_stale, &ctx);
        assert!(
            issues
                .iter()
                .any(|item| item.kind == AutomationIssueKind::StalePropertyInvalid)
        );
    }

    #[test]
    fn refuses_guard_referencing_computed_field_or_link() {
        let owner: ObjectTypeDef = serde_json::from_value(json!({
            "name": "demo.Applicant",
            "properties": [
                { "id": "email", "type": { "kind": "text" } }
            ]
        }))
        .unwrap();
        let app: ObjectTypeDef = serde_json::from_value(json!({
            "name": "demo.Application",
            "properties": [
                { "id": "score", "type": { "kind": "integer" } },
                {
                    "id": "weightedScore",
                    "type": { "kind": "integer" },
                    "computed": "self.score * 2"
                }
            ],
            "links": [
                {
                    "id": "applicant",
                    "target": "demo.Applicant",
                    "cardinality": { "to": "one", "from": "many", "required": true }
                }
            ]
        }))
        .unwrap();
        let (_, actions) = sample_model();
        let object_types = [owner, app];
        let ctx = AutomationCheckContext {
            object_types: &object_types,
            interfaces: &[],
            property_types: &[],
            actions: &actions,
            roles: &["admin"],
        };

        for (guard, expected_word) in [
            ("self.weightedScore > 10", "computed property"),
            ("self.applicant.email == 'a@example.com'", "link"),
            ("actor.subjectId == 'admin'", "actor"),
        ] {
            let candidate = json!({
                "title": "Unsupported guard shape",
                "enabled": true,
                "objectType": "demo.Application",
                "when": { "kind": "event", "event": "updated" },
                "guard": guard,
                "then": { "actionId": "demo.app.offerAssistance", "mode": "auto" }
            });
            let (issues, _) = check_automation(&candidate, &ctx);
            assert!(
                issues
                    .iter()
                    .any(|item| item.kind == AutomationIssueKind::GuardInvalid
                        && item.pointer == "/guard"
                        && item.message.contains(expected_word)),
                "{guard}: {issues:?}"
            );
        }
    }

    /// #1704: an `event:action` trigger names an action that can commit, as
    /// `then.actionId` must; an action with no effect program never fires it.
    #[test]
    fn refuses_a_trigger_action_without_an_effect_program() {
        let (object_types, mut actions) = sample_model();
        let mut inert = actions[0].clone();
        inert.action_id = "demo.app.note".to_owned();
        inert.effect_program = None;
        actions.push(inert);
        let ctx = AutomationCheckContext {
            object_types: &object_types,
            interfaces: &[],
            property_types: &[],
            actions: &actions,
            roles: &["admin"],
        };
        let on = |trigger: &str| {
            json!({
                "title": "Follow an action",
                "enabled": true,
                "objectType": "demo.Application",
                "when": { "kind": "event", "event": "action", "actionId": trigger },
                "then": { "actionId": "demo.app.offerAssistance", "mode": "auto" }
            })
        };
        let (issues, _) = check_automation(&on("demo.app.note"), &ctx);
        assert_eq!(
            issues
                .iter()
                .filter(|item| item.pointer == "/when/actionId")
                .map(|item| (item.kind, item.message.as_str()))
                .collect::<Vec<_>>(),
            vec![(
                AutomationIssueKind::ActionTargetMismatch,
                "trigger action \"demo.app.note\" has no executable effect program"
            )],
            "{issues:?}"
        );
        let (issues, _) = check_automation(&on("demo.app.offerAssistance"), &ctx);
        assert_eq!(issues, vec![]);
    }
}
