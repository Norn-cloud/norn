//! ACTION-COMPENSATION-1 (D4): Ring-0 admission checks for
//! `ActionRoot.compensation` over resolved artifact sets and installed bases.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    missing_docs,
    reason = "integration-test crate"
)]

use std::collections::BTreeSet;

use norn_checker::action_set::codes;
use norn_checker::artifact_set::codes as set_codes;
use norn_checker::{EntityArtifactSetContext, check_artifact_set};
use norn_ir::{
    ArtifactEnvelope, ArtifactLifecycle, ArtifactPayload, ArtifactProvenance, ProvenanceSource,
    SUPPORTED_IR_VERSION, ScopeRef,
};
use serde_json::{Value, json};

fn scope(org: Option<&str>, env: Option<&str>) -> ScopeRef {
    ScopeRef {
        org_id: org.map(str::to_owned),
        tenant_id: None,
        workspace_id: None,
        environment_id: env.map(str::to_owned),
        business_unit_id: None,
    }
}

fn contact_entity() -> ArtifactEnvelope {
    ArtifactEnvelope {
        ir_version: SUPPORTED_IR_VERSION.to_owned(),
        artifact_id: "crm.Contact.entity".to_owned(),
        namespace: "crm".to_owned(),
        name: "Contact".to_owned(),
        version: "1.0.0".to_owned(),
        lifecycle: ArtifactLifecycle::Active,
        scope: scope(Some("org_demo"), None),
        dependencies: Vec::new(),
        provenance: ArtifactProvenance {
            source: ProvenanceSource::AuthoringSdk,
            source_path: None,
            actor_id: None,
            created_at: None,
        },
        payload: ArtifactPayload::Entity(
            serde_json::from_value(json!({
                "entityVersion": "2.0",
                "objectType": {
                    "name": "crm.Contact",
                    "identity": [{ "name": "primary", "properties": ["contact_id"] }],
                    "properties": [
                        { "id": "contact_id", "type": { "kind": "text" } },
                        {
                            "id": "status",
                            "default": "draft",
                            "type": {
                                "kind": "enum",
                                "values": [
                                    { "value": "draft" },
                                    { "value": "submitted" },
                                    { "value": "approved" },
                                    { "value": "posted" },
                                    { "value": "archived" },
                                    { "value": "cancelled" }
                                ]
                            }
                        }
                    ],
                    "lifecycles": [{
                        "property": "status",
                        "initial": ["draft"],
                        "terminal": ["cancelled"],
                        "transitions": [
                            { "id": "submit", "from": ["draft"], "to": "submitted" },
                            { "id": "approve", "from": ["submitted"], "to": "approved" },
                            { "id": "post", "from": ["approved"], "to": "posted" },
                            { "id": "archive", "from": ["posted"], "to": "archived" },
                            { "id": "restore", "from": ["archived"], "to": "posted" },
                            { "id": "cancel_early", "from": ["draft", "submitted"], "to": "cancelled" }
                        ]
                    }]
                }
            }))
            .expect("valid contact entity"),
        ),
    }
}

fn refund_entity() -> ArtifactEnvelope {
    ArtifactEnvelope {
        ir_version: SUPPORTED_IR_VERSION.to_owned(),
        artifact_id: "fin.Refund.entity".to_owned(),
        namespace: "fin".to_owned(),
        name: "Refund".to_owned(),
        version: "1.0.0".to_owned(),
        lifecycle: ArtifactLifecycle::Active,
        scope: scope(Some("org_demo"), None),
        dependencies: Vec::new(),
        provenance: ArtifactProvenance {
            source: ProvenanceSource::AuthoringSdk,
            source_path: None,
            actor_id: None,
            created_at: None,
        },
        payload: ArtifactPayload::Entity(
            serde_json::from_value(json!({
                "entityVersion": "2.0",
                "objectType": {
                    "name": "fin.Refund",
                    "identity": [{ "name": "primary", "properties": ["refund_id"] }],
                    "properties": [
                        { "id": "refund_id", "type": { "kind": "text" } }
                    ]
                }
            }))
            .expect("valid refund entity"),
        ),
    }
}

struct ActionSpec<'a> {
    artifact_id: &'a str,
    action_id: &'a str,
    scope: ScopeRef,
    scope_required: &'a [&'a str],
    ledgered: bool,
    lifecycle: ArtifactLifecycle,
    transition: Option<&'a str>,
    compensation: Option<&'a str>,
}

fn transition_action(spec: &ActionSpec<'_>) -> ArtifactEnvelope {
    let mut payload = json!({
        "actionId": spec.action_id,
        "scopeRequired": spec.scope_required,
        "idempotent": false,
        "ledgered": spec.ledgered,
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "properties": { "contact_id": { "type": "string" } },
            "required": ["contact_id"]
        },
        "outputSchema": {
            "type": "object",
            "additionalProperties": false,
            "properties": { "contact_id": { "type": "string" } },
            "required": ["contact_id"]
        },
        "writes": [{ "entityKind": "crm.Contact", "change": "modified" }]
    });
    if let Some(transition) = spec.transition {
        payload["effectProgram"] = json!({
            "kind": "fire_transition",
            "objectIdField": "contact_id",
            "objectKind": "crm.Contact",
            "entityArtifactId": "crm.Contact.entity",
            "entityArtifactVersion": "1.0.0",
            "lifecycle": "status",
            "transition": transition
        });
    }
    if let Some(comp) = spec.compensation {
        payload["compensation"] = json!({ "actionId": comp });
    }
    ArtifactEnvelope {
        ir_version: SUPPORTED_IR_VERSION.to_owned(),
        artifact_id: spec.artifact_id.to_owned(),
        namespace: "crm".to_owned(),
        name: spec.action_id.to_owned(),
        version: "1.0.0".to_owned(),
        lifecycle: spec.lifecycle.clone(),
        scope: spec.scope.clone(),
        dependencies: Vec::new(),
        provenance: ArtifactProvenance {
            source: ProvenanceSource::AuthoringSdk,
            source_path: None,
            actor_id: None,
            created_at: None,
        },
        payload: ArtifactPayload::Action(serde_json::from_value(payload).expect("action payload")),
    }
}

fn refund_create_action(artifact_id: &str, action_id: &str) -> ArtifactEnvelope {
    let payload: Value = json!({
        "actionId": action_id,
        "scopeRequired": ["orgId", "environmentId"],
        "idempotent": true,
        "ledgered": true,
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "properties": { "refund_id": { "type": "string" } },
            "required": ["refund_id"]
        },
        "outputSchema": {
            "type": "object",
            "additionalProperties": false,
            "properties": { "refund_id": { "type": "string" } },
            "required": ["refund_id"]
        },
        "writes": [
            { "entityKind": "fin.Refund", "change": "created" },
            { "entityKind": "fin.Refund", "change": "modified" }
        ],
        "effectProgram": {
            "kind": "upsert_input_object",
            "objectIdField": "refund_id",
            "objectKind": "fin.Refund",
            "entityArtifactId": "fin.Refund.entity",
            "entityArtifactVersion": "1.0.0"
        }
    });
    ArtifactEnvelope {
        ir_version: SUPPORTED_IR_VERSION.to_owned(),
        artifact_id: artifact_id.to_owned(),
        namespace: "fin".to_owned(),
        name: action_id.to_owned(),
        version: "1.0.0".to_owned(),
        lifecycle: ArtifactLifecycle::Active,
        scope: scope(Some("org_demo"), Some("dev")),
        dependencies: Vec::new(),
        provenance: ArtifactProvenance {
            source: ProvenanceSource::AuthoringSdk,
            source_path: None,
            actor_id: None,
            created_at: None,
        },
        payload: ArtifactPayload::Action(serde_json::from_value(payload).expect("refund payload")),
    }
}

fn check_with_base(
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
) -> Vec<(String, Option<String>)> {
    let registry = BTreeSet::new();
    let ctx = EntityArtifactSetContext {
        registry: &registry,
        base,
        tenant: None,
    };
    check_artifact_set(artifacts, &ctx)
        .into_iter()
        .filter(norn_checker::Diagnostic::is_error)
        .map(|d| (d.code.0, d.span.map(|s| s.pointer)))
        .collect()
}

#[test]
fn admits_direct_multi_hop_cross_entity_and_base_compensations() {
    let dev = scope(Some("org_demo"), Some("dev"));
    let org = scope(Some("org_demo"), None);
    let restore = transition_action(&ActionSpec {
        artifact_id: "crm.contact.restore.action",
        action_id: "crm.contact.restore",
        scope: org.clone(),
        scope_required: &["orgId"],
        ledgered: true,
        lifecycle: ArtifactLifecycle::Active,
        transition: Some("restore"),
        compensation: None,
    });
    let archive = transition_action(&ActionSpec {
        artifact_id: "crm.contact.archive.action",
        action_id: "crm.contact.archive",
        scope: dev.clone(),
        scope_required: &["orgId", "environmentId"],
        ledgered: true,
        lifecycle: ArtifactLifecycle::Draft,
        transition: Some("archive"),
        compensation: Some("crm.contact.restore"),
    });
    // Multi-hop reachability: approve -> approved -> posted -> archived, where restore fires.
    let approve = transition_action(&ActionSpec {
        artifact_id: "crm.contact.approve.action",
        action_id: "crm.contact.approve",
        scope: dev.clone(),
        scope_required: &["orgId", "environmentId"],
        ledgered: true,
        lifecycle: ArtifactLifecycle::Active,
        transition: Some("approve"),
        compensation: Some("crm.contact.restore"),
    });
    // Cross-entity compensation (e.g. posting compensated by creating a separate Refund).
    let refund = refund_create_action("fin.refund.create.action", "fin.refund.create");
    let post = transition_action(&ActionSpec {
        artifact_id: "crm.contact.post.action",
        action_id: "crm.contact.post",
        scope: dev,
        scope_required: &["orgId", "environmentId"],
        ledgered: true,
        lifecycle: ArtifactLifecycle::Active,
        transition: Some("post"),
        compensation: Some("fin.refund.create"),
    });

    let base = [contact_entity(), restore];
    let candidates = [refund_entity(), refund, archive, approve, post];
    let errors = check_with_base(&candidates, &base);
    assert!(errors.is_empty(), "expected admission, got {errors:?}");
}

#[test]
fn refuses_unresolved_compensation() {
    let dev = scope(Some("org_demo"), Some("dev"));
    let archive = transition_action(&ActionSpec {
        artifact_id: "crm.contact.archive.action",
        action_id: "crm.contact.archive",
        scope: dev,
        scope_required: &["orgId", "environmentId"],
        ledgered: true,
        lifecycle: ArtifactLifecycle::Active,
        transition: Some("archive"),
        compensation: Some("crm.contact.does_not_exist"),
    });
    let errors = check_with_base(&[contact_entity(), archive], &[]);
    assert!(
        errors.contains(&(
            codes::UNRESOLVED_COMPENSATION.to_owned(),
            Some("/payload/compensation/actionId".to_owned()),
        )),
        "{errors:?}"
    );
}

#[test]
fn refuses_self_and_mutual_compensation_cycles() {
    let dev = scope(Some("org_demo"), Some("dev"));
    let self_comp = transition_action(&ActionSpec {
        artifact_id: "crm.contact.archive.action",
        action_id: "crm.contact.archive",
        scope: dev.clone(),
        scope_required: &["orgId", "environmentId"],
        ledgered: true,
        lifecycle: ArtifactLifecycle::Active,
        transition: Some("archive"),
        compensation: Some("crm.contact.archive"),
    });
    let self_errors = check_with_base(&[contact_entity(), self_comp], &[]);
    assert!(
        self_errors.contains(&(
            codes::COMPENSATION_CYCLE.to_owned(),
            Some("/payload/compensation/actionId".to_owned()),
        )),
        "{self_errors:?}"
    );

    let archive = transition_action(&ActionSpec {
        artifact_id: "crm.contact.archive.action",
        action_id: "crm.contact.archive",
        scope: dev.clone(),
        scope_required: &["orgId", "environmentId"],
        ledgered: true,
        lifecycle: ArtifactLifecycle::Active,
        transition: Some("archive"),
        compensation: Some("crm.contact.restore"),
    });
    let restore = transition_action(&ActionSpec {
        artifact_id: "crm.contact.restore.action",
        action_id: "crm.contact.restore",
        scope: dev,
        scope_required: &["orgId", "environmentId"],
        ledgered: true,
        lifecycle: ArtifactLifecycle::Active,
        transition: Some("restore"),
        compensation: Some("crm.contact.archive"),
    });
    let cycle_errors = check_with_base(&[contact_entity(), archive, restore], &[]);
    let cycle_count = cycle_errors
        .iter()
        .filter(|(code, ptr)| {
            code == codes::COMPENSATION_CYCLE
                && ptr.as_deref() == Some("/payload/compensation/actionId")
        })
        .count();
    assert_eq!(cycle_count, 2, "{cycle_errors:?}");
}

#[test]
fn refuses_candidate_to_base_compensation_cycle() {
    let dev = scope(Some("org_demo"), Some("dev"));
    let base_approve = transition_action(&ActionSpec {
        artifact_id: "crm.contact.approve.action",
        action_id: "crm.contact.approve",
        scope: dev.clone(),
        scope_required: &["orgId", "environmentId"],
        ledgered: true,
        lifecycle: ArtifactLifecycle::Active,
        transition: Some("approve"),
        compensation: Some("crm.contact.restore"),
    });
    let base_restore = transition_action(&ActionSpec {
        artifact_id: "crm.contact.restore.action",
        action_id: "crm.contact.restore",
        scope: dev.clone(),
        scope_required: &["orgId", "environmentId"],
        ledgered: true,
        lifecycle: ArtifactLifecycle::Active,
        transition: Some("restore"),
        compensation: Some("crm.contact.approve"),
    });
    let cand_archive = transition_action(&ActionSpec {
        artifact_id: "crm.contact.archive.action",
        action_id: "crm.contact.archive",
        scope: dev,
        scope_required: &["orgId", "environmentId"],
        ledgered: true,
        lifecycle: ArtifactLifecycle::Active,
        transition: Some("archive"),
        compensation: Some("crm.contact.approve"),
    });
    let base_cycle_errors = check_with_base(
        &[cand_archive],
        &[contact_entity(), base_approve, base_restore],
    );
    assert!(
        base_cycle_errors.contains(&(
            codes::COMPENSATION_CYCLE.to_owned(),
            Some("/payload/compensation/actionId".to_owned()),
        )),
        "{base_cycle_errors:?}"
    );
}

#[test]
fn skips_spurious_cycle_on_ambiguous_return_edge() {
    let dev = scope(Some("org_demo"), Some("dev"));
    // Candidate `archive` duplicates base `archive` and points to `restore`,
    // which points back to `archive` (ambiguous across candidate + base).
    // Duplicate + ambiguity are reported without a misleading cycle finding.
    let base_archive = transition_action(&ActionSpec {
        artifact_id: "crm.contact.archive.base.action",
        action_id: "crm.contact.archive",
        scope: dev.clone(),
        scope_required: &["orgId", "environmentId"],
        ledgered: true,
        lifecycle: ArtifactLifecycle::Active,
        transition: Some("archive"),
        compensation: None,
    });
    let dup_archive = transition_action(&ActionSpec {
        artifact_id: "crm.contact.archive.cand.action",
        action_id: "crm.contact.archive",
        scope: dev.clone(),
        scope_required: &["orgId", "environmentId"],
        ledgered: true,
        lifecycle: ArtifactLifecycle::Active,
        transition: Some("archive"),
        compensation: Some("crm.contact.restore"),
    });
    let restore = transition_action(&ActionSpec {
        artifact_id: "crm.contact.restore.action",
        action_id: "crm.contact.restore",
        scope: dev,
        scope_required: &["orgId", "environmentId"],
        ledgered: true,
        lifecycle: ArtifactLifecycle::Active,
        transition: Some("restore"),
        compensation: Some("crm.contact.archive"),
    });
    let dup_errors = check_with_base(&[dup_archive, restore], &[contact_entity(), base_archive]);
    assert!(
        dup_errors
            .iter()
            .any(|(code, _)| code == codes::DUPLICATE_ACTION_ID),
        "{dup_errors:?}"
    );
    assert!(
        dup_errors.contains(&(
            codes::INVALID_COMPENSATION.to_owned(),
            Some("/payload/compensation/actionId".to_owned()),
        )),
        "{dup_errors:?}"
    );
    assert!(
        !dup_errors
            .iter()
            .any(|(code, _)| code == codes::COMPENSATION_CYCLE),
        "ambiguous return edge must not emit a misleading cycle: {dup_errors:?}"
    );
}

#[test]
fn refuses_incompatible_scope_visibility_on_compensation() {
    let org_only = scope(Some("org_demo"), None);
    let dev_only = scope(Some("org_demo"), Some("dev"));
    let restore = transition_action(&ActionSpec {
        artifact_id: "crm.contact.restore.action",
        action_id: "crm.contact.restore",
        scope: dev_only,
        scope_required: &["orgId"],
        ledgered: true,
        lifecycle: ArtifactLifecycle::Active,
        transition: Some("restore"),
        compensation: None,
    });
    let archive = transition_action(&ActionSpec {
        artifact_id: "crm.contact.archive.action",
        action_id: "crm.contact.archive",
        scope: org_only,
        scope_required: &["orgId"],
        ledgered: true,
        lifecycle: ArtifactLifecycle::Active,
        transition: Some("archive"),
        compensation: Some("crm.contact.restore"),
    });
    let errors = check_with_base(&[contact_entity(), restore, archive], &[]);
    assert!(
        errors.contains(&(
            set_codes::INCOMPATIBLE_SCOPE.to_owned(),
            Some("/payload/compensation/actionId".to_owned()),
        )),
        "{errors:?}"
    );
}

#[test]
fn refuses_unledgered_and_narrower_scope_compensations() {
    let dev = scope(Some("org_demo"), Some("dev"));

    let unledgered_restore = transition_action(&ActionSpec {
        artifact_id: "crm.contact.restore.action",
        action_id: "crm.contact.restore",
        scope: dev.clone(),
        scope_required: &["orgId", "environmentId"],
        ledgered: false,
        lifecycle: ArtifactLifecycle::Active,
        transition: Some("restore"),
        compensation: None,
    });
    let archive = transition_action(&ActionSpec {
        artifact_id: "crm.contact.archive.action",
        action_id: "crm.contact.archive",
        scope: dev.clone(),
        scope_required: &["orgId", "environmentId"],
        ledgered: true,
        lifecycle: ArtifactLifecycle::Active,
        transition: Some("archive"),
        compensation: Some("crm.contact.restore"),
    });
    let unledgered_errors = check_with_base(&[contact_entity(), unledgered_restore, archive], &[]);
    assert!(
        unledgered_errors.contains(&(
            codes::INVALID_COMPENSATION.to_owned(),
            Some("/payload/compensation/actionId".to_owned()),
        )),
        "{unledgered_errors:?}"
    );

    let extra_scope_restore = transition_action(&ActionSpec {
        artifact_id: "crm.contact.restore.action",
        action_id: "crm.contact.restore",
        scope: dev.clone(),
        scope_required: &["orgId", "environmentId", "tenantId"],
        ledgered: true,
        lifecycle: ArtifactLifecycle::Active,
        transition: Some("restore"),
        compensation: None,
    });
    let archive_missing_tenant = transition_action(&ActionSpec {
        artifact_id: "crm.contact.archive.action",
        action_id: "crm.contact.archive",
        scope: dev,
        scope_required: &["orgId", "environmentId"],
        ledgered: true,
        lifecycle: ArtifactLifecycle::Active,
        transition: Some("archive"),
        compensation: Some("crm.contact.restore"),
    });
    let scope_errors = check_with_base(
        &[
            contact_entity(),
            extra_scope_restore,
            archive_missing_tenant,
        ],
        &[],
    );
    assert!(
        scope_errors.contains(&(
            codes::INVALID_COMPENSATION.to_owned(),
            Some("/payload/compensation/actionId".to_owned()),
        )),
        "{scope_errors:?}"
    );
}

#[test]
fn refuses_unreachable_transition_and_rejected_compensations() {
    let dev = scope(Some("org_demo"), Some("dev"));

    let cancel_early = transition_action(&ActionSpec {
        artifact_id: "crm.contact.cancel.action",
        action_id: "crm.contact.cancel",
        scope: dev.clone(),
        scope_required: &["orgId", "environmentId"],
        ledgered: true,
        lifecycle: ArtifactLifecycle::Active,
        transition: Some("cancel_early"),
        compensation: None,
    });
    let approve = transition_action(&ActionSpec {
        artifact_id: "crm.contact.approve.action",
        action_id: "crm.contact.approve",
        scope: dev.clone(),
        scope_required: &["orgId", "environmentId"],
        ledgered: true,
        lifecycle: ArtifactLifecycle::Active,
        transition: Some("approve"),
        compensation: Some("crm.contact.cancel"),
    });
    let reach_errors = check_with_base(&[contact_entity(), cancel_early, approve], &[]);
    assert!(
        reach_errors.contains(&(
            codes::INVALID_COMPENSATION.to_owned(),
            Some("/payload/compensation/actionId".to_owned()),
        )),
        "{reach_errors:?}"
    );

    let rejected_restore = transition_action(&ActionSpec {
        artifact_id: "crm.contact.restore.action",
        action_id: "crm.contact.restore",
        scope: dev.clone(),
        scope_required: &["orgId", "environmentId"],
        ledgered: true,
        lifecycle: ArtifactLifecycle::Rejected,
        transition: Some("restore"),
        compensation: None,
    });
    let archive_rejected = transition_action(&ActionSpec {
        artifact_id: "crm.contact.archive.action",
        action_id: "crm.contact.archive",
        scope: dev,
        scope_required: &["orgId", "environmentId"],
        ledgered: true,
        lifecycle: ArtifactLifecycle::Active,
        transition: Some("archive"),
        compensation: Some("crm.contact.restore"),
    });
    let lifecycle_errors =
        check_with_base(&[contact_entity(), rejected_restore, archive_rejected], &[]);
    assert!(
        lifecycle_errors.contains(&(
            codes::INVALID_COMPENSATION.to_owned(),
            Some("/payload/compensation/actionId".to_owned()),
        )),
        "{lifecycle_errors:?}"
    );
}
