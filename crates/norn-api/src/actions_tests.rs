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
use action_cell::{
    DERIVES_LOSS, MELT, MELT_ACTION, OMITS_LOSS, admitted_store, melt_body, melt_store,
};
use serde_json::{Value, json};

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
impl<E: norn_cell::engine::SqlEngine> QuestionRuntime<E> for Disclosure {
    fn plan(&self, _question: &str) -> Result<QuestionPlan, Refusal> {
        Err(read_denied())
    }
    fn snapshot(
        &self,
        _store: &CellStore<E>,
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
    host_of(melt_store(
        writer,
        crate::action_admission::prepare_action_model,
    ))
}

fn host_of(store: CellStore<NativeEngine>) -> DevHost<NativeEngine> {
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
    call_at(
        host,
        query,
        actor,
        action,
        body,
        LogicalTime::from_millis(1_700_000_000_000),
    )
}

fn call_at(
    host: &DevHost<NativeEngine>,
    query: &ActionQuery,
    actor: Option<&str>,
    action: &str,
    body: &str,
    now: LogicalTime,
) -> Result<ActionReceipt, Refusal> {
    let request = decode_action_request(body)?;
    match call_action(host, query, actor, action, &request, &Disclosure, now)? {
        ActionAnswer::Receipt(receipt) => Ok(receipt),
        ActionAnswer::Pending(_) => Err(Refusal::host("no approval gates these actions")),
    }
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

const ORDER_EDIT: &str = "test.order.edit";

fn envelope(artifact_id: &str, kind: &str, payload: &Value, dependencies: &[&str]) -> Value {
    let dependencies: Vec<Value> = dependencies
        .iter()
        .map(|id| json!({ "artifactId": id, "kind": "entity", "version": "1.0.0", "required": true }))
        .collect();
    json!({
        "irVersion": "0.1",
        "artifactId": artifact_id,
        "kind": kind,
        "namespace": "test",
        "name": artifact_id.split('.').nth(1).unwrap(),
        "version": "1.0.0",
        "lifecycle": "draft",
        "scope": { "orgId": "org_demo", "environmentId": "dev" },
        "dependencies": dependencies,
        "provenance": { "source": "authoring_sdk", "sourcePath": "crates/norn-api/src/actions_tests.rs" },
        "payload": payload
    })
}

fn entity(artifact_id: &str, object_type: &Value, dependencies: &[&str]) -> Value {
    envelope(
        artifact_id,
        "entity",
        &json!({ "entityVersion": "2.0", "objectType": object_type }),
        dependencies,
    )
}

fn action_artifact(
    artifact_id: &str,
    action_id: &str,
    input_schema: &Value,
    writes: &Value,
    effect: &Value,
    dependencies: &[&str],
) -> Value {
    envelope(
        artifact_id,
        "action",
        &json!({
            "actionId": action_id,
            "scopeRequired": ["orgId", "environmentId"],
            "idempotent": true,
            "ledgered": true,
            "inputSchema": input_schema,
            "outputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "writes": writes,
            "effectProgram": effect
        }),
        dependencies,
    )
}

/// `test.order.edit` patches a `test.Order` and its owned `lines` rows.
fn rows_documents() -> Vec<Value> {
    let order = entity(
        "test.Order.entity",
        &json!({
            "name": "test.Order",
            "properties": [{ "id": "title", "type": { "kind": "text" }, "constraints": { "nullable": true } }],
            "links": [{ "id": "lines", "target": "test.OrderLine", "cardinality": { "to": "many", "from": "one" } }],
            "ownedCollections": [{
                "id": "lines", "childType": "test.OrderLine", "rowIdProperty": "row_id",
                "orderingKey": "idx", "cascadeOnParentDelete": true, "maxRows": 20
            }]
        }),
        &["test.OrderLine.entity"],
    );
    let line = entity(
        "test.OrderLine.entity",
        &json!({
            "name": "test.OrderLine",
            "properties": [
                { "id": "row_id", "type": { "kind": "text" } },
                { "id": "idx", "type": { "kind": "integer" } },
                { "id": "label", "type": { "kind": "text" } },
                { "id": "quantity", "type": { "kind": "integer" } },
                { "id": "double_quantity", "type": { "kind": "integer" } }
            ]
        }),
        &[],
    );
    let any = json!({ "type": ["null", "boolean", "number", "string", "array", "object"] });
    let edit = action_artifact(
        "test.Order.edit.action",
        ORDER_EDIT,
        &json!({
            "type": "object",
            "properties": {
                "parentId": { "type": "string" },
                "parentValues": any,
                "parentVersion": { "type": "string" },
                "rowOps": any
            },
            "required": ["parentId", "parentValues", "parentVersion", "rowOps"],
            "additionalProperties": false
        }),
        &json!([
            { "entityKind": "test.Order", "change": "created" },
            { "entityKind": "test.Order", "change": "modified" },
            { "entityKind": "test.OrderLine", "change": "created" },
            { "entityKind": "test.OrderLine", "change": "modified" },
            { "entityKind": "test.OrderLine", "change": "deleted" }
        ]),
        &json!({
            "kind": "mutate_parent_with_rows",
            "objectIdField": "parentId",
            "objectKind": "test.Order",
            "entityArtifactId": "test.Order.entity",
            "entityArtifactVersion": "1.0.0",
            "parentValuesField": "parentValues",
            "rowOpsField": "rowOps",
            "parentVersionField": "parentVersion",
            "collections": [{
                "id": "lines",
                "preconditions": ["self.quantity > 0"],
                "derive": { "double_quantity": "self.quantity * 2" }
            }]
        }),
        &["test.Order.entity", "test.OrderLine.entity"],
    );
    vec![order, line, edit]
}

fn rows_host() -> DevHost<NativeEngine> {
    host_of(admitted_store(
        &rows_documents(),
        crate::action_admission::prepare_action_model,
    ))
}

#[test]
fn the_kernel_turn_runs_on_the_cell_clock_a_refusal_advanced() {
    let host = host();
    let query = scoped("org_demo");
    let later = LogicalTime::from_millis(1_767_225_600_000);
    let earlier = LogicalTime::from_millis(1_767_225_599_000);
    // The kernel hides every target from HIDDEN: refused after the cell clock
    // moved to `later`, committing nothing, so the stored floor stays behind.
    let hidden = melt_body("melt-h", "12", "10", "hidden");
    let refused = call_at(&host, &query, Some(HIDDEN), MELT_ACTION, &hidden, later).unwrap_err();
    assert_eq!(refused.status, 403);
    // A call carrying the older time still runs its turn (and so any naming
    // series it allocates) on the clock its receipt records.
    assert_eq!(
        turn_clock(&mut host.cell().unwrap(), earlier).unwrap(),
        later
    );
    let body = melt_body("melt-t", "12", "10", "turn");
    let receipt = call_at(&host, &query, Some(OPERATOR), MELT_ACTION, &body, earlier).unwrap();
    assert_eq!(receipt.receipt.recorded_at_ms, later.as_millis());
}

#[test]
fn a_call_that_leaves_its_target_unwritten_reports_its_current_version() {
    let host = rows_host();
    let query = scoped("org_demo");
    let operator = Some(OPERATOR);
    let body = |values: &str, version: &str, key: &str| {
        format!(
            r#"{{"input":{{"parentId":"order-1","parentValues":{values},"parentVersion":"{version}","rowOps":[]}},"idempotencyKey":"{key}"}}"#
        )
    };
    let created = call(
        &host,
        &query,
        operator,
        ORDER_EDIT,
        &body(r#"{"title":"first"}"#, "0", "create"),
    )
    .unwrap();
    assert_eq!(created.outcome, ActionCallOutcome::Committed);
    // An empty parent patch and no row operations: the kernel commits the call
    // record without rewriting the order.
    let unchanged = body("{}", &created.version.to_string(), "noop");
    let committed = call(&host, &query, operator, ORDER_EDIT, &unchanged).unwrap();
    assert_eq!(committed.outcome, ActionCallOutcome::Committed);
    assert_eq!(
        (committed.object_id.as_str(), committed.version),
        ("order-1", created.version)
    );
    assert!(committed.receipt.commit_seq > created.receipt.commit_seq);
    let replayed = call(&host, &query, operator, ORDER_EDIT, &unchanged).unwrap();
    assert_eq!(replayed.outcome, ActionCallOutcome::Replayed);
    assert_eq!(
        (replayed.version, &replayed.receipt),
        (committed.version, &committed.receipt)
    );
}

/// S5b: a call that left its target unwritten reports the target's version
/// as of its own commit, on every replay: a later write to the target never
/// changes what the call's recovery or replay reports.
#[test]
fn a_replay_reports_the_version_as_of_its_commit_after_a_later_write() {
    let host = rows_host();
    let query = scoped("org_demo");
    let operator = Some(OPERATOR);
    let body = |values: &str, version: &str, key: &str| {
        format!(
            r#"{{"input":{{"parentId":"order-1","parentValues":{values},"parentVersion":"{version}","rowOps":[]}},"idempotencyKey":"{key}"}}"#
        )
    };
    let created = call(
        &host,
        &query,
        operator,
        ORDER_EDIT,
        &body(r#"{"title":"first"}"#, "0", "create"),
    )
    .unwrap();
    let unchanged = body("{}", &created.version.to_string(), "noop");
    let committed = call(&host, &query, operator, ORDER_EDIT, &unchanged).unwrap();
    assert_eq!(committed.version, created.version);
    let later = call(
        &host,
        &query,
        operator,
        ORDER_EDIT,
        &body(
            r#"{"title":"second"}"#,
            &created.version.to_string(),
            "later",
        ),
    )
    .unwrap();
    assert!(later.version > created.version);
    let replayed = call(&host, &query, operator, ORDER_EDIT, &unchanged).unwrap();
    assert_eq!(replayed.outcome, ActionCallOutcome::Replayed);
    assert_eq!(
        (replayed.version, &replayed.receipt),
        (committed.version, &committed.receipt),
        "the version as of the call's commit, never the later write's"
    );
}

/// ADMIT-CACHE-1 (#1533): one melt cell on a file, provisioned with `writer`,
/// and a byte copy of it: the same cell twice.
fn twin_files(writer: &str) -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("a.sqlite"), dir.path().join("b.sqlite"));
    drop(open_melt(&a, writer));
    for suffix in ["", "-wal"] {
        let from = std::path::PathBuf::from(format!("{}{suffix}", a.display()));
        if from.exists() {
            std::fs::copy(&from, format!("{}{suffix}", b.display())).unwrap();
        }
    }
    (dir, a, b)
}

/// The melt cell on the file `path`, opened (and its definitions verified)
/// afresh: a new store, a new model and a host with nothing cached.
fn open_melt(path: &std::path::Path, writer: &str) -> DevHost<NativeEngine> {
    let engine = NativeEngine::open(path, norn_cell::StoreOptions::default()).unwrap();
    host_of(action_cell::melt_store_on(
        engine,
        writer,
        crate::action_admission::prepare_action_model,
    ))
}

/// Every call of an ERP-shaped sequence over the melt action: commits, a
/// replay, a reused key with another payload, a refused precondition, a
/// pinned version (current and stale), an unknown action and a denied actor.
fn mixed_calls() -> Vec<(ActionQuery, Option<&'static str>, &'static str, String)> {
    let current = ActionQuery {
        version: Some(1),
        ..scoped("org_demo")
    };
    let stale = ActionQuery {
        version: Some(9),
        ..scoped("org_demo")
    };
    let mut calls = Vec::new();
    for (n, output) in ["10", "9", "8", "10", "9", "8"].into_iter().enumerate() {
        let body = melt_body(&format!("melt-{n}"), "12", output, &format!("k{n}"));
        calls.push((
            scoped("org_demo"),
            Some(OPERATOR),
            MELT_ACTION,
            body.clone(),
        ));
        calls.push((scoped("org_demo"), Some(OPERATOR), MELT_ACTION, body));
    }
    calls.push((
        scoped("org_demo"),
        Some(OPERATOR),
        MELT_ACTION,
        melt_body("melt-0", "12", "7", "k0"),
    ));
    calls.push((
        scoped("org_demo"),
        Some(OPERATOR),
        MELT_ACTION,
        melt_body("melt-p", "4", "9", "pre"),
    ));
    calls.push((
        current,
        Some(OPERATOR),
        MELT_ACTION,
        melt_body("melt-v", "12", "10", "pin"),
    ));
    calls.push((
        stale,
        Some(OPERATOR),
        MELT_ACTION,
        melt_body("melt-s", "12", "10", "stale"),
    ));
    calls.push((
        scoped("org_demo"),
        Some(OPERATOR),
        "shop.unknown",
        melt_body("melt-u", "12", "10", "u"),
    ));
    calls.push((
        scoped("org_demo"),
        Some(HOST_ONLY),
        MELT_ACTION,
        melt_body("melt-h", "12", "10", "h"),
    ));
    calls.push((
        scoped("org_demo"),
        Some(AUDITOR),
        MELT_ACTION,
        melt_body("melt-a", "12", "10", "a"),
    ));
    calls.push((
        scoped("org_other"),
        Some(OPERATOR),
        MELT_ACTION,
        melt_body("melt-o", "12", "10", "o"),
    ));
    calls
}

#[test]
fn cached_and_uncached_definitions_admit_and_refuse_identically() {
    // ADMIT-CACHE-1 (#1533): a host keeps the verified definitions, the
    // caller's PDP and the model's catalog work across calls. The reference
    // reopens its copy of the same cell for every call, so each call verifies
    // the retained set and derives every catalog from nothing. Every answer,
    // and the whole store after, must be byte-identical.
    for writer in [DERIVES_LOSS, OMITS_LOSS] {
        let (_dir, a, b) = twin_files(writer);
        let cached = open_melt(&a, writer);
        let mut answers = (Vec::new(), Vec::new());
        for (n, (query, actor, action, body)) in mixed_calls().into_iter().enumerate() {
            let now = LogicalTime::from_millis(1_700_000_000_000 + u64::try_from(n).unwrap());
            answers.0.push(format!(
                "{:?}",
                call_at(&cached, &query, actor, action, &body, now)
            ));
            let fresh = open_melt(&b, writer);
            answers.1.push(format!(
                "{:?}",
                call_at(&fresh, &query, actor, action, &body, now)
            ));
        }
        assert_eq!(answers.0, answers.1, "{writer}");
        // The lawful writer commits; the lawless one is refused by the law.
        let expected = if writer == DERIVES_LOSS {
            "Ok"
        } else {
            "core.laws.violated"
        };
        assert!(
            answers.0.iter().any(|answer| answer.contains(expected)),
            "{answers:?}"
        );
        assert!(
            answers.0.iter().any(|answer| answer.starts_with("Err")),
            "{answers:?}"
        );
        assert_eq!(dump(&cached), dump(&open_melt(&b, writer)), "{writer}");
    }
}

#[test]
fn a_changed_definitions_identity_is_verified_again_before_any_call() {
    // ADMIT-CACHE-1: the cache serves only the identity it verified. Another
    // receipt digest on the active row (here a forged one) misses and is
    // verified, and the forged receipt refuses the call.
    let (_dir, a, _b) = twin_files(DERIVES_LOSS);
    let host = open_melt(&a, DERIVES_LOSS);
    let operator = Some(OPERATOR);
    call(
        &host,
        &scoped("org_demo"),
        operator,
        MELT_ACTION,
        &melt_body("melt-1", "12", "10", "one"),
    )
    .unwrap();
    let raw = NativeEngine::open(&a, norn_cell::StoreOptions::default()).unwrap();
    norn_cell::engine::SqlEngine::definition_writes(&raw, || {
        norn_cell::engine::SqlEngine::execute(
            &raw,
            "UPDATE \"_norn_definition_version\" SET \"receipt_sha256\" = ?1 WHERE \"version\" = 1",
            &[norn_cell::engine::SqlValue::Text("0".repeat(64))],
        )
    })
    .unwrap();
    drop(raw);
    let before = dump(&host);
    let refused = call(
        &host,
        &scoped("org_demo"),
        operator,
        MELT_ACTION,
        &melt_body("melt-2", "12", "10", "two"),
    )
    .unwrap_err();
    assert_eq!(refused.status, 500, "{refused:?}");
    assert_eq!(dump(&host), before, "a refused call writes nothing");
}

/// Edit `column` of `table`'s version-1 row in place through another
/// connection, as an out-of-process editor would; the receipt digest column
/// on the active row stays as it was.
fn edit_in_place(path: &std::path::Path, table: &str, column: &str, edit: fn(&mut Value)) {
    use norn_cell::engine::{SqlEngine, SqlValue};
    let raw = NativeEngine::open(path, norn_cell::StoreOptions::default()).unwrap();
    let digest = |raw: &NativeEngine| {
        raw.query_row(
            "SELECT \"receipt_sha256\" FROM \"_norn_definition_version\" WHERE \"version\" = 1",
            &[],
            |row| row.get::<String>(0),
        )
        .unwrap()
    };
    let held = digest(&raw);
    let text = raw
        .query_row(
            &format!("SELECT \"{column}\" FROM \"{table}\" WHERE \"version\" = 1"),
            &[],
            |row| row.get::<String>(0),
        )
        .unwrap();
    let mut value: Value = serde_json::from_str(&text).unwrap();
    edit(&mut value);
    let edited = serde_json::to_string(&value).unwrap();
    assert_ne!(edited, text, "the edit changes the retained bytes");
    raw.definition_writes(|| {
        raw.execute(
            &format!("UPDATE \"{table}\" SET \"{column}\" = ?1 WHERE \"version\" = 1"),
            &[SqlValue::Text(edited)],
        )
    })
    .unwrap();
    assert_eq!(digest(&raw), held, "the digest column is held constant");
}

#[test]
fn a_warm_host_refuses_definitions_edited_in_place_as_a_fresh_host_does() {
    // C1 (#1547): the cache is keyed by the definitions identity and by the
    // store's foreign-write revision, so an in-place edit of the retained
    // set or of its receipt that keeps the digest column misses the warm
    // cache and is verified again. The warm host must refuse exactly as a
    // host opened afresh on an identical, identically edited file.
    let drop_last_artifact: fn(&mut Value) = |set| {
        set.as_array_mut().unwrap().pop().unwrap();
    };
    let annotate_receipt: fn(&mut Value) = |receipt| {
        receipt
            .as_object_mut()
            .unwrap()
            .insert("edited".into(), json!(true));
    };
    for (table, column, edit) in [
        (
            "_norn_definition_version",
            "canonical_set",
            drop_last_artifact,
        ),
        (
            "_norn_definition_admission_receipt",
            "receipt",
            annotate_receipt,
        ),
    ] {
        let (_dir, a, b) = twin_files(DERIVES_LOSS);
        let warm = open_melt(&a, DERIVES_LOSS);
        let operator = Some(OPERATOR);
        let first = LogicalTime::from_millis(1_700_000_000_000);
        let later = LogicalTime::from_millis(1_700_000_000_001);
        let warming = melt_body("melt-1", "12", "10", "one");
        // The first call verifies and keeps the retained set; the twin takes
        // the same commit, so both files hold the same cell.
        call_at(
            &warm,
            &scoped("org_demo"),
            operator,
            MELT_ACTION,
            &warming,
            first,
        )
        .unwrap();
        call_at(
            &open_melt(&b, DERIVES_LOSS),
            &scoped("org_demo"),
            operator,
            MELT_ACTION,
            &warming,
            first,
        )
        .unwrap();
        for path in [&a, &b] {
            edit_in_place(path, table, column, edit);
        }
        let before = dump(&warm);
        let body = melt_body("melt-2", "12", "10", "two");
        let answer = |host: &DevHost<NativeEngine>| {
            call_at(
                host,
                &scoped("org_demo"),
                operator,
                MELT_ACTION,
                &body,
                later,
            )
        };
        let warm_answer = answer(&warm);
        let fresh = {
            let (model, input) = action_cell::melt_install(
                DERIVES_LOSS,
                crate::action_admission::prepare_action_model,
                1,
            );
            let engine = NativeEngine::open(&b, norn_cell::StoreOptions::default()).unwrap();
            CellStore::provision_definitions(engine, model, &input, "org_demo/dev").map(host_of)
        };
        let refused = warm_answer.as_ref().expect_err(column);
        match fresh {
            // A fresh host that opens refuses the call itself: the same answer.
            Ok(fresh) => assert_eq!(
                format!("{warm_answer:?}"),
                format!("{:?}", answer(&fresh)),
                "{column}"
            ),
            // A fresh host that refuses to open serves nothing: a host refusal.
            Err(_refused_open) => assert_eq!(refused.status, 500, "{column}: {refused:?}"),
        }
        assert_eq!(
            dump(&warm),
            before,
            "{column}: a refused call writes nothing"
        );
        // A refused verification is never kept: the next call refuses again.
        assert_eq!(
            format!("{:?}", answer(&warm)),
            format!("{warm_answer:?}"),
            "{column}"
        );
    }
}

/// The retained definition bytes of `path`, read through another connection.
fn retained_bytes(path: &std::path::Path) -> Vec<String> {
    use norn_cell::engine::SqlEngine;
    let raw = NativeEngine::open(path, norn_cell::StoreOptions::default()).unwrap();
    raw.query(
        "SELECT \"canonical_set\" || ' ' || \"receipt_sha256\" FROM \"_norn_definition_version\" ORDER BY \"version\"",
        &[],
    )
    .unwrap()
    .into_iter()
    .map(|row| format!("{row:?}"))
    .collect()
}

#[test]
fn a_trigger_installed_from_outside_cannot_edit_the_definitions_on_a_host_commit() {
    // S1 (#1547): another connection adds a trigger that rewrites the
    // retained set on an ordinary host commit. That write rides the serving
    // connection's own commit, which `data_version` does not count, so the
    // warm cache would serve the old actions while a fresh host refuses. The
    // serving connection refuses any statement that fires such a trigger, so
    // the warm host answers as a fresh host does and the set is untouched.
    use norn_cell::engine::SqlEngine;
    let (_dir, a, b) = twin_files(DERIVES_LOSS);
    let warm = open_melt(&a, DERIVES_LOSS);
    let operator = Some(OPERATOR);
    let first = LogicalTime::from_millis(1_700_000_000_000);
    let later = LogicalTime::from_millis(1_700_000_000_001);
    let warming = melt_body("melt-1", "12", "10", "one");
    for host in [&warm, &open_melt(&b, DERIVES_LOSS)] {
        call_at(
            host,
            &scoped("org_demo"),
            operator,
            MELT_ACTION,
            &warming,
            first,
        )
        .unwrap();
    }
    for path in [&a, &b] {
        let raw = NativeEngine::open(path, norn_cell::StoreOptions::default()).unwrap();
        raw.execute_batch(
            "CREATE TRIGGER \"rewrite_definitions\" AFTER INSERT ON \"_norn_ledger\" BEGIN \
             UPDATE \"_norn_definition_version\" SET \"canonical_set\" = '[]' WHERE \"version\" = 1; \
             END",
        )
        .unwrap();
    }
    let retained = retained_bytes(&a);
    let before = dump(&warm);
    let body = melt_body("melt-2", "12", "10", "two");
    let answer = |host: &DevHost<NativeEngine>| {
        call_at(
            host,
            &scoped("org_demo"),
            operator,
            MELT_ACTION,
            &body,
            later,
        )
    };
    let warm_answer = answer(&warm);
    let refused = warm_answer
        .as_ref()
        .expect_err("the commit fires the trigger");
    assert_eq!(refused.status, 500, "{refused:?}");
    assert_eq!(
        retained_bytes(&a),
        retained,
        "the retained set is untouched"
    );
    assert_eq!(dump(&warm), before, "a refused call writes nothing");
    // The next call refuses the same way: nothing stale was served or kept.
    assert_eq!(format!("{:?}", answer(&warm)), format!("{warm_answer:?}"));
    let fresh = {
        let (model, input) = action_cell::melt_install(
            DERIVES_LOSS,
            crate::action_admission::prepare_action_model,
            1,
        );
        let engine = NativeEngine::open(&b, norn_cell::StoreOptions::default()).unwrap();
        CellStore::provision_definitions(engine, model, &input, "org_demo/dev").map(host_of)
    };
    if let Ok(fresh) = fresh {
        assert_eq!(format!("{:?}", answer(&fresh)), format!("{warm_answer:?}"));
    }
}

#[test]
fn an_install_drops_the_verified_definitions_of_the_last_revision() {
    // ADMIT-CACHE-1: an install clears the cache, and the next call runs under
    // the new revision, verified on its first call.
    let host = host();
    let operator = Some(OPERATOR);
    let pinned = |version| ActionQuery {
        version: Some(version),
        ..scoped("org_demo")
    };
    call(
        &host,
        &pinned(1),
        operator,
        MELT_ACTION,
        &melt_body("melt-1", "12", "10", "one"),
    )
    .unwrap();
    let (model, input) = action_cell::melt_install(
        DERIVES_LOSS,
        crate::action_admission::prepare_action_model,
        // After the first call: the cell's clock never runs backwards.
        1_700_000_000_001,
    );
    let later = LogicalTime::from_millis(1_700_000_000_002);
    assert_eq!(
        host.install_definitions(model, input, Some(1))
            .unwrap()
            .version,
        2
    );
    let stale = call_at(
        &host,
        &pinned(1),
        operator,
        MELT_ACTION,
        &melt_body("melt-2", "12", "10", "two"),
        later,
    )
    .unwrap_err();
    assert_eq!((stale.status, stale.code.as_str()), (409, "defs.stale"));
    let committed = call_at(
        &host,
        &pinned(2),
        operator,
        MELT_ACTION,
        &melt_body("melt-3", "12", "10", "three"),
        later,
    )
    .unwrap();
    assert_eq!(committed.outcome, ActionCallOutcome::Committed);
}

#[test]
fn action_request_and_receipt_thread_optional_trace_and_refuse_invalid_trace() {
    let host = host();
    let query = scoped("org_demo");
    let digest = format!("sha256:{}", "d".repeat(64));

    // Omitting trace omits `trace` in the serialized receipt JSON.
    let plain = call(
        &host,
        &query,
        Some(OPERATOR),
        MELT_ACTION,
        &melt_body("melt-plain", "12", "10", "k-plain"),
    )
    .unwrap();
    assert_eq!(plain.trace, None);
    let plain_json = serde_json::to_value(&plain).unwrap();
    assert!(
        plain_json.get("trace").is_none(),
        "absent trace must stay omitted from serialized ActionReceipt: {plain_json}"
    );

    // Supplying trace persists and returns it on commit and replay.
    let traced_body = json!({
        "input": { "id": "melt-traced", "input_fine_g": "12", "output_fine_g": "10" },
        "idempotencyKey": "k-traced",
        "trace": {
            "traceId": "trace-api-1",
            "intentRef": "prompt:sha256:intent",
            "sessionDigest": digest,
        }
    })
    .to_string();
    let committed = call(&host, &query, Some(OPERATOR), MELT_ACTION, &traced_body).unwrap();
    let expected = TraceContext {
        trace_id: "trace-api-1".to_owned(),
        intent_ref: Some("prompt:sha256:intent".to_owned()),
        session_digest: Some(digest),
    };
    assert_eq!(committed.outcome, ActionCallOutcome::Committed);
    assert_eq!(committed.trace.as_ref(), Some(&expected));
    let replayed = call(&host, &query, Some(OPERATOR), MELT_ACTION, &traced_body).unwrap();
    assert_eq!(replayed.outcome, ActionCallOutcome::Replayed);
    assert_eq!(replayed.trace.as_ref(), Some(&expected));
    assert_eq!(replayed.receipt, committed.receipt);

    // Conflicting trace under the same idempotency key refuses 409.
    let conflict_body = json!({
        "input": { "id": "melt-traced", "input_fine_g": "12", "output_fine_g": "10" },
        "idempotencyKey": "k-traced",
        "trace": { "traceId": "trace-api-other" }
    })
    .to_string();
    let conflict = call(&host, &query, Some(OPERATOR), MELT_ACTION, &conflict_body).unwrap_err();
    assert_eq!(
        (conflict.status, conflict.code.as_str()),
        (409, "action.idempotency_conflict")
    );

    // Malformed trace refuses 400 before touching storage.
    let before = dump(&host);
    for bad_trace in [
        json!(null),
        json!(["tr-1"]),
        json!(["tr-1", "goal:1"]),
        json!({}),
        json!({ "traceId": "" }),
        json!({ "traceId": "tr\u{202e}1" }),
        json!({ "traceId": "t1", "intentRef": "goal\u{2028}1" }),
        json!({ "traceId": "t1", "sessionDigest": "raw transcript text" }),
        json!({ "traceId": "t1", "unknown": "x" }),
        json!({ "traceId": "t".repeat(257) }),
        json!({ "traceId": "t1", "intentRef": null }),
    ] {
        let body = json!({
            "input": { "id": "melt-bad", "input_fine_g": "12", "output_fine_g": "10" },
            "idempotencyKey": "k-bad",
            "trace": bad_trace,
        })
        .to_string();
        let refused = decode_action_request(&body).unwrap_err();
        assert_eq!(refused.status, 400, "bad trace must refuse 400: {body}");
    }
    assert_eq!(dump(&host), before, "malformed trace must mutate nothing");
}

fn parameter_entity_documents() -> Vec<Value> {
    let scope = json!({"orgId": "org_demo", "environmentId": "dev"});
    let provenance = json!({"source": "authoring_sdk", "sourcePath": "test/parameter.ts"});
    vec![
        json!({
            "irVersion": "0.1", "artifactId": "billing.setting.vat_rate",
            "kind": "setting", "namespace": "billing", "name": "VAT Rate",
            "version": "1.0.0", "lifecycle": "draft", "scope": scope,
            "dependencies": [], "provenance": provenance,
            "payload": {
                "settingVersion": "0.1", "settingKey": "billing.vat_rate",
                "valueType": "number", "value": "0.10",
                "effectiveFrom": "2026-06-01T00:00:00Z", "effectiveTo": "2026-07-01T00:00:00Z",
                "review": {"state": "approved"}, "scopeRequired": ["orgId", "environmentId"]
            }
        }),
        json!({
            "irVersion": "0.1", "artifactId": "billing.NumericSetting.entity",
            "kind": "entity", "namespace": "billing", "name": "Numeric Setting",
            "version": "1.0.0", "lifecycle": "draft", "scope": scope,
            "dependencies": [], "provenance": provenance,
            "payload": {
                "entityVersion": "2.0",
                "objectType": {
                    "name": "billing.NumericSetting",
                    "properties": [
                        {"id": "id", "type": {"kind": "text"}},
                        {"id": "setting_key", "type": {"kind": "text"}},
                        {"id": "value", "type": {"kind": "decimal", "scale": 4}},
                        {"id": "effective_from", "type": {"kind": "timestamp", "timezone": "utc"}},
                        {"id": "effective_to", "type": {"kind": "timestamp", "timezone": "utc"}, "constraints": {"nullable": true}},
                        {"id": "review_state", "type": {"kind": "text"}},
                        {"id": "org_id", "type": {"kind": "text"}, "constraints": {"nullable": true}},
                        {"id": "tenant_id", "type": {"kind": "text"}, "constraints": {"nullable": true}},
                        {"id": "workspace_id", "type": {"kind": "text"}, "constraints": {"nullable": true}},
                        {"id": "environment_id", "type": {"kind": "text"}, "constraints": {"nullable": true}},
                        {"id": "business_unit_id", "type": {"kind": "text"}, "constraints": {"nullable": true}}
                    ],
                    "identity": [{"name": "primary", "properties": ["id"]}]
                }
            }
        }),
        json!({
            "irVersion": "0.1", "artifactId": "billing.SettingRegistry.entity",
            "kind": "entity", "namespace": "billing", "name": "Setting Registry",
            "version": "1.0.0", "lifecycle": "draft", "scope": scope,
            "dependencies": [{"artifactId": "billing.NumericSetting.entity", "kind": "entity", "version": "1.0.0", "required": true}],
            "provenance": provenance,
            "payload": {
                "entityVersion": "2.0",
                "objectType": {
                    "name": "billing.SettingRegistry",
                    "properties": [{"id": "id", "type": {"kind": "text"}}],
                    "identity": [{"name": "primary", "properties": ["id"]}],
                    "links": [{"id": "settings", "target": "billing.NumericSetting", "cardinality": {"to": "many", "from": "many"}}]
                }
            }
        }),
        json!({
            "irVersion": "0.1", "artifactId": "billing.TaxReceipt.entity",
            "kind": "entity", "namespace": "billing", "name": "Tax Receipt",
            "version": "1.0.0", "lifecycle": "draft", "scope": scope,
            "dependencies": [], "provenance": provenance,
            "payload": {
                "entityVersion": "2.0",
                "objectType": {
                    "name": "billing.TaxReceipt",
                    "properties": [
                        {"id": "id", "type": {"kind": "text"}},
                        {"id": "registry_id", "type": {"kind": "text"}},
                        {"id": "tax_rate", "type": {"kind": "decimal", "scale": 4}},
                        {"id": "effective_at", "type": {"kind": "text"}}
                    ],
                    "identity": [{"name": "primary", "properties": ["id"]}]
                }
            }
        }),
    ]
}

fn parameter_action_document() -> Value {
    json!({
        "irVersion": "0.1", "artifactId": "billing.tax_receipt.record.action",
        "kind": "action", "namespace": "billing", "name": "Record Tax Receipt",
        "version": "1.0.0", "lifecycle": "draft",
        "scope": {"orgId": "org_demo", "environmentId": "dev"},
        "dependencies": [
            {"artifactId": "billing.TaxReceipt.entity", "kind": "entity", "version": "1.0.0", "required": true},
            {"artifactId": "billing.SettingRegistry.entity", "kind": "entity", "version": "1.0.0", "required": true},
            {"artifactId": "billing.NumericSetting.entity", "kind": "entity", "version": "1.0.0", "required": true}
        ],
        "provenance": {"source": "authoring_sdk", "sourcePath": "test/parameter.ts"},
        "payload": {
            "actionId": "billing.tax_receipt.record",
            "scopeRequired": ["orgId", "environmentId"],
            "idempotent": true, "ledgered": true,
            "inputSchema": {
                "type": "object", "additionalProperties": false,
                "properties": {
                    "id": {"type": "string"},
                    "registry_id": {"type": "string"},
                    "tax_rate": {"type": "number", "nornType": {"kind": "decimal", "scale": 4}},
                    "effective_at": {"type": "string"}
                },
                "required": ["id", "registry_id", "tax_rate", "effective_at"]
            },
            "outputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
            "slice": [
                {"name": "registry", "objectType": "billing.SettingRegistry", "key": {"property": {"from": "input", "property": "registry_id"}}},
                {"name": "settings", "objectType": "billing.NumericSetting", "key": {"link": {"from": "registry", "link": "settings"}}},
                {"name": "tax", "objectType": "billing.NumericSetting", "key": {"parameter": {
                    "from": "settings", "settingKey": "billing.vat_rate", "effectiveAt": "effective_at",
                    "precedence": [{"orgId": "org_demo", "environmentId": "dev"}, {"orgId": "org_demo"}, {}]
                }}}
            ],
            "preconditions": ["input.tax_rate == slice.tax.value"],
            "writes": [{"entityKind": "billing.TaxReceipt", "change": "created"}],
            "effectProgram": {
                "kind": "upsert_input_object", "objectKind": "billing.TaxReceipt",
                "objectIdField": "id", "entityArtifactId": "billing.TaxReceipt.entity",
                "entityArtifactVersion": "1.0.0", "writeMode": {"kind": "create_only"}
            }
        }
    })
}

fn api_setting_row(
    id: &str,
    value: &str,
    from: &str,
    to: Option<&str>,
) -> BTreeMap<String, RuntimeValue> {
    BTreeMap::from([
        ("id".to_owned(), RuntimeValue::String(id.to_owned())),
        (
            "setting_key".to_owned(),
            RuntimeValue::String("billing.vat_rate".to_owned()),
        ),
        (
            "value".to_owned(),
            RuntimeValue::Number(norn_ir::Decimal::parse(value).unwrap()),
        ),
        (
            "effective_from".to_owned(),
            RuntimeValue::String(from.to_owned()),
        ),
        (
            "effective_to".to_owned(),
            to.map_or(RuntimeValue::Null, |s| RuntimeValue::String(s.to_owned())),
        ),
        (
            "review_state".to_owned(),
            RuntimeValue::String("approved".to_owned()),
        ),
        (
            "org_id".to_owned(),
            RuntimeValue::String("org_demo".to_owned()),
        ),
        ("tenant_id".to_owned(), RuntimeValue::Null),
        ("workspace_id".to_owned(), RuntimeValue::Null),
        (
            "environment_id".to_owned(),
            RuntimeValue::String("dev".to_owned()),
        ),
        ("business_unit_id".to_owned(), RuntimeValue::Null),
    ])
}

fn seed_parameter_settings(host: &DevHost<NativeEngine>) {
    use norn_kernel::{
        CellCommit, CellHost, FactKey, FactMutation, FactVersion, FactWrite, LinkKey, LinkMutation,
        LinkWrite,
    };
    let mut cell = host.cell().unwrap();
    let reg = FactKey::new("billing.SettingRegistry", "reg-main");
    let june = FactKey::new("billing.NumericSetting", "vat-june");
    let july = FactKey::new("billing.NumericSetting", "vat-july");
    CellHost::commit(
        &mut *cell,
        CellCommit {
            writes: vec![
                FactWrite {
                    key: reg.clone(),
                    expected: FactVersion::ABSENT,
                    mutation: FactMutation::Put {
                        values: BTreeMap::from([(
                            "id".to_owned(),
                            RuntimeValue::String("reg-main".to_owned()),
                        )]),
                    },
                },
                FactWrite {
                    key: june.clone(),
                    expected: FactVersion::ABSENT,
                    mutation: FactMutation::Put {
                        values: api_setting_row(
                            "vat-june",
                            "0.10",
                            "2026-06-01T00:00:00Z",
                            Some("2026-07-01T00:00:00Z"),
                        ),
                    },
                },
                FactWrite {
                    key: july.clone(),
                    expected: FactVersion::ABSENT,
                    mutation: FactMutation::Put {
                        values: api_setting_row("vat-july", "0.12", "2026-07-01T00:00:00Z", None),
                    },
                },
            ],
            links: vec![
                LinkWrite {
                    key: LinkKey::new(reg.clone(), "settings", june),
                    expected: FactVersion::ABSENT,
                    mutation: LinkMutation::Add {
                        values: BTreeMap::new(),
                    },
                },
                LinkWrite {
                    key: LinkKey::new(reg, "settings", july),
                    expected: FactVersion::ABSENT,
                    mutation: LinkMutation::Add {
                        values: BTreeMap::new(),
                    },
                },
            ],
            ..CellCommit::empty()
        },
    )
    .unwrap();
}

#[test]
fn parameter_slice_executes_end_to_end_through_api_and_cell_store() {
    let mut documents = parameter_entity_documents();
    documents.push(parameter_action_document());
    let store = admitted_store(&documents, crate::action_admission::prepare_action_model);
    let host = host_of(store);
    let query = scoped("org_demo");
    seed_parameter_settings(&host);

    // June 30 call succeeds with 0.10 and pins vat-june version 1 in norn.ActionCall.
    let june_body = json!({
        "input": {"id": "rec-june", "registry_id": "reg-main", "tax_rate": "0.10", "effective_at": "2026-06-30T23:59:59Z"},
        "idempotencyKey": "k-june"
    })
    .to_string();
    let june_receipt = call(
        &host,
        &query,
        Some(OPERATOR),
        "billing.tax_receipt.record",
        &june_body,
    )
    .unwrap();
    assert_eq!(june_receipt.outcome, ActionCallOutcome::Committed);

    let stored_record = {
        let cell = host.cell().unwrap();
        let row = cell
            .store()
            .read()
            .object(
                MAIN_WORLD,
                ACTION_CALL_TYPE,
                "billing.tax_receipt.record/k-june",
            )
            .unwrap()
            .expect("action call record");
        ActionCallRecord::from_values(&row.values).expect("decode action call record")
    };
    assert_eq!(stored_record.parameter_pins.len(), 1);
    assert_eq!(stored_record.parameter_pins[0].setting.key, "vat-june");
    assert_eq!(stored_record.parameter_pins[0].version, 1);

    // Missing window (May 1) refuses 422 core.composition.parameter_missing without mutating state.
    let before_missing = dump(&host);
    let missing_body = json!({
        "input": {"id": "rec-may", "registry_id": "reg-main", "tax_rate": "0.10", "effective_at": "2026-05-01T00:00:00Z"},
        "idempotencyKey": "k-may"
    })
    .to_string();
    let missing = call(
        &host,
        &query,
        Some(OPERATOR),
        "billing.tax_receipt.record",
        &missing_body,
    )
    .unwrap_err();
    assert_eq!(
        (missing.status, missing.code.as_str()),
        (422, "core.composition.parameter_missing")
    );
    assert_eq!(dump(&host), before_missing);

    // Unauthorized caller gets opaque 403 authz.denied (fail closed).
    let hidden = call(
        &host,
        &query,
        Some(HIDDEN),
        "billing.tax_receipt.record",
        &missing_body,
    )
    .unwrap_err();
    assert_eq!(
        (hidden.status, hidden.code.as_str()),
        (403, codes::AUTHZ_DENIED)
    );
}

/// OPEN-CHECKPOINT-1 (#1551 round 5, Blockers 3, 6, 10):
/// - An unchanged-target non-quantity action committing during an active
///   history scrub succeeds cleanly with the target's live version instead of
///   committing and then returning retryable `cell.verifying` (Blocker 3).
/// - Kernel commit paths and `CommitError::Host` preserve typed `503
///   cell.verifying` (retryable) and `503 cell.fenced` (fail-closed) instead
///   of downgrading to `500 commit.host` / `500 host.error` (Blocker 10).
/// - `timers::run_due` + `timers::next_due` on a fenced cell return terminal
///   failure and `Ok(None)` without throwing `cell.fenced` (Blocker 6).
fn order_edit_body(id: &str, values: &str, version: u64, key: &str) -> String {
    format!(
        r#"{{"input":{{"parentId":"{id}","parentValues":{values},"parentVersion":"{version}","rowOps":[]}},"idempotencyKey":"{key}"}}"#
    )
}

fn seed_checkpointed_rows_cell() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    norn_cell::Model,
    norn_cell::StoreOptions,
    ActionReceipt,
) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rows.sqlite");
    let options = norn_cell::StoreOptions {
        checkpoint_key: Some(norn_cell::CheckpointKey::from_bytes([0x5a; 32])),
        ..norn_cell::StoreOptions::default()
    };
    let docs = rows_documents();
    let engine = NativeEngine::open(&path, options).unwrap();
    let store = action_cell::admitted_store_on(
        engine,
        &docs,
        crate::action_admission::prepare_action_model,
    );
    let model = store.model().clone();
    let host = host_of(store);
    let query = scoped("org_demo");
    let created = call(
        &host,
        &query,
        Some(OPERATOR),
        ORDER_EDIT,
        &order_edit_body("order-1", r#"{"title":"first"}"#, 0, "create-order-1"),
    )
    .unwrap();
    for n in 0..130_u64 {
        let id = format!("filler-{n}");
        call(
            &host,
            &query,
            Some(OPERATOR),
            ORDER_EDIT,
            &order_edit_body(&id, r#"{"title":"fill"}"#, 0, &format!("fill-{n}")),
        )
        .unwrap();
    }
    drop(host);
    (dir, path, model, options, created)
}

#[test]
fn open_checkpoint_refusal_mappings_preserve_verifying_and_fenced() {
    use norn_cell::CellError;
    use norn_kernel::{ActionRefusal, CommitError, DecisionRefusal};

    let verifying_action = Refusal::from_action_host_error(&ActionRefusal::Decision(
        DecisionRefusal::Host(CellError::Verifying("idempotency")),
    ))
    .expect("verifying action refusal");
    assert_eq!(
        (
            verifying_action.status,
            verifying_action.code.as_str(),
            verifying_action.retryable
        ),
        (503, codes::CELL_VERIFYING, true)
    );
    let fenced_action = Refusal::from_action_host_error(&ActionRefusal::Decision(
        DecisionRefusal::Host(CellError::Fenced("tampered".to_owned())),
    ))
    .expect("fenced action refusal");
    assert_eq!(
        (
            fenced_action.status,
            fenced_action.code.as_str(),
            fenced_action.retryable
        ),
        (503, codes::CELL_FENCED, false)
    );
    let verifying_commit =
        Refusal::from_commit_host_error(&CommitError::Host(CellError::Verifying("idempotency")));
    assert_eq!(
        (
            verifying_commit.status,
            verifying_commit.code.as_str(),
            verifying_commit.retryable
        ),
        (503, codes::CELL_VERIFYING, true)
    );
    let fenced_commit = Refusal::from_commit_host_error(&CommitError::Host(CellError::Fenced(
        "tampered".to_owned(),
    )));
    assert_eq!(
        (
            fenced_commit.status,
            fenced_commit.code.as_str(),
            fenced_commit.retryable
        ),
        (503, codes::CELL_FENCED, false)
    );
}

#[test]
fn open_checkpoint_scrub_actions_refusals_and_fenced_timers() {
    use norn_cell::ScrubStatus;

    let (_dir, path, model, options, created) = seed_checkpointed_rows_cell();
    let query = scoped("org_demo");
    let operator = Some(OPERATOR);

    // Reopen from checkpoint: starts in Receipts phase (idempotency unproven).
    let reopened_store = CellStore::open(&path, model, options).unwrap();
    assert!(matches!(
        reopened_store.scrub_status(),
        ScrubStatus::Verifying {
            phase: "receipts",
            history_ready: false,
            ..
        }
    ));
    let host = host_of(reopened_store);

    // Blocker 10 end-to-end: during Receipts phase, an idempotency-keyed kernel
    // commit hits CommitError::Host(CellError::Verifying) -> 503 cell.verifying.
    {
        let mut cell = host.cell().unwrap();
        let err = norn_kernel::CellHost::commit_action_quantity(
            &mut *cell,
            norn_kernel::CellCommit::empty(),
            &[],
            "unproven-receipt-key",
        )
        .unwrap_err();
        let mapped = Refusal::from_commit_host_error(&err);
        assert_eq!(
            (mapped.status, mapped.code.as_str(), mapped.retryable),
            (503, codes::CELL_VERIFYING, true)
        );
    }

    // Advance scrub past Receipts phase into History phase.
    {
        let cell = host.cell().unwrap();
        loop {
            let status = cell.store().scrub_step();
            if matches!(
                status,
                ScrubStatus::Verifying {
                    phase: "history",
                    history_ready: false,
                    ..
                }
            ) {
                break;
            }
            assert!(
                matches!(
                    status,
                    ScrubStatus::Verifying {
                        phase: "receipts",
                        ..
                    }
                ),
                "expected history phase, got {status:?}"
            );
        }
    }

    // Blocker 3: unchanged-target non-quantity action commits during History scrub.
    let noop_body = order_edit_body("order-1", "{}", created.version, "noop-during-history");
    let noop = call(&host, &query, operator, ORDER_EDIT, &noop_body)
        .expect("unchanged-target action must not commit and return cell.verifying");
    assert_eq!(noop.outcome, ActionCallOutcome::Committed);
    assert_eq!(
        (noop.object_id.as_str(), noop.version),
        ("order-1", created.version)
    );

    // Replay during History scrub preserves S5b and refuses 503 cell.verifying.
    let replay_err = call(&host, &query, operator, ORDER_EDIT, &noop_body).unwrap_err();
    assert_eq!(
        (
            replay_err.status,
            replay_err.code.as_str(),
            replay_err.retryable
        ),
        (503, codes::CELL_VERIFYING, true)
    );
    assert_eq!(
        host.cell().unwrap().store().scrub_to_end(),
        ScrubStatus::Idle
    );
    let replayed = call(&host, &query, operator, ORDER_EDIT, &noop_body).unwrap();
    assert_eq!(replayed.outcome, ActionCallOutcome::Replayed);
    assert_eq!(
        (replayed.object_id.as_str(), replayed.version),
        ("order-1", created.version)
    );
}

#[test]
fn open_checkpoint_fenced_timers_and_action_refusal() {
    use norn_cell::StoreOptions;

    let (_dir, path, model, options, created) = seed_checkpointed_rows_cell();
    let query = scoped("org_demo");
    let operator = Some(OPERATOR);

    // Blocker 6 + Blocker 10 (fenced): tamper covered fact -> fenced alarm cleanup.
    {
        use norn_cell::engine::SqlEngine as _;
        let raw = NativeEngine::open(&path, StoreOptions::default()).unwrap();
        raw.execute("DELETE FROM _norn_fact_values WHERE commit_seq = 5", &[])
            .unwrap();
    }
    let fenced_host = host_of(CellStore::open(&path, model, options).unwrap());
    let now = LogicalTime::from_millis(1_800_000_000_000);
    let fenced_work = loop {
        let work = crate::timers::run_due(&fenced_host, now).expect("run_due must not throw");
        if work.failed.iter().any(|msg| msg.contains("fenced")) {
            break work;
        }
        assert!(work.next.is_some());
    };
    assert_eq!(fenced_work.next, None);
    let second = crate::timers::run_due(&fenced_host, now).expect("repeat run_due must not throw");
    assert_eq!(second.next, None);
    assert!(second.failed.iter().any(|msg| msg.contains("fenced")));
    assert_eq!(
        crate::timers::next_due(&fenced_host).expect("next_due on fenced cell must not throw"),
        None
    );
    let fenced_call = call(
        &fenced_host,
        &query,
        operator,
        ORDER_EDIT,
        &order_edit_body("order-1", "{}", created.version, "call-when-fenced"),
    )
    .unwrap_err();
    assert_eq!(
        (
            fenced_call.status,
            fenced_call.code.as_str(),
            fenced_call.retryable
        ),
        (503, codes::CELL_FENCED, false)
    );
}

struct FenceCommitFaultEngine {
    inner: NativeEngine,
    armed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    fired: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl norn_cell::engine::SqlEngine for FenceCommitFaultEngine {
    fn read_isolation(&self) -> norn_cell::engine::ReadIsolation {
        self.inner.read_isolation()
    }
    fn checkpoint_key(&self) -> Option<norn_cell::CheckpointKey> {
        self.inner.checkpoint_key()
    }
    fn external_data_version(&self) -> Result<Option<u64>, norn_cell::CellError> {
        self.inner.external_data_version()
    }
    fn definition_writes<T>(
        &self,
        f: impl FnOnce() -> Result<T, norn_cell::CellError>,
    ) -> Result<T, norn_cell::CellError> {
        self.inner.definition_writes(f)
    }
    fn execute(
        &self,
        sql: &str,
        params: &[norn_cell::engine::SqlValue],
    ) -> Result<(), norn_cell::CellError> {
        let is_fence_write = params.iter().any(|value| {
            matches!(value, norn_cell::engine::SqlValue::Text(text) if text == "open_checkpoint:fenced")
        });
        if self.armed.load(std::sync::atomic::Ordering::Acquire) && is_fence_write {
            self.armed
                .store(false, std::sync::atomic::Ordering::Release);
            self.fired.store(true, std::sync::atomic::Ordering::Release);
            return Err(norn_cell::CellError::Engine(
                "transient fence persistence fault".to_owned(),
            ));
        }
        self.inner.execute(sql, params)
    }
    fn query(
        &self,
        sql: &str,
        params: &[norn_cell::engine::SqlValue],
    ) -> Result<Vec<Vec<norn_cell::engine::SqlValue>>, norn_cell::CellError> {
        self.inner.query(sql, params)
    }
    fn execute_batch(&self, sql: &str) -> Result<(), norn_cell::CellError> {
        self.inner.execute_batch(sql)
    }
    fn transaction<T, X: From<norn_cell::CellError>>(
        &self,
        f: impl FnOnce() -> Result<T, X>,
    ) -> Result<T, X> {
        self.inner.transaction(f)
    }
}

/// Blocker 1 (S12, #1551 round 6): when durable fence persistence fails
/// transiently during `run_due`, `run_due` and `next_due` MUST keep the alarm
/// armed and retry persistence on the next wake until durable, only then
/// returning `None`.
#[test]
fn open_checkpoint_failed_fence_write_keeps_alarm_armed_until_persisted() {
    use norn_cell::engine::SqlEngine as _;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let (_dir, path, model, options, _created) = seed_checkpointed_rows_cell();
    {
        let raw = NativeEngine::open(&path, norn_cell::StoreOptions::default()).unwrap();
        raw.execute("DELETE FROM _norn_fact_values WHERE commit_seq = 5", &[])
            .unwrap();
    }

    let armed = Arc::new(AtomicBool::new(true));
    let fired = Arc::new(AtomicBool::new(false));
    let store = CellStore::from_engine(
        FenceCommitFaultEngine {
            inner: NativeEngine::open(&path, options).unwrap(),
            armed: Arc::clone(&armed),
            fired: Arc::clone(&fired),
        },
        model.clone(),
    )
    .unwrap();
    let scope = CellScope {
        org_id: "org_demo".into(),
        environment_id: "dev".into(),
    };
    let host = DevHost::from_store(store, scope, |cmd| cmd.payload.clone())
        .unwrap()
        .with_authorizer(Policy);

    let now = LogicalTime::from_millis(1_800_000_000_000);
    let pending_work = loop {
        let work = crate::timers::run_due(&host, now).expect("run_due must not throw");
        if work.failed.iter().any(|msg| msg.contains("fenced")) {
            break work;
        }
        assert!(work.next.is_some());
    };
    assert!(fired.load(Ordering::Acquire), "fence write fault must fire");
    assert!(
        crate::timers::is_fence_persist_pending(&host),
        "fence must still await durable persistence"
    );
    assert!(
        pending_work.next.is_some(),
        "failed fence write must keep run_due.next armed for alarm retry"
    );
    assert!(
        crate::timers::next_due(&host)
            .expect("next_due must not throw")
            .is_some(),
        "failed fence write must keep next_due armed (not None)"
    );

    // Next alarm wake retries durable persistence and succeeds -> alarm clears!
    let persisted_work = crate::timers::run_due(&host, now).expect("retry run_due must not throw");
    assert!(!crate::timers::is_fence_persist_pending(&host));
    assert_eq!(
        persisted_work.next, None,
        "persisted fence clears run_due.next"
    );
    assert_eq!(
        crate::timers::next_due(&host).expect("next_due after durable fence"),
        None,
        "persisted fence clears next_due alarm"
    );
    drop(host);
    assert!(
        matches!(
            CellStore::open(&path, model, options),
            Err(norn_cell::CellError::Fenced(_))
        ),
        "reopen fails closed once alarm retry persisted the durable fence"
    );
}

struct SiblingUpdateAfterCommitEngine {
    inner: NativeEngine,
    path: std::path::PathBuf,
    model: norn_cell::Model,
    options: norn_cell::StoreOptions,
    armed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    raced: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl norn_cell::engine::SqlEngine for SiblingUpdateAfterCommitEngine {
    fn read_isolation(&self) -> norn_cell::engine::ReadIsolation {
        self.inner.read_isolation()
    }
    fn checkpoint_key(&self) -> Option<norn_cell::CheckpointKey> {
        self.inner.checkpoint_key()
    }
    fn external_data_version(&self) -> Result<Option<u64>, norn_cell::CellError> {
        self.inner.external_data_version()
    }
    fn definition_writes<T>(
        &self,
        f: impl FnOnce() -> Result<T, norn_cell::CellError>,
    ) -> Result<T, norn_cell::CellError> {
        self.inner.definition_writes(f)
    }
    fn execute(
        &self,
        sql: &str,
        params: &[norn_cell::engine::SqlValue],
    ) -> Result<(), norn_cell::CellError> {
        self.inner.execute(sql, params)
    }
    fn query(
        &self,
        sql: &str,
        params: &[norn_cell::engine::SqlValue],
    ) -> Result<Vec<Vec<norn_cell::engine::SqlValue>>, norn_cell::CellError> {
        self.inner.query(sql, params)
    }
    fn execute_batch(&self, sql: &str) -> Result<(), norn_cell::CellError> {
        self.inner.execute_batch(sql)
    }
    fn transaction<T, X: From<norn_cell::CellError>>(
        &self,
        f: impl FnOnce() -> Result<T, X>,
    ) -> Result<T, X> {
        let out = self.inner.transaction(f)?;
        if self.armed.swap(false, std::sync::atomic::Ordering::AcqRel) {
            let sibling_store = CellStore::open(&self.path, self.model.clone(), self.options)?;
            let sibling_host = host_of(sibling_store);
            let query = scoped("org_demo");
            call(
                &sibling_host,
                &query,
                Some(OPERATOR),
                ORDER_EDIT,
                &order_edit_body(
                    "order-1",
                    r#"{"title":"sibling-updated"}"#,
                    1,
                    "sibling-upd",
                ),
            )
            .map_err(|err| norn_cell::CellError::Engine(format!("{err:?}")))?;
            self.raced.store(true, std::sync::atomic::Ordering::Release);
        }
        Ok(out)
    }
}

/// Blocker 5 (S7/S5b, #1551 round 6): fresh and replayed responses for an
/// untouched-target action MUST report the exact same target version at
/// `commit_seq` even if a sibling updates the target immediately after commit.
#[test]
fn unchanged_target_fresh_and_replayed_receipts_agree_across_sibling_update() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let (_dir, path, model, options, created) = seed_checkpointed_rows_cell();
    let armed = Arc::new(AtomicBool::new(false));
    let raced = Arc::new(AtomicBool::new(false));
    let store = CellStore::from_engine(
        SiblingUpdateAfterCommitEngine {
            inner: NativeEngine::open(&path, options).unwrap(),
            path,
            model: model.clone(),
            options,
            armed: Arc::clone(&armed),
            raced: Arc::clone(&raced),
        },
        model,
    )
    .unwrap();
    let scope = CellScope {
        org_id: "org_demo".into(),
        environment_id: "dev".into(),
    };
    let host = DevHost::from_store(store, scope, |cmd| cmd.payload.clone())
        .unwrap()
        .with_authorizer(Policy);
    // Complete scrub first so replay `version_as_of` succeeds immediately after.
    assert_eq!(
        host.cell().unwrap().store().scrub_to_end(),
        norn_cell::ScrubStatus::Idle
    );

    let query = scoped("org_demo");
    let now = LogicalTime::from_millis(1_700_000_000_000);
    let noop_req = decode_action_request(&order_edit_body(
        "order-1",
        "{}",
        created.version,
        "noop-sibling",
    ))
    .unwrap();
    armed.store(true, Ordering::Release);
    let fresh = match call_action(
        &host,
        &query,
        Some(OPERATOR),
        ORDER_EDIT,
        &noop_req,
        &Disclosure,
        now,
    )
    .unwrap()
    {
        ActionAnswer::Receipt(receipt) => receipt,
        ActionAnswer::Pending(pending) => panic!("unexpected pending: {pending:?}"),
    };
    assert!(
        raced.load(Ordering::Acquire),
        "sibling update must race after commit"
    );
    let live_version = host
        .cell()
        .unwrap()
        .store()
        .read()
        .version(norn_cell::MAIN_WORLD, "test.Order", "order-1")
        .unwrap();
    assert_eq!(live_version, created.version.saturating_add(1));

    let replayed = match call_action(
        &host,
        &query,
        Some(OPERATOR),
        ORDER_EDIT,
        &noop_req,
        &Disclosure,
        now,
    )
    .unwrap()
    {
        ActionAnswer::Receipt(receipt) => receipt,
        ActionAnswer::Pending(pending) => panic!("unexpected pending: {pending:?}"),
    };
    assert_eq!(fresh.outcome, ActionCallOutcome::Committed);
    assert_eq!(replayed.outcome, ActionCallOutcome::Replayed);
    assert_eq!(
        fresh.version, created.version,
        "fresh receipt must report exact CAS-verified version at commit_seq, not sibling's newer live version"
    );
    assert_eq!(
        fresh.version, replayed.version,
        "fresh and replayed responses must agree on untouched target version"
    );
}

fn subtype_noop_documents() -> Vec<Value> {
    let base_order = entity(
        "test.BaseOrder.entity",
        &json!({
            "name": "test.BaseOrder",
            "properties": [
                { "id": "order_id", "type": { "kind": "text" } },
                { "id": "title", "type": { "kind": "text" } }
            ]
        }),
        &[],
    );
    let special_order = entity(
        "test.SpecialOrder.entity",
        &json!({
            "name": "test.SpecialOrder",
            "extends": "test.BaseOrder",
            "properties": [
                { "id": "reviewer", "type": { "kind": "text" }, "constraints": { "nullable": true } }
            ]
        }),
        &["test.BaseOrder.entity"],
    );
    let schema = json!({
        "type": "object",
        "properties": {
            "order_id": { "type": "string" },
            "title": { "type": "string" }
        },
        "required": ["order_id", "title"],
        "additionalProperties": false
    });
    let upsert_special = action_artifact(
        "test.SpecialOrder.upsert.action",
        "test.special_order.upsert",
        &schema,
        &json!([
            { "entityKind": "test.SpecialOrder", "change": "created" },
            { "entityKind": "test.SpecialOrder", "change": "modified" }
        ]),
        &json!({
            "kind": "upsert_input_object",
            "objectIdField": "order_id",
            "objectKind": "test.SpecialOrder",
            "entityArtifactId": "test.SpecialOrder.entity",
            "entityArtifactVersion": "1.0.0",
            "writeMode": { "kind": "upsert" }
        }),
        &["test.SpecialOrder.entity"],
    );
    let noop_ancestor = envelope(
        "test.BaseOrder.noop_ancestor.action",
        "action",
        &json!({
            "actionId": "test.base_order.noop_ancestor",
            "scopeRequired": ["orgId", "environmentId"],
            "idempotent": true,
            "ledgered": true,
            "inputSchema": schema,
            "outputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "slice": [{
                "name": "special",
                "objectType": "test.SpecialOrder",
                "key": { "property": { "from": "input", "property": "order_id" } }
            }],
            "preconditions": ["slice.special.title == \"ready\""],
            "writes": [
                { "entityKind": "test.BaseOrder", "change": "created" },
                { "entityKind": "test.BaseOrder", "change": "modified" }
            ],
            "effectProgram": {
                "kind": "upsert_input_object",
                "objectIdField": "order_id",
                "objectKind": "test.BaseOrder",
                "entityArtifactId": "test.BaseOrder.entity",
                "entityArtifactVersion": "1.0.0",
                "writeMode": { "kind": "upsert" }
            }
        }),
        &["test.BaseOrder.entity", "test.SpecialOrder.entity"],
    );
    vec![base_order, special_order, upsert_special, noop_ancestor]
}

/// S7/S5b (#1551 round 8): `norn-api` fresh and replayed responses agree on the
/// exact target version when an ancestor-addressed no-op action reads a
/// concrete subtype through a declared slice entry.
#[test]
fn ancestor_addressed_noop_and_replay_report_subtype_target_version() {
    let store = admitted_store(
        &subtype_noop_documents(),
        crate::action_admission::prepare_action_model,
    );
    let host = host_of(store);
    let query = scoped("org_demo");

    // Seed concrete subtype at version 1, then bump to version 2.
    let v1 = call(
        &host,
        &query,
        Some(OPERATOR),
        "test.special_order.upsert",
        r#"{"input":{"order_id":"ord-1","title":"draft"},"idempotencyKey":"seed-1"}"#,
    )
    .unwrap();
    assert_eq!(v1.version, 1);
    let v2 = call(
        &host,
        &query,
        Some(OPERATOR),
        "test.special_order.upsert",
        r#"{"input":{"order_id":"ord-1","title":"ready"},"idempotencyKey":"seed-2"}"#,
    )
    .unwrap();
    assert_eq!(v2.version, 2);

    // Ancestor-addressed no-op with subtype slice read: fresh reports v2.
    let noop_body =
        r#"{"input":{"order_id":"ord-1","title":"ready"},"idempotencyKey":"noop-ancestor-1"}"#;
    let fresh = call(
        &host,
        &query,
        Some(OPERATOR),
        "test.base_order.noop_ancestor",
        noop_body,
    )
    .unwrap();
    assert_eq!(fresh.outcome, ActionCallOutcome::Committed);
    assert_eq!(fresh.version, 2);

    // Mutate concrete subtype to v3 AFTER the no-op commit so live version != 2.
    let v3 = call(
        &host,
        &query,
        Some(OPERATOR),
        "test.special_order.upsert",
        r#"{"input":{"order_id":"ord-1","title":"post-noop"},"idempotencyKey":"seed-3"}"#,
    )
    .unwrap();
    assert_eq!(v3.version, 3);

    // Replay must still report historical version 2 (not live version 3).
    let replayed = call(
        &host,
        &query,
        Some(OPERATOR),
        "test.base_order.noop_ancestor",
        noop_body,
    )
    .unwrap();
    assert_eq!(replayed.outcome, ActionCallOutcome::Replayed);
    assert_eq!(replayed.version, 2);
}
