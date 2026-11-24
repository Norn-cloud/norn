//! The `automation` retained-definition kind (`AUTOMATION-1`).
//!
//! An automation is a declarative When / If / Then definition admitted by
//! `norn-checker` ([`norn_checker::automation::check_automation`]) and
//! validated against `norn-kernel`'s closed action input validator
//! ([`norn_kernel::validate_automation_static_input`]).
//!
//! At admission and install, the host verifies the installing author's grant
//! through the scoped Cedar PDP across the watched `objectType` and every
//! subtype extending it, and binds an attenuated, revocable delegation citing
//! the permitting parent Cedar policy ids (Core 05 `AUTH-ATTENUATE-1`).
//! Authority is never borrowed from a later triggering actor, and retiring an
//! automation voids its pending outbox candidates in the same transaction.
//!
//! Dispatch obligations (U2):
//! - Before invoking `then.actionId` for a candidate, U2 must require the
//!   candidate's `(automation_id, revision)` to match the current active
//!   revision and must reauthorize the bound author's live grant and the
//!   target row under the dispatch hold.
//! - Before draining pending candidates, U2 must expand any pending
//!   `_norn_automation_outbox_continuation` rows from committed history at
//!   `trigger_commit_seq` (before = state at `seq - 1`, after = state at `seq`)
//!   under the continuation's recorded storage, engine, and definitions
//!   versions, inserting expanded candidates idempotently into
//!   `_norn_automation_outbox`.
//!
//! The runtime reserves the `norn.automation.` / `norn.automation/` idempotency
//! key namespace (`S5a`-equivalent): the public `POST /v1/actions/{actionId}`
//! route unconditionally refuses any key in this namespace.

use http::StatusCode;
use norn_cell::automation_outbox::AutomationTriggerCandidate;
use norn_cell::engine::SqlEngine;
use norn_cell::retained::{RetainedAuthority, RetainedDefinition};
use norn_checker::automation::{AutomationCheckContext, AutomationIssueKind, check_automation};
use norn_ir::RuntimeValue;
use serde_json::Value;

use crate::authz::{AutomationGrantRequest, read_denied};
use crate::host::DevHost;
use crate::refusal::{Refusal, RefusalDiagnostic};

/// Dot-delimited prefix reserved for automation-owned idempotency keys.
pub const AUTOMATION_KEY_PREFIX: &str = "norn.automation.";

/// Slash-delimited prefix reserved for automation-owned idempotency keys.
pub const AUTOMATION_KEY_SLASH_PREFIX: &str = "norn.automation/";

/// Hex digest length kept in deterministic automation call keys.
const AUTOMATION_KEY_DIGITS: usize = 48;

/// Stable `definition.automation_*` refusal diagnostic codes.
pub mod codes {
    /// The payload does not match the automation shape or bounds.
    pub const INVALID: &str = "definition.automation_invalid";
    /// `objectType` is not a declared object type of the cell.
    pub const OBJECT_TYPE_UNKNOWN: &str = "definition.automation_object_type_unknown";
    /// `when.actionId` or `then.actionId` is not an admitted action.
    pub const ACTION_UNKNOWN: &str = "definition.automation_action_unknown";
    /// An action's target kind does not match `objectType`.
    pub const ACTION_TARGET_MISMATCH: &str = "definition.automation_action_target_mismatch";
    /// `when.property` is not a declared lifecycle of `objectType`.
    pub const LIFECYCLE_UNKNOWN: &str = "definition.automation_lifecycle_unknown";
    /// `when.state` is not a declared state of that lifecycle.
    pub const LIFECYCLE_STATE_UNKNOWN: &str = "definition.automation_lifecycle_state_unknown";
    /// `when.property` for `stale` is not a declared timestamp or date property.
    pub const STALE_PROPERTY_INVALID: &str = "definition.automation_stale_property_invalid";
    /// `guard` failed ZEN expression compilation, type, or profile checking.
    pub const GUARD_INVALID: &str = "definition.automation_guard_invalid";
    /// `then.input` failed closed static action input validation.
    pub const INPUT_INVALID: &str = "definition.automation_input_invalid";
    /// A `then.decider.roles` entry is not in the cell's declared role table.
    pub const ROLE_UNKNOWN: &str = "definition.automation_role_unknown";
}

/// Whether `key` belongs to the automation-owned idempotency key namespace.
///
/// The public `POST /v1/actions/{actionId}` route unconditionally refuses any
/// matching key so external callers can never collide with or spoof an
/// automation dispatch.
#[must_use]
pub fn is_automation_call_key(key: &str) -> bool {
    key.starts_with(AUTOMATION_KEY_PREFIX) || key.starts_with(AUTOMATION_KEY_SLASH_PREFIX)
}

/// Deterministic automation-owned idempotency key for one trigger candidate's
/// direct dispatch (U2).
///
/// # Errors
/// Canonical JSON encoding failure.
pub fn automation_run_key(
    automation_id: &str,
    revision: u64,
    trigger_commit_seq: u64,
    object_type: &str,
    object_id: &str,
    object_version: u64,
) -> Result<String, Refusal> {
    let digest = norn_ir::canonical::domain_hash_hex(
        b"norn.automation.run/1\0",
        &(
            automation_id,
            revision,
            trigger_commit_seq,
            object_type,
            object_id,
            object_version,
        ),
    )
    .map_err(Refusal::host)?;
    Ok(format!(
        "{AUTOMATION_KEY_PREFIX}run.{}",
        digest.get(..AUTOMATION_KEY_DIGITS).unwrap_or(&digest)
    ))
}

/// Deterministic automation-owned idempotency key for one approved automation
/// proposal's execution (U3).
///
/// # Errors
/// Canonical JSON encoding failure.
pub fn automation_approved_key(decision_id: &str) -> Result<String, Refusal> {
    let digest = norn_ir::canonical::domain_hash_hex(b"norn.automation.approved/1\0", &decision_id)
        .map_err(Refusal::host)?;
    Ok(format!(
        "{AUTOMATION_KEY_PREFIX}approved.{}",
        digest.get(..AUTOMATION_KEY_DIGITS).unwrap_or(&digest)
    ))
}

/// Deterministic revocable delegation id binding one automation revision to its
/// installing author's attenuated grant and permitting parent Cedar policy ids
/// (Core 05 `AUTH-ATTENUATE-1`).
///
/// # Errors
/// Canonical JSON encoding failure.
pub fn delegation_id(
    definition_id: &str,
    revision: u64,
    author_subject: &str,
    scope: &str,
    action_id: &str,
    action_digest: &str,
    parent_grant_ids: &[String],
) -> Result<String, Refusal> {
    let digest = norn_ir::canonical::domain_hash_hex(
        b"norn.automation.grant/1\0",
        &(
            definition_id,
            revision,
            author_subject,
            scope,
            action_id,
            action_digest,
            parent_grant_ids,
        ),
    )
    .map_err(Refusal::host)?;
    Ok(format!(
        "norn.automation.grant/{}",
        digest.get(..AUTOMATION_KEY_DIGITS).unwrap_or(&digest)
    ))
}

/// Every concrete object type in `object_types` that matches `watched`
/// (`watched` itself plus every subtype extending it), in sorted order.
fn matched_subtypes(object_types: &[norn_ir::ObjectTypeDef], watched: &str) -> Vec<String> {
    let parents: std::collections::BTreeMap<&str, Option<&str>> = object_types
        .iter()
        .map(|def| {
            (
                def.name.as_str(),
                def.extends.as_ref().map(norn_ir::ObjectTypeName::as_str),
            )
        })
        .collect();
    let mut matched = std::collections::BTreeSet::new();
    for def in object_types {
        let mut cursor = Some(def.name.as_str());
        let mut seen = std::collections::BTreeSet::new();
        while let Some(name) = cursor {
            if !seen.insert(name) {
                break;
            }
            if name == watched {
                matched.insert(def.name.as_str().to_owned());
                break;
            }
            cursor = parents.get(name).copied().flatten();
        }
    }
    matched.into_iter().collect()
}

fn canonical_parent_grants(grant_ids: Vec<String>) -> Result<Vec<String>, Refusal> {
    let canonical: std::collections::BTreeSet<String> = grant_ids
        .into_iter()
        .filter(|id| !id.trim().is_empty())
        .collect();
    if canonical.is_empty() {
        return Err(read_denied());
    }
    Ok(canonical.into_iter().collect())
}

/// Resolved `(objectType, then.actionId)` target of a checked automation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedAutomation {
    /// Declared object type watched by the automation.
    pub object_type: String,
    /// Admitted action invoked by the automation.
    pub action_id: String,
}

const fn map_issue_code(kind: AutomationIssueKind) -> &'static str {
    match kind {
        AutomationIssueKind::Invalid => codes::INVALID,
        AutomationIssueKind::ObjectTypeUnknown => codes::OBJECT_TYPE_UNKNOWN,
        AutomationIssueKind::ActionUnknown => codes::ACTION_UNKNOWN,
        AutomationIssueKind::ActionTargetMismatch => codes::ACTION_TARGET_MISMATCH,
        AutomationIssueKind::LifecycleUnknown => codes::LIFECYCLE_UNKNOWN,
        AutomationIssueKind::LifecycleStateUnknown => codes::LIFECYCLE_STATE_UNKNOWN,
        AutomationIssueKind::StalePropertyInvalid => codes::STALE_PROPERTY_INVALID,
        AutomationIssueKind::GuardInvalid => codes::GUARD_INVALID,
        AutomationIssueKind::RoleUnknown => codes::ROLE_UNKNOWN,
    }
}

/// Admit an automation payload through `norn-checker` and `norn-kernel`'s
/// closed static action input validator.
///
/// `canonical` is the Norn1-canonicalized JSON text of `payload`, used to
/// decode `then.input` without losing exact decimal precision.
#[must_use]
pub fn check(
    canonical: &str,
    payload: &Value,
    context: &AutomationCheckContext<'_>,
) -> (Vec<RefusalDiagnostic>, Option<CheckedAutomation>) {
    let (checker_issues, checked_target) = check_automation(payload, context);
    let mut findings: Vec<RefusalDiagnostic> = checker_issues
        .into_iter()
        .map(|issue| {
            RefusalDiagnostic::error(map_issue_code(issue.kind), issue.message, issue.pointer)
        })
        .collect();

    // Complete static input admission via `norn-kernel` whenever `then.actionId`
    // resolves to an admitted action with an executable effect program.
    if let Some(action_id) = payload.pointer("/then/actionId").and_then(Value::as_str)
        && let Some(action) = context
            .actions
            .iter()
            .find(|action| action.action_id == action_id)
        && action.effect_program.is_some()
        && payload.pointer("/then/input").is_none_or(Value::is_object)
    {
        match RuntimeValue::from_json_text(canonical) {
            Ok(exact) => {
                let static_input = exact
                    .get("then")
                    .and_then(|then| then.get("input"))
                    .and_then(RuntimeValue::as_object);
                if let Err(violations) =
                    norn_kernel::validate_automation_static_input(action, static_input)
                {
                    for violation in violations {
                        findings.push(RefusalDiagnostic::error(
                            codes::INPUT_INVALID,
                            violation.message,
                            violation.pointer,
                        ));
                    }
                }
            }
            Err(error) => {
                findings.push(RefusalDiagnostic::error(
                    codes::INPUT_INVALID,
                    format!("then.input is not exact JSON: {error}"),
                    "/then/input",
                ));
            }
        }
    }

    let checked = checked_target.map(|target| CheckedAutomation {
        object_type: target.object_type,
        action_id: target.action_id,
    });
    (findings, checked)
}

/// Deterministic plain-words restatement signed by the installing author (D59).
///
/// # Errors
/// Returns a reason string if `canonical` is not a well-formed automation payload.
pub fn restate(canonical: &str) -> Result<String, String> {
    let value: Value = serde_json::from_str(canonical).map_err(|error| error.to_string())?;
    let text = |pointer: &str| value.pointer(pointer).and_then(Value::as_str);
    let title = text("/title").ok_or("the automation has no title")?;
    let object_type = text("/objectType").ok_or("the automation names no objectType")?;
    let enabled = value
        .get("enabled")
        .and_then(Value::as_bool)
        .ok_or("the automation has no enabled flag")?;
    let action_id = text("/then/actionId").ok_or("the automation names no target action")?;
    let mode = text("/then/mode").ok_or("the automation names no mode")?;

    let when_phrase = match text("/when/kind") {
        Some("event") => match text("/when/event") {
            Some("created") => format!("a {object_type} record is created"),
            Some("updated") => format!("a {object_type} record is updated"),
            Some("action") => {
                let trigger_action =
                    text("/when/actionId").ok_or("event:action trigger names no actionId")?;
                format!("action {trigger_action} commits on a {object_type} record")
            }
            Some(_) | None => return Err("unknown when.event".to_owned()),
        },
        Some("lifecycle_entered") => {
            let property = text("/when/property").ok_or("lifecycle_entered names no property")?;
            let state = text("/when/state").ok_or("lifecycle_entered names no state")?;
            format!("a {object_type} record enters {property} = {state:?}")
        }
        Some("stale") => {
            let property = text("/when/property").ok_or("stale names no property")?;
            let duration_ms = value
                .pointer("/when/durationMs")
                .and_then(Value::as_u64)
                .ok_or("stale names no durationMs")?;
            format!("a {object_type} record's {property} is older than {duration_ms} ms")
        }
        Some(_) | None => return Err("unknown when.kind".to_owned()),
    };

    let status_label = if enabled { "Enabled" } else { "Disabled" };
    let mut lines = vec![format!(
        "{status_label} automation \"{title}\": when {when_phrase}."
    )];
    if let Some(guard) = text("/guard") {
        lines.push(format!("Only when guard holds: {guard}."));
    }
    match mode {
        "auto" => {
            lines.push(format!("Automatically invokes action {action_id}."));
        }
        "propose" => {
            let question = text("/then/question").ok_or("propose automation asks no question")?;
            let roles: Vec<&str> = value
                .pointer("/then/decider/roles")
                .and_then(Value::as_array)
                .map_or_default(|entries| entries.iter().filter_map(Value::as_str).collect());
            let risk = text("/then/risk").unwrap_or("medium");
            lines.push(format!(
                "Proposes action {action_id} for approval by a holder of {} (risk: {risk}).",
                roles.join(" or ")
            ));
            lines.push(format!("It asks: {question}"));
            if let Some(expires_in_ms) = value.pointer("/then/expiresInMs").and_then(Value::as_u64)
            {
                lines.push(format!(
                    "Pending proposals expire after {expires_in_ms} ms."
                ));
            }
        }
        other => return Err(format!("unknown then.mode {other:?}")),
    }
    if let Some(input) = value.pointer("/then/input") {
        lines.push(format!("Static action input: {input}."));
    }
    lines.push(format!(
        "Authority: bound at install to the author's attenuated, revocable grant for {action_id}, never borrowed from a triggering actor."
    ));
    Ok(lines.join("\n"))
}

/// Refusal when an automation install omits its required restatement sign-off.
pub(crate) fn signoff_required() -> Refusal {
    Refusal::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        crate::definition_routes::codes::SIGNOFF_REQUIRED,
        "an automation is installed by signing the server's words for it",
        "admit the automation, read its restatement, and install it with signoff.restatementSha256",
    )
}

/// Admit an automation payload against `host`'s active model, actions, and
/// author grant (Core 05 `AUTH-ATTENUATE-1`).
pub(crate) fn validate_for_host<E: SqlEngine>(
    host: &DevHost<E>,
    actor: &str,
    roles: &[&str],
    canonical: &str,
    value: &Value,
) -> Result<(Vec<RefusalDiagnostic>, Option<CheckedAutomation>), Refusal> {
    let (object_types, interfaces, property_types, actions) = {
        let cell = host.cell()?;
        let reader = cell.store().read();
        let model = reader.model();
        let object_types: Vec<norn_ir::ObjectTypeDef> = model.object_types().cloned().collect();
        let interfaces: Vec<norn_ir::InterfaceDef> = model.interfaces().cloned().collect();
        let property_types: Vec<norn_ir::PropertyTypeDef> = model.property_types().to_vec();
        let actions: Vec<norn_ir::ActionRoot> = host
            .retained(&reader)?
            .map(|(_, retained)| {
                retained
                    .actions
                    .iter()
                    .map(|(_, action)| action.clone())
                    .collect()
            })
            .unwrap_or_default();
        (object_types, interfaces, property_types, actions)
    };
    let context = AutomationCheckContext {
        object_types: &object_types,
        interfaces: &interfaces,
        property_types: &property_types,
        actions: &actions,
        roles,
    };
    let (findings, checked) = check(canonical, value, &context);
    if let Some(target) = &checked
        && let Some(action) = actions
            .iter()
            .find(|action| action.action_id == target.action_id)
    {
        let matched_types = matched_subtypes(&object_types, &target.object_type);
        if matched_types.is_empty() {
            return Err(read_denied());
        }
        let verified = host
            .authorizer
            .authorize_automation_grant(&AutomationGrantRequest {
                scope: host.scope(),
                actor,
                action,
                object_type: &target.object_type,
                matched_types: &matched_types,
            })?;
        let _parent_grants = canonical_parent_grants(verified.parent_grant_ids)?;
        if host.authorizer.verified_principal(actor).is_none() {
            return Err(read_denied());
        }
    }
    Ok((findings, checked))
}

/// Re-verify the author's grant under the definitions write lock across the
/// watched `objectType` and every subtype extending it, and build the
/// server-owned, attenuated, revocable [`RetainedAuthority`] record citing the
/// permitting parent Cedar grant identities.
///
/// U2 dispatch obligation: before dispatching any outbox candidate under this
/// authority, U2 must require the candidate's `(automation_id, revision)` to be
/// the current active revision and must reauthorize `author`'s live grant
/// against the live target row under the dispatch hold.
pub(crate) fn bind_authority<E: SqlEngine>(
    host: &DevHost<E>,
    cell: &crate::host::ActionCell<E>,
    actor: &str,
    definition: &RetainedDefinition,
    checked: &CheckedAutomation,
    installed_at: u64,
    definitions_version: u64,
) -> Result<RetainedAuthority, Refusal> {
    let (_, admitted) = host
        .retained(&cell.store().read())?
        .ok_or_else(read_denied)?;
    let (_, action) = admitted
        .actions
        .iter()
        .find(|(_, action)| action.action_id == checked.action_id)
        .ok_or_else(|| {
            Refusal::new(
                StatusCode::CONFLICT,
                crate::definition_routes::codes::VERSION_CONFLICT,
                "the definition changed since it was last read",
                "read the current definition and retry with its version",
            )
        })?;
    let model_ref = cell.store().model();
    let matched_types: Vec<String> = model_ref
        .object_types()
        .filter(|def| model_ref.is_a(def.name.as_str(), &checked.object_type))
        .map(|def| def.name.as_str().to_owned())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    if matched_types.is_empty() {
        return Err(read_denied());
    }
    let verified = host
        .authorizer
        .authorize_automation_grant(&AutomationGrantRequest {
            scope: host.scope(),
            actor,
            action,
            object_type: &checked.object_type,
            matched_types: &matched_types,
        })?;
    let parent_grant_ids = canonical_parent_grants(verified.parent_grant_ids)?;
    let author = host
        .authorizer
        .verified_principal(actor)
        .ok_or_else(read_denied)?;
    let catalog = model_ref
        .type_catalog()
        .with_governing_laws(model_ref.law_artifacts())
        .with_action_rules(&admitted.rules);
    let action_digest =
        crate::action_admission::effective_digest(&admitted, action, model_ref, &catalog)?;
    let scope_key = host.scope().key();
    let delegation_id = delegation_id(
        &definition.id,
        definition.revision,
        &author.subject,
        &scope_key,
        &checked.action_id,
        &action_digest,
        &parent_grant_ids,
    )?;
    Ok(RetainedAuthority {
        delegation_id,
        parent_grant_ids,
        author,
        scope: scope_key,
        action_id: checked.action_id.clone(),
        object_type: checked.object_type.clone(),
        action_digest,
        bound_at: installed_at,
        definitions_version,
        revocable: true,
    })
}

/// Summary of a bound [`RetainedAuthority`] recorded in the install diff.
pub(crate) fn authority_diff(authority: Option<&RetainedAuthority>) -> Option<Value> {
    authority.map(|authority| {
        serde_json::json!({
            "delegationId": authority.delegation_id,
            "parentGrantIds": authority.parent_grant_ids,
            "author": authority.author.subject,
            "actionId": authority.action_id,
            "objectType": authority.object_type,
            "actionDigest": authority.action_digest,
            "revocable": authority.revocable,
        })
    })
}

/// Pending automation trigger candidates in the main world, for the host
/// runtime's outbox drain (U2) and integration tests; no network route exposes
/// this directly.
///
/// # Errors
/// Poisoned synchronization or a store failure.
pub fn pending_automation_triggers<E: SqlEngine>(
    host: &DevHost<E>,
    limit: usize,
) -> Result<Vec<AutomationTriggerCandidate>, Refusal> {
    let _epoch = host.definition_read()?;
    let cell = host.cell()?;
    cell.store()
        .read()
        .pending_automation_triggers(norn_cell::MAIN_WORLD, limit)
        .map_err(Refusal::host)
}

/// Automation trigger candidates recorded inside commit `commit_seq` in the
/// main world, for the host runtime and integration tests.
///
/// # Errors
/// Poisoned synchronization or a store failure.
pub fn automation_triggers_for_commit<E: SqlEngine>(
    host: &DevHost<E>,
    commit_seq: u64,
) -> Result<Vec<AutomationTriggerCandidate>, Refusal> {
    let _epoch = host.definition_read()?;
    let cell = host.cell()?;
    cell.store()
        .read()
        .automation_triggers_for_commit(norn_cell::MAIN_WORLD, commit_seq)
        .map_err(Refusal::host)
}

/// Count of pending automation trigger candidates in the main world.
///
/// # Errors
/// Poisoned synchronization or a store failure.
pub fn pending_automation_trigger_count<E: SqlEngine>(host: &DevHost<E>) -> Result<u64, Refusal> {
    let _epoch = host.definition_read()?;
    let cell = host.cell()?;
    cell.store()
        .read()
        .pending_automation_trigger_count(norn_cell::MAIN_WORLD)
        .map_err(Refusal::host)
}

/// Count of pending automation outbox continuation rows in the main world.
///
/// # Errors
/// Poisoned synchronization or a store failure.
pub fn pending_automation_continuation_count<E: SqlEngine>(
    host: &DevHost<E>,
) -> Result<u64, Refusal> {
    let _epoch = host.definition_read()?;
    let cell = host.cell()?;
    cell.store()
        .read()
        .pending_automation_continuation_count(norn_cell::MAIN_WORLD)
        .map_err(Refusal::host)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "automation definition unit tests"
)]
mod tests {
    use super::*;
    use norn_ir::{ActionRoot, ObjectTypeDef};
    use serde_json::json;

    fn sample_context() -> (Vec<ObjectTypeDef>, Vec<ActionRoot>) {
        let item: ObjectTypeDef = serde_json::from_value(json!({
            "name": "demo.Order",
            "properties": [
                {
                    "id": "status",
                    "type": {
                        "kind": "enum",
                        "values": [{ "value": "open" }, { "value": "stalled" }]
                    }
                },
                { "id": "amount", "type": { "kind": "decimal", "scale": 2 } }
            ],
            "lifecycles": [
                {
                    "property": "status",
                    "initial": ["open"],
                    "transitions": [
                        { "id": "stall", "from": ["open"], "to": "stalled" }
                    ]
                }
            ]
        }))
        .unwrap();
        let action: ActionRoot = serde_json::from_value(json!({
            "actionVersion": "0.1",
            "actionId": "demo.order.adjust",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "required": ["orderId", "amount", "updatedBy"],
                "properties": {
                    "orderId": { "type": "string" },
                    "amount": { "type": "number", "nornType": { "kind": "decimal", "scale": 2 } },
                    "updatedBy": { "type": "string" }
                }
            },
            "derive": [
                {
                    "field": "updatedBy",
                    "expression": "actor.subjectId",
                    "inputRequired": false
                }
            ],
            "effectProgram": {
                "kind": "upsert_input_object",
                "objectIdField": "orderId",
                "objectKind": "demo.Order",
                "entityArtifactId": "demo.Order.entity",
                "entityArtifactVersion": "0.1.0"
            }
        }))
        .unwrap();
        (vec![item], vec![action])
    }

    #[test]
    fn static_input_admission_catches_omitted_required_and_bad_decimal_scale() {
        let (object_types, actions) = sample_context();
        let ctx = AutomationCheckContext {
            object_types: &object_types,
            interfaces: &[],
            property_types: &[],
            actions: &actions,
            roles: &["admin"],
        };

        // Omitting `then.input` must fail because `amount` is required.
        let omitted = json!({
            "title": "Adjust stalled order",
            "enabled": true,
            "objectType": "demo.Order",
            "when": { "kind": "lifecycle_entered", "property": "status", "state": "stalled" },
            "then": { "actionId": "demo.order.adjust", "mode": "auto" }
        });
        let (findings, _) = check(&omitted.to_string(), &omitted, &ctx);
        assert!(
            findings.iter().any(|f| f.code == codes::INPUT_INVALID
                && f.span.as_ref().map(|s| s.pointer.as_str()) == Some("/then/input/amount")),
            "{findings:?}"
        );

        // Exact wire decimal passes; excess scale fails.
        let valid = json!({
            "title": "Adjust stalled order",
            "enabled": true,
            "objectType": "demo.Order",
            "when": { "kind": "lifecycle_entered", "property": "status", "state": "stalled" },
            "then": {
                "actionId": "demo.order.adjust",
                "mode": "auto",
                "input": { "amount": "15.25" }
            }
        });
        let (findings, checked) = check(&valid.to_string(), &valid, &ctx);
        assert_eq!(findings, vec![]);
        assert_eq!(
            checked,
            Some(CheckedAutomation {
                object_type: "demo.Order".to_owned(),
                action_id: "demo.order.adjust".to_owned(),
            })
        );

        let mut bad_scale = valid.clone();
        bad_scale["then"]["input"]["amount"] = json!("15.255");
        let (findings, _) = check(&bad_scale.to_string(), &bad_scale, &ctx);
        assert!(
            findings.iter().any(|f| f.code == codes::INPUT_INVALID),
            "{findings:?}"
        );

        let words = restate(&valid.to_string()).expect("restate");
        assert!(words.contains("attenuated, revocable grant for demo.order.adjust"));
        assert!(words.contains("never borrowed from a triggering actor"));
    }

    #[test]
    fn reserved_automation_keys_are_recognized() {
        let run_key = automation_run_key("ops.a", 1, 7, "demo.Order", "o-1", 2).unwrap();
        let approved_key = automation_approved_key("dec_1").unwrap();
        assert!(is_automation_call_key(&run_key));
        assert!(is_automation_call_key(&approved_key));
        assert!(is_automation_call_key("norn.automation.custom"));
        assert!(is_automation_call_key("norn.automation/custom"));
        assert!(!is_automation_call_key("caller-key-1"));
        assert!(run_key.chars().count() <= 128);
        assert!(approved_key.chars().count() <= 128);
    }
}
