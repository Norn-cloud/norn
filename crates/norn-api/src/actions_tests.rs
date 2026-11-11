//! HOST-ACTION-1: the shared service over a real admitted cell, its commit,
//! replay and every refusal, each refusal leaving the complete store unchanged.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "service correspondence setup and assertions"
)]
use super::*;
use crate::{
    authz::{Authorizer, DenyAll, ReadCapability, ReadRequest, read_denied},
    question_routes::QuestionSnapshot,
    scope::CellScope,
};
use norn_cell::{CellStore, ObjectRecord, engine::NativeEngine};
use norn_checker::metric::compute::QuestionPlan;
use norn_kernel::{
    ActionAuthzRequest, ActionDenial, ActionReadAuthzRequest, ActionReadTypeAuthzRequest,
    authz::{ActorKind, Delegation, PrincipalInput, ResourceInput, ScopeKey},
};

#[path = "../tests/support/action_cell.rs"]
mod action_cell;
use action_cell::{DERIVES_LOSS, MELT, MELT_ACTION, OMITS_LOSS, melt_body, melt_store};

const OPERATOR: &str = "user:operator";
const AUDITOR: &str = "user:auditor";
/// Admitted by the host operation policy but by no kernel action decision.
const HOST_ONLY: &str = "user:host-only";
/// Admitted by the host operation policy; the kernel's decision hides every
/// target from it (`target_not_found`).
const HIDDEN: &str = "user:hidden";

/// Server policy keyed on the verified subject only.
struct Policy;
impl Policy {
    fn acts(actor: &str) -> bool {
        actor == OPERATOR || actor == AUDITOR
    }
}
impl Authorizer for Policy {
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Refusal> {
        match (&request.operation, request.actor) {
            (Operation::CallAction(_), Some(actor))
                if Self::acts(actor) || actor == HOST_ONLY || actor == HIDDEN =>
            {
                Ok(())
            }
            _ => DenyAll.authorize(request),
        }
    }
    fn read(&self, request: &ReadRequest<'_>) -> Result<(), Refusal> {
        if request.access.principal.subject_id == AUDITOR {
            Ok(())
        } else {
            Err(read_denied())
        }
    }
    fn action(&self, request: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        if Self::acts(request.actor) {
            Ok(())
        } else if request.actor == HIDDEN {
            Err(ActionDenial::Invisible)
        } else {
            Err(ActionDenial::Forbidden {
                reason: codes::AUTHZ_DENIED.into(),
            })
        }
    }
    fn action_read(&self, request: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        if Self::acts(request.actor) {
            Ok(())
        } else {
            Err(ActionDenial::Invisible)
        }
    }
    fn action_read_type(
        &self,
        request: &ActionReadTypeAuthzRequest<'_>,
    ) -> Result<(), ActionDenial> {
        if Self::acts(request.actor) {
            Ok(())
        } else {
            Err(ActionDenial::Invisible)
        }
    }
    fn disclose_refusal_detail(&self, request: &RefusalDetailRequest<'_>) -> bool {
        request.actor == OPERATOR
    }
}

/// The cell's disclosure runtime: a handle key and collection-grant mapping only.
struct Disclosure;
impl QuestionRuntime<NativeEngine> for Disclosure {
    fn plan(&self, _question: &str) -> Result<QuestionPlan, Refusal> {
        Err(read_denied())
    }
    fn snapshot(
        &self,
        _store: &CellStore<NativeEngine>,
        _plan: &QuestionPlan,
        _actor: &str,
    ) -> Result<QuestionSnapshot, Refusal> {
        Err(read_denied())
    }
    fn access(
        &self,
        _scope: &CellScope,
        _actor: &str,
        _question: &str,
        _capability: ReadCapability,
        _row: Option<&ObjectRecord>,
    ) -> Result<norn_kernel::authz::AccessRequest, Refusal> {
        Err(read_denied())
    }
    fn min_disclosed_bucket(&self) -> usize {
        3
    }
    fn handle_key(&self) -> &[u8] {
        &[7; 32]
    }
    fn collection_access(
        &self,
        scope: &CellScope,
        actor: &str,
        _capability: ReadCapability,
        object_type: &str,
    ) -> Result<norn_kernel::authz::AccessRequest, Refusal> {
        let key = ScopeKey {
            org: scope.org_id.clone(),
            environment: Some(scope.environment_id.clone()),
            tenant: None,
            workspace: None,
            business_unit: None,
        };
        Ok(norn_kernel::authz::AccessRequest {
            principal: PrincipalInput {
                subject_id: actor.into(),
                actor_kind: ActorKind::User,
                agent_session_id: None,
                roles: vec![],
                attributes: BTreeMap::new(),
            },
            scope: Some(key.clone()),
            capability: "norn.read.values".into(),
            action_id: "norn.read".into(),
            resource: ResourceInput {
                object_type: object_type.into(),
                id: object_type.into(),
                scope: key,
                attributes: BTreeMap::new(),
            },
            context: BTreeMap::new(),
            binding: None,
            delegation: Delegation::Direct,
        })
    }
}

fn host() -> DevHost<NativeEngine> {
    host_with(DERIVES_LOSS)
}

fn host_with(writer: &str) -> DevHost<NativeEngine> {
    let store = melt_store(writer, crate::action_admission::prepare_action_model);
    let scope = CellScope {
        org_id: "org_demo".into(),
        environment_id: "dev".into(),
    };
    DevHost::from_store(store, scope, |command| command.payload.clone())
        .unwrap()
        .with_authorizer(Policy)
}

fn scoped(org: &str) -> ActionQuery {
    ActionQuery {
        org_id: Some(org.into()),
        environment_id: Some("dev".into()),
        version: None,
    }
}

fn call(
    host: &DevHost<NativeEngine>,
    query: &ActionQuery,
    actor: Option<&str>,
    action: &str,
    body: &str,
) -> Result<ActionReceipt, Refusal> {
    let request = decode_action_request(body)?;
    call_action(
        host,
        query,
        actor,
        action,
        &request,
        &Disclosure,
        LogicalTime::from_millis(1_700_000_000_000),
    )
}

fn dump(host: &DevHost<NativeEngine>) -> BTreeMap<String, Vec<String>> {
    host.cell().unwrap().store().read().dump().unwrap()
}

fn raw_hash(host: &DevHost<NativeEngine>, seq: u64) -> String {
    host.cell()
        .unwrap()
        .store()
        .read()
        .receipt_at(seq)
        .unwrap()
        .unwrap()
        .hash
}

#[test]
fn governed_action_commits_once_and_replays_the_original_receipt() {
    let host = host();
    let body = melt_body("melt-1", "12", "10", "draft-1");
    let committed = call(
        &host,
        &scoped("org_demo"),
        Some(OPERATOR),
        MELT_ACTION,
        &body,
    )
    .unwrap();
    assert_eq!(committed.outcome, ActionCallOutcome::Committed);
    assert_eq!(committed.object_type, MELT);
    assert_eq!(committed.object_id, "melt-1");
    assert_eq!(committed.version, 1);
    assert_eq!(committed.receipt.recorded_at_ms, 1_700_000_000_000);
    let raw = raw_hash(&host, committed.receipt.commit_seq);
    // The operator lacks whole-world Values: the keyed handle, never the raw hash.
    assert_ne!(committed.receipt.receipt_hash, raw);
    assert_eq!(
        committed.receipt.receipt_hash,
        keyed_receipt_handle(&[7; 32], &raw)
    );
    let melt = host
        .cell()
        .unwrap()
        .store()
        .read()
        .object(MAIN_WORLD, MELT, "melt-1")
        .unwrap()
        .unwrap();
    // The derived loss is exact: 12 - 10.
    assert_eq!(
        melt.values["loss_fine_g"],
        RuntimeValue::from_json_text("2").unwrap()
    );

    let after = dump(&host);
    let replayed = call(
        &host,
        &scoped("org_demo"),
        Some(OPERATOR),
        MELT_ACTION,
        &body,
    )
    .unwrap();
    assert_eq!(replayed.outcome, ActionCallOutcome::Replayed);
    assert_eq!(replayed.receipt, committed.receipt);
    assert_eq!(
        (&replayed.object_type, &replayed.object_id, replayed.version),
        (
            &committed.object_type,
            &committed.object_id,
            committed.version
        )
    );
    assert_eq!(dump(&host), after, "a replay commits nothing");
}

#[test]
fn whole_world_values_reader_sees_the_raw_receipt_hash() {
    let host = host();
    let body = melt_body("melt-a", "5", "5", "audit-1");
    let receipt = call(
        &host,
        &scoped("org_demo"),
        Some(AUDITOR),
        MELT_ACTION,
        &body,
    )
    .unwrap();
    assert_eq!(
        receipt.receipt.receipt_hash,
        raw_hash(&host, receipt.receipt.commit_seq)
    );
}

#[test]
fn denials_are_one_opaque_document_and_commit_nothing() {
    let host = host();
    let before = dump(&host);
    let body = melt_body("melt-x", "12", "10", "denied");
    let opaque = serde_json::to_string(&action_denied()).unwrap();
    let missing = ActionQuery::default();
    for (query, actor, action) in [
        (scoped("org_demo"), None, MELT_ACTION),
        (scoped("org_demo"), Some("  "), MELT_ACTION),
        (scoped("org_demo"), Some("user:stranger"), MELT_ACTION),
        (scoped("org_other"), Some(OPERATOR), MELT_ACTION),
        (missing, Some(OPERATOR), MELT_ACTION),
        (
            scoped("org_demo"),
            Some(OPERATOR),
            "constitution.melt.unknown",
        ),
    ] {
        let refused = call(&host, &query, actor, action, &body).unwrap_err();
        assert_eq!(refused.status, 403);
        assert_eq!(serde_json::to_string(&refused).unwrap(), opaque);
    }
    // A caller the host does not grant the action learns nothing from the
    // known action's own refusals: input its schema refuses and a stale
    // definitions pin read byte for byte as the unknown action does, because
    // the host grant is decided before the action is looked up.
    let invalid = melt_body("melt-x", "not-a-decimal", "10", "denied-invalid");
    let stale = ActionQuery {
        version: Some(99),
        ..scoped("org_demo")
    };
    let stranger = Some("user:stranger");
    for (query, sent) in [
        (scoped("org_demo"), invalid.as_str()),
        (stale, body.as_str()),
    ] {
        for action in [MELT_ACTION, "constitution.melt.unknown"] {
            let refused = call(&host, &query, stranger, action, sent).unwrap_err();
            assert_eq!(refused.status, 403);
            assert_eq!(serde_json::to_string(&refused).unwrap(), opaque);
        }
    }
    // The host grants every `CallAction` to HOST_ONLY and HIDDEN; the kernel
    // denies one (permission) and hides the target from the other
    // (`target_not_found`). A known action with a made-up target reads byte
    // for byte as an unknown action, so neither reveals which actions exist.
    let bogus = melt_body("melt-bogus", "12", "10", "bogus");
    let query = scoped("org_demo");
    for actor in [Some(HOST_ONLY), Some(HIDDEN)] {
        for action in [MELT_ACTION, "constitution.melt.unknown"] {
            let refused = call(&host, &query, actor, action, &bogus).unwrap_err();
            assert_eq!(refused.status, 403);
            assert_eq!(serde_json::to_string(&refused).unwrap(), opaque);
        }
    }
    assert_eq!(dump(&host), before);
}

#[test]
fn law_precondition_and_stale_pin_refuse_without_effects() {
    let operator = Some(OPERATOR);
    // A writer that records no loss breaks the fine-mass law: 10 != 8 + 0.
    let lawless = host_with(OMITS_LOSS);
    let untouched = dump(&lawless);
    let law = melt_body("melt-l", "10", "8", "law");
    let refused = call(&lawless, &scoped("org_demo"), operator, MELT_ACTION, &law).unwrap_err();
    assert_eq!(dump(&lawless), untouched);
    assert_eq!(
        (refused.status, refused.code.as_str()),
        (422, "core.laws.violated")
    );
    assert_eq!(
        refused.category,
        crate::refusal::RefusalCategory::Precondition
    );
    let host = host();
    let before = dump(&host);
    let output_above_input = melt_body("melt-p", "4", "9", "pre");
    let refused = call(
        &host,
        &scoped("org_demo"),
        operator,
        MELT_ACTION,
        &output_above_input,
    )
    .unwrap_err();
    assert_eq!(
        (refused.status, refused.code.as_str()),
        (422, "action.precondition_failed")
    );
    let stale = ActionQuery {
        version: Some(99),
        ..scoped("org_demo")
    };
    let body = melt_body("melt-s", "12", "10", "stale");
    let refused = call(&host, &stale, operator, MELT_ACTION, &body).unwrap_err();
    assert_eq!((refused.status, refused.code.as_str()), (409, "defs.stale"));
    assert_eq!(dump(&host), before);
    let current = host
        .cell()
        .unwrap()
        .store()
        .read()
        .active_definitions_version()
        .unwrap();
    let pinned = ActionQuery {
        version: current,
        ..scoped("org_demo")
    };
    assert_eq!(
        call(&host, &pinned, operator, MELT_ACTION, &body)
            .unwrap()
            .outcome,
        ActionCallOutcome::Committed
    );
}

#[test]
fn a_reused_key_with_another_payload_refuses_as_a_conflict() {
    let host = host();
    let operator = Some(OPERATOR);
    let first = melt_body("melt-r", "10", "8", "retry");
    call(&host, &scoped("org_demo"), operator, MELT_ACTION, &first).unwrap();
    let after = dump(&host);
    let changed = melt_body("melt-r", "10", "7", "retry");
    let refused = call(&host, &scoped("org_demo"), operator, MELT_ACTION, &changed).unwrap_err();
    assert_eq!(
        (refused.status, refused.code.as_str()),
        (409, "action.idempotency_conflict")
    );
    assert_eq!(dump(&host), after);
}

#[test]
fn bodies_are_bounded_and_decoded_exactly_before_any_decision() {
    let oversized = format!(
        r#"{{"input":{{"id":"{}"}},"idempotencyKey":"big"}}"#,
        "x".repeat(MAX_ACTION_BODY_BYTES)
    );
    let refused = decode_action_request(&oversized).unwrap_err();
    assert_eq!(
        (refused.status, refused.code.as_str()),
        (413, "request.too_large")
    );
    for (body, pointer) in [
        (r#"{"input":[],"idempotencyKey":"k"}"#, Some("/input")),
        (
            r#"{"input":{},"idempotencyKey":""}"#,
            Some("/idempotencyKey"),
        ),
        (
            r#"{"input":{},"idempotencyKey":"a\u0000b"}"#,
            Some("/idempotencyKey"),
        ),
        (r#"{"input":{"a":1,"a":2},"idempotencyKey":"k"}"#, None),
        (
            r#"{"input":{},"idempotencyKey":"k","actor":"user:owner"}"#,
            None,
        ),
        ("not json", None),
    ] {
        let refused = decode_action_request(body).unwrap_err();
        assert_eq!(
            (refused.status, refused.code.as_str()),
            (400, "request.invalid")
        );
        assert_eq!(
            refused
                .diagnostics
                .first()
                .and_then(|d| d.span.as_ref())
                .map(|span| span.pointer.as_str()),
            pointer,
            "{body}"
        );
    }
    let long_key = format!(r#"{{"input":{{}},"idempotencyKey":"{}"}}"#, "k".repeat(129));
    assert_eq!(decode_action_request(&long_key).unwrap_err().status, 400);
    let exact = decode_action_request(
        r#"{"input":{"q":12.500,"big":7922816251426433759354395033},"idempotencyKey":"k"}"#,
    )
    .unwrap();
    let RuntimeValue::Object(input) = exact.input else {
        panic!("object input");
    };
    assert_eq!(input["q"], RuntimeValue::from_json_text("12.5").unwrap());
    assert_eq!(
        input["big"],
        RuntimeValue::from_json_text("7922816251426433759354395033").unwrap()
    );
    // A number no exact decimal holds is refused, never rounded.
    let inexact = r#"{"input":{"big":123456789012345678901234567890},"idempotencyKey":"k"}"#;
    assert_eq!(decode_action_request(inexact).unwrap_err().status, 400);
}

#[test]
fn admission_refuses_sqlite_equivalent_internal_type_names() {
    let object: norn_ir::ObjectTypeDef =
        serde_json::from_value(serde_json::json!({"name":"norn.Actioncall"})).unwrap();
    let model = norn_cell::Model::from_definitions(&[object], &[]).unwrap();
    assert_eq!(
        crate::action_admission::prepare_action_model(&model)
            .err()
            .unwrap()
            .status,
        400
    );
}
