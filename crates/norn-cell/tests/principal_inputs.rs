#![cfg(feature = "native")]
//! PRINCIPAL-INPUT-1 (#1091): a synthetic approval decision that requires a
//! role under a scope, through the production `SQLite` host, the kernel
//! commit path and the kernel PDP.
//!
//! An expense is approved by an action whose pinned decision declares three
//! principal inputs and writes `approver_confirmed = principal.isApprover`;
//! the expense's own invariant refuses an approval that is not confirmed.
//! The host seam ([`Directory`]) resolves the inputs from the principal it
//! verified and the PDP, never from the call.

use std::cell::{Cell as Counter, RefCell};
use std::collections::BTreeMap;
use std::path::Path;

use norn_cell::{Model, SqliteCell, StoreOptions};
use norn_ir::access::{AccessIr, PrincipalAttributeCatalog};
use norn_ir::{ActionRoot, ObjectTypeDef, RuleRoot, RuntimeValue};
use norn_kernel::authz::{
    ActorKind, AttrValue, CapabilityGrant, Delegation, HOLDS_ROLE_ACTION, HoldsRolePolicy,
    PrincipalContext, PrincipalInput, ScopeKey, holds_role_keys, principal_attribute_catalog,
    resolve_principal_inputs, scope_dimensions,
};
use norn_kernel::cell_host::JsonObject;
use norn_kernel::{
    ACTION_CALL_TYPE, ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionCallRecord,
    ActionDenial, ActionOutcome, ActionPrincipalInputs, CellCommit, CellHost, Command,
    DecisionTurn, FactEvent, FactKey, FactMutation, FactVersion, FactWrite, LogicalTime,
    PrincipalInputsRequest, SynchronousRule, TypeCatalog, UnresolvedPrincipalInput, World,
    commit_action,
};
use serde_json::{Value, json};

type Cell = SqliteCell<FactEvent, JsonObject, JsonObject>;
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const EXPENSE: &str = "expense.Expense";
/// An object only a holder of the approver role may file (#1760).
const ADVANCE: &str = "expense.Advance";
const UNAVAILABLE: &str = "decision.principal_input_unavailable";

fn types() -> TestResult<Vec<ObjectTypeDef>> {
    let expense = json!({
        "name": EXPENSE,
        "properties": [
            {"id": "expense_id", "type": {"kind": "text"}},
            {"id": "approver_confirmed", "type": {"kind": "boolean"}, "default": false},
            {"id": "status", "type": {"kind": "enum", "values": [
                {"value": "draft"}, {"value": "approved"}
            ]}, "default": "draft"}
        ],
        "lifecycles": [{
            "property": "status", "initial": ["draft"], "terminal": ["approved"],
            "transitions": [{"id": "approve", "from": ["draft"], "to": "approved"}]
        }],
        "invariants": [{
            "id": "approved_by_approver",
            "expression": "self.status != \"approved\" or self.approver_confirmed",
            "message": "only a holder of the approver role approves"
        }]
    });
    let nullable = |id: &str, kind: &str| json!({"id": id, "type": {"kind": kind}, "constraints": {"nullable": true}});
    let mut fields: Vec<Value> = [
        "actionId",
        "actor",
        "turn",
        "transition",
        "from",
        "to",
        "actionArtifactId",
        "actionVersion",
        "effectKind",
        "ruleBinding",
    ]
    .iter()
    .map(|id| nullable(id, "text"))
    .collect();
    fields.extend(
        [
            "object",
            "input",
            "effectiveInput",
            "derivedValues",
            "preconditionOutcomes",
            "principalInputs",
            "rowOperations",
        ]
        .iter()
        .map(|id| nullable(id, "json-ext")),
    );
    let advance = json!({
        "name": ADVANCE,
        "properties": [
            {"id": "advance_id", "type": {"kind": "text"}},
            {"id": "issuer_confirmed", "type": {"kind": "boolean"}, "default": false}
        ],
        "invariants": [{
            "id": "filed_by_approver",
            "expression": "self.issuer_confirmed",
            "message": "only a holder of the approver role files an advance"
        }]
    });
    Ok(vec![
        serde_json::from_value(expense)?,
        serde_json::from_value(advance)?,
        serde_json::from_value(json!({"name": ACTION_CALL_TYPE, "properties": fields}))?,
    ])
}

fn empty_list() -> Value {
    json!({"type": "array", "items": {"type": "object", "additionalProperties": false, "properties": {}}})
}

fn rule_root() -> TestResult<RuleRoot> {
    decide_root(
        "expense.approval.decide",
        EXPENSE,
        "expense_id",
        "approver_confirmed",
    )
}

/// A `decide` rule that declares three principal inputs and sets `property`
/// of the `object_type` object named by the input's `id_field` to
/// `principal.isApprover`.
fn decide_root(
    rule_id: &str,
    object_type: &str,
    id_field: &str,
    property: &str,
) -> TestResult<RuleRoot> {
    let facts = format!(
        "[{{op: 'set', objectType: '{object_type}', key: event.input.{id_field}, \
         property: '{property}', value: principal.isApprover}}]"
    );
    Ok(serde_json::from_value(json!({
        "ruleVersion": "2.0",
        "ruleId": rule_id,
        "contract": "decide",
        "scopeRequired": ["orgId", "environmentId"],
        "principalInputs": [
            {"name": "isApprover", "kind": {"holdsRole": {"role": "approver", "at": "resource"}}},
            {"name": "financeRoles", "kind": {"roleProfile": {"profile": "finance"}}},
            {"name": "actorId", "kind": {"attribute": {"name": "id"}}}
        ],
        "inputSchema": {
            "type": "object", "additionalProperties": false,
            "required": ["slice", "event", "principal"],
            "properties": {
                "slice": {"type": "object", "additionalProperties": false, "properties": {}},
                "event": {
                    "type": "object", "additionalProperties": false,
                    "required": ["input", "actor", "operationId"],
                    "properties": {
                        "input": {
                            "type": "object", "additionalProperties": false,
                            "required": [id_field],
                            "properties": {id_field: {"type": "string"}}
                        },
                        "actor": {"type": "string"},
                        "operationId": {"type": "string"}
                    }
                },
                "principal": {
                    "type": "object", "additionalProperties": false,
                    "required": ["isApprover", "financeRoles", "actorId"],
                    "properties": {
                        "isApprover": {"type": "boolean"},
                        "financeRoles": {"type": "array", "items": {"type": "string"}},
                        "actorId": {"type": "string"}
                    }
                }
            }
        },
        "outputSchema": {
            "type": "object", "additionalProperties": false,
            "required": ["facts", "commands", "timers"],
            "properties": {
                "commands": empty_list(),
                "timers": empty_list(),
                "facts": {"type": "array", "items": {
                    "type": "object", "additionalProperties": false,
                    "required": ["op", "objectType", "key", "property", "value"],
                    "properties": {
                        "op": {"type": "string", "enum": ["set"]},
                        "objectType": {"type": "string", "enum": [object_type]},
                        "key": {"type": "string"},
                        "property": {"type": "string", "enum": [property]},
                        "value": {"type": "boolean"}
                    }
                }}
            }
        },
        "graph": {
            "contentType": "application/vnd.gorules.decision",
            "nodes": [
                {"id": "in", "type": "inputNode", "name": "input"},
                {"id": "decision", "type": "decisionTableNode", "name": "decision", "content": {
                    "hitPolicy": "first",
                    "inputs": [{"id": "condition", "name": "condition", "type": "expression"}],
                    "outputs": [
                        {"field": "facts", "id": "out_facts", "name": "facts"},
                        {"field": "commands", "id": "out_commands", "name": "commands"},
                        {"field": "timers", "id": "out_timers", "name": "timers"}
                    ],
                    "rules": [{
                        "_id": "row_0",
                        "condition": "",
                        "out_facts": facts,
                        "out_commands": "[]",
                        "out_timers": "[]"
                    }]
                }},
                {"id": "out", "type": "outputNode", "name": "output"}
            ],
            "edges": [
                {"id": "edge_0", "sourceId": "in", "targetId": "decision", "type": "edge"},
                {"id": "edge_1", "sourceId": "decision", "targetId": "out", "type": "edge"}
            ]
        }
    }))?)
}

fn rule() -> TestResult<SynchronousRule> {
    admitted("expense.approval.decide.rule", &rule_root()?)
}

fn admitted(artifact_id: &str, root: &RuleRoot) -> TestResult<SynchronousRule> {
    Ok(SynchronousRule::admit(
        norn_ir::ActionRuleRef {
            artifact_id: artifact_id.into(),
            version: "1.0.0".into(),
        },
        root,
        &BTreeMap::new(),
        norn_kernel::DecisionBinding {
            profile_id: "norn-zen/7".into(),
            environment_hash: "sqlite-principal".into(),
        },
    )?)
}

/// CREATE-WRITER-1 × PRINCIPAL-INPUT-1 (#1760): the writer of a governed
/// create, declaring the same principal inputs as the approval decision.
fn advance_rule() -> TestResult<SynchronousRule> {
    admitted(
        "expense.advance.decide.rule",
        &decide_root(
            "expense.advance.decide",
            ADVANCE,
            "advance_id",
            "issuer_confirmed",
        )?,
    )
}

/// A create-only action whose writer is [`advance_rule`].
fn file_advance() -> TestResult<ActionRoot> {
    let record = json!({
        "type": "object", "additionalProperties": false,
        "required": ["advance_id"], "properties": {"advance_id": {"type": "string"}}
    });
    Ok(serde_json::from_value(json!({
        "actionId": "expense.advance.file",
        "title": "File an advance",
        "scopeRequired": ["orgId", "environmentId"],
        "idempotent": true,
        "ledgered": true,
        "inputSchema": record,
        "outputSchema": record,
        "synchronousRule": {"artifactId": "expense.advance.decide.rule", "version": "1.0.0"},
        "writes": [
            {"entityKind": ADVANCE, "change": "created"},
            {"entityKind": ADVANCE, "change": "modified"}
        ],
        "effectProgram": {
            "kind": "upsert_input_object",
            "entityArtifactId": "expense.Advance.entity",
            "entityArtifactVersion": "1.0.0",
            "objectKind": ADVANCE,
            "objectIdField": "advance_id",
            "writeMode": {"kind": "create_only"}
        }
    }))?)
}

fn action() -> TestResult<ActionRoot> {
    let record = json!({
        "type": "object", "additionalProperties": false,
        "required": ["expense_id"], "properties": {"expense_id": {"type": "string"}}
    });
    Ok(serde_json::from_value(json!({
        "actionId": "expense.approve",
        "title": "Approve an expense",
        "scopeRequired": ["orgId", "environmentId"],
        "idempotent": true,
        "ledgered": true,
        "inputSchema": record,
        "outputSchema": record,
        "synchronousRule": {"artifactId": "expense.approval.decide.rule", "version": "1.0.0"},
        "writes": [{"entityKind": EXPENSE, "change": "modified"}],
        "effectProgram": {
            "kind": "fire_transition",
            "entityArtifactId": "expense.Expense.entity",
            "entityArtifactVersion": "1.0.0",
            "objectKind": EXPENSE,
            "objectIdField": "expense_id",
            "lifecycle": "status",
            "transition": "approve"
        }
    }))?)
}

fn cell_scope() -> ScopeKey {
    ScopeKey {
        org: "org_demo".into(),
        tenant: None,
        workspace: None,
        environment: Some("dev".into()),
        business_unit: None,
    }
}

fn catalog() -> TestResult<PrincipalAttributeCatalog> {
    let access: AccessIr = serde_json::from_value(json!({
        "accessVersion": "0.1",
        "policyId": "expense.access",
        "rules": [],
        "principalCatalog": [
            {"name": "approver", "type": "bool", "source": {"kind": "role"}},
            {"name": "finance", "type": "stringSet",
             "source": {"kind": "roleProfile", "roles": ["approver", "auditor"]}},
            {"name": "id", "type": "string", "source": {"kind": "attribute"}}
        ]
    }))?;
    Ok(principal_attribute_catalog(&[access], &BTreeMap::new())?)
}

/// The host seam of this test: the principals it verified with their roles,
/// the catalog, and the `holdsRole` PDP. Roles and the policy version change
/// between calls to model a later grant change.
struct Directory {
    roles: RefCell<BTreeMap<String, Vec<String>>>,
    policy_version: RefCell<String>,
    catalog: PrincipalAttributeCatalog,
    holds: HoldsRolePolicy,
    /// Where the decision's subject lives.
    subject_scope: ScopeKey,
    resolutions: Counter<usize>,
}

impl Directory {
    fn new(roles: &[(&str, &[&str])]) -> TestResult<Self> {
        let catalog = catalog()?;
        let keys = holds_role_keys(
            &catalog,
            &rule_root()?.principal_inputs,
            &scope_dimensions(&cell_scope()),
        );
        Ok(Self {
            roles: RefCell::new(
                roles
                    .iter()
                    .map(|(subject, held)| {
                        (
                            (*subject).to_owned(),
                            held.iter().map(|role| (*role).to_owned()).collect(),
                        )
                    })
                    .collect(),
            ),
            policy_version: RefCell::new("grants/1".into()),
            holds: HoldsRolePolicy::compile(&keys)?,
            catalog,
            subject_scope: cell_scope(),
            resolutions: Counter::new(0),
        })
    }
}

fn unresolved(request: &PrincipalInputsRequest<'_>) -> UnresolvedPrincipalInput {
    UnresolvedPrincipalInput {
        input: request
            .inputs
            .first()
            .map_or_default(|input| input.name.clone()),
    }
}

impl ActionAuthorizer for Directory {
    fn authorize(&self, _: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
    fn authorize_read(
        &self,
        _: &norn_kernel::ActionReadAuthzRequest<'_>,
    ) -> Result<(), ActionDenial> {
        Ok(())
    }
    fn authorize_read_type(
        &self,
        _: &norn_kernel::ActionReadTypeAuthzRequest<'_>,
    ) -> Result<(), ActionDenial> {
        Ok(())
    }
    fn resolve_principal_inputs(
        &self,
        request: &PrincipalInputsRequest<'_>,
    ) -> Result<ActionPrincipalInputs, UnresolvedPrincipalInput> {
        self.resolutions
            .set(self.resolutions.get().saturating_add(1));
        let roles = self
            .roles
            .borrow()
            .get(request.actor)
            .cloned()
            .ok_or_else(|| unresolved(request))?;
        let scope = cell_scope();
        let principal = PrincipalInput {
            subject_id: request.actor.to_owned(),
            actor_kind: ActorKind::User,
            agent_session_id: None,
            roles,
            attributes: BTreeMap::from([(
                "id".to_owned(),
                AttrValue::String(request.actor.to_owned()),
            )]),
        };
        let pdp = self
            .holds
            .pdp(&[CapabilityGrant {
                subject_id: request.actor.to_owned(),
                actor_kind: ActorKind::User,
                agent_session_id: None,
                scope: Some(scope.clone()),
                capabilities: vec![HOLDS_ROLE_ACTION.to_owned()],
                binding: None,
            }])
            .map_err(|_uncompiled| unresolved(request))?;
        let context = PrincipalContext {
            principal: &principal,
            request_scope: &scope,
            subject_scope: &self.subject_scope,
            delegation: &Delegation::Direct,
        };
        let values = resolve_principal_inputs(&self.catalog, &pdp, &context, request.inputs)
            .map_err(|unavailable| UnresolvedPrincipalInput {
                input: unavailable.input,
            })?;
        Ok(ActionPrincipalInputs {
            principal: request.actor.to_owned(),
            policy_version: self.policy_version.borrow().clone(),
            subject: request.subject.clone(),
            values,
        })
    }
}

/// A host that answers with what `tamper` made of an honest answer.
struct Tampered<'a> {
    honest: &'a Directory,
    tamper: fn(&mut ActionPrincipalInputs),
}

impl ActionAuthorizer for Tampered<'_> {
    fn authorize(&self, request: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        self.honest.authorize(request)
    }
    fn resolve_principal_inputs(
        &self,
        request: &PrincipalInputsRequest<'_>,
    ) -> Result<ActionPrincipalInputs, UnresolvedPrincipalInput> {
        let mut resolved = self.honest.resolve_principal_inputs(request)?;
        (self.tamper)(&mut resolved);
        Ok(resolved)
    }
}

/// A host with no resolver: the trait's default.
struct NoResolver;

impl ActionAuthorizer for NoResolver {
    fn authorize(&self, _: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
}

fn call<'a>(action: &'a ActionRoot, actor: &str, key: &str) -> TestResult<ActionCall<'a>> {
    call_with(action, actor, key, json!({"expense_id": "EXP-1"}))
}

fn call_with<'a>(
    action: &'a ActionRoot,
    actor: &str,
    key: &str,
    input: Value,
) -> TestResult<ActionCall<'a>> {
    Ok(ActionCall {
        action,
        action_artifact_id: "expense.approve.action",
        action_version: "1.0.0",
        scope: BTreeMap::from([
            ("orgId".into(), "org_demo".into()),
            ("environmentId".into(), "dev".into()),
        ]),
        actor: actor.into(),
        input: serde_json::from_value(input)?,
        idempotency_key: key.into(),
        object_type: None,
        turn: DecisionTurn {
            id: key.into(),
            now: LogicalTime::from_millis(0),
            world: World::Main,
            consumes: None,
        },
        trace: None,
    })
}

fn open(path: &Path, definitions: &[ObjectTypeDef]) -> TestResult<Cell> {
    let mut cell = Cell::open(
        path,
        Model::from_definitions(definitions, &[])?,
        StoreOptions::default(),
        "org_demo/dev",
        // The approval enqueues no command.
        |_: &Command<JsonObject>| JsonObject::new(),
    )?;
    let expense = FactKey::new(EXPENSE, "EXP-1");
    if !cell.load(&expense)?.exists() {
        let mut proposal = CellCommit::empty();
        proposal.writes.push(FactWrite {
            key: expense,
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: serde_json::from_value(json!({
                    "expense_id": "EXP-1", "status": "draft", "approver_confirmed": false
                }))?,
            },
        });
        cell.commit(proposal)?;
    }
    Ok(cell)
}

fn status(cell: &Cell) -> TestResult<RuntimeValue> {
    let expense = cell.load(&FactKey::new(EXPENSE, "EXP-1"))?;
    Ok(expense
        .values
        .and_then(|values| values.get("status").cloned())
        .ok_or("the expense has no status")?)
}

fn stored_record(cell: &Cell, key: &str) -> TestResult<Option<ActionCallRecord>> {
    stored_call(cell, "expense.approve", key)
}

fn stored_call(cell: &Cell, action_id: &str, key: &str) -> TestResult<Option<ActionCallRecord>> {
    let stored = cell.load(&FactKey::new(
        ACTION_CALL_TYPE,
        format!("{action_id}/{key}"),
    ))?;
    Ok(stored
        .values
        .map(|values| ActionCallRecord::from_values(&values).ok_or("an undecodable call record"))
        .transpose()?)
}

fn text(value: &str) -> RuntimeValue {
    RuntimeValue::String(value.to_owned())
}

#[test]
fn an_approver_holding_the_role_under_the_scope_approves_and_one_without_it_is_refused()
-> TestResult {
    let directory = tempfile::tempdir()?;
    let definitions = types()?;
    let rules = [rule()?];
    let catalog = TypeCatalog::new(&definitions).with_action_rules(&rules);
    let action = action()?;
    let host = Directory::new(&[("user:alice", &["approver"]), ("user:bob", &["member"])])?;
    let mut cell = open(&directory.path().join("approval.sqlite"), &definitions)?;

    // Bob holds no approver role: the decision confirms nothing and the
    // expense's own invariant refuses the approval. Nothing is committed.
    let refused = commit_action(
        &mut cell,
        &catalog,
        &call(&action, "user:bob", "bob-1")?,
        &host,
    )
    .expect_err("a non-approver is refused");
    assert!(refused.code().ends_with("invariant_violation"), "{refused}");
    assert_eq!(status(&cell)?, text("draft"));
    assert!(stored_record(&cell, "bob-1")?.is_none());

    // Alice holds it in the expense's scope: the approval commits and the
    // receipt records exactly the declared inputs, her own values, the
    // policy version and the subject.
    let outcome = commit_action(
        &mut cell,
        &catalog,
        &call(&action, "user:alice", "alice-1")?,
        &host,
    )?;
    let ActionOutcome::Committed { record, .. } = outcome else {
        panic!("the first call commits");
    };
    assert_eq!(status(&cell)?, text("approved"));
    let recorded = record
        .principal_inputs
        .ok_or("the record has no principal inputs")?;
    assert_eq!(recorded.principal, "user:alice");
    assert_eq!(recorded.policy_version, "grants/1");
    assert_eq!(recorded.subject, FactKey::new(EXPENSE, "EXP-1"));
    assert_eq!(
        recorded.values,
        JsonObject::from([
            ("actorId".to_owned(), text("user:alice")),
            (
                "financeRoles".to_owned(),
                RuntimeValue::Array(vec![text("approver")])
            ),
            ("isApprover".to_owned(), RuntimeValue::Bool(true)),
        ])
    );
    Ok(())
}

#[test]
fn a_role_held_in_another_scope_than_the_subjects_does_not_approve() -> TestResult {
    let directory = tempfile::tempdir()?;
    let definitions = types()?;
    let rules = [rule()?];
    let catalog = TypeCatalog::new(&definitions).with_action_rules(&rules);
    let action = action()?;
    let mut host = Directory::new(&[("user:alice", &["approver"])])?;
    host.subject_scope.org = "org_other".into();
    let mut cell = open(&directory.path().join("scope.sqlite"), &definitions)?;

    let refused = commit_action(
        &mut cell,
        &catalog,
        &call(&action, "user:alice", "alice-1")?,
        &host,
    )
    .expect_err("the role is not held at the subject's scope");
    assert!(refused.code().ends_with("invariant_violation"), "{refused}");
    assert_eq!(status(&cell)?, text("draft"));
    Ok(())
}

#[test]
fn replay_reads_the_receipt_and_never_resolves_again_after_grants_change() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("replay.sqlite");
    let definitions = types()?;
    let rules = [rule()?];
    let catalog = TypeCatalog::new(&definitions).with_action_rules(&rules);
    let action = action()?;
    let host = Directory::new(&[("user:alice", &["approver"])])?;
    let request = call(&action, "user:alice", "alice-1")?;
    let mut cell = open(&path, &definitions)?;

    let ActionOutcome::Committed { record, .. } =
        commit_action(&mut cell, &catalog, &request, &host)?
    else {
        panic!("the first call commits");
    };
    assert_eq!(host.resolutions.get(), 1);

    // The grant is revoked and the policy moves on.
    host.roles.borrow_mut().clear();
    *host.policy_version.borrow_mut() = "grants/2".into();
    drop(cell);
    let mut cell = open(&path, &definitions)?;

    let ActionOutcome::Replayed { record: replayed } =
        commit_action(&mut cell, &catalog, &request, &host)?
    else {
        panic!("an exact retry replays");
    };
    assert_eq!(host.resolutions.get(), 1, "replay never resolves");
    assert_eq!(replayed, record);
    assert_eq!(stored_record(&cell, "alice-1")?, Some(record));
    let recorded = replayed
        .principal_inputs
        .ok_or("the replayed record has no principal inputs")?;
    assert_eq!(recorded.policy_version, "grants/1");
    assert_eq!(
        recorded.values.get("isApprover"),
        Some(&RuntimeValue::Bool(true))
    );
    assert_eq!(status(&cell)?, text("approved"));

    // A new call under the changed grants resolves afresh and is unavailable.
    let fresh = commit_action(
        &mut cell,
        &catalog,
        &call(&action, "user:alice", "alice-2")?,
        &host,
    )
    .expect_err("the revoked principal is no longer verified here");
    assert_eq!(fresh.code(), UNAVAILABLE);
    assert_eq!(host.resolutions.get(), 2);
    Ok(())
}

#[test]
fn an_input_that_cannot_be_resolved_fails_closed_and_names_no_value() -> TestResult {
    let directory = tempfile::tempdir()?;
    let definitions = types()?;
    let rules = [rule()?];
    let catalog = TypeCatalog::new(&definitions).with_action_rules(&rules);
    let action = action()?;
    let honest = Directory::new(&[("user:alice", &["approver"])])?;
    let mut cell = open(&directory.path().join("closed.sqlite"), &definitions)?;
    let tampers: [fn(&mut ActionPrincipalInputs); 5] = [
        // A declared input is missing.
        |resolved| {
            resolved.values.remove("financeRoles");
        },
        // An undeclared value rides along.
        |resolved| {
            resolved
                .values
                .insert("otherPrincipalRoles".into(), text("user:eve=owner"));
        },
        // A value of the wrong type.
        |resolved| {
            resolved
                .values
                .insert("isApprover".into(), text("yes-secret-value"));
        },
        // Resolved for another subject.
        |resolved| resolved.subject = FactKey::new(EXPENSE, "EXP-2"),
        // No policy version.
        |resolved| resolved.policy_version.clear(),
    ];
    let mut hosts: Vec<Box<dyn ActionAuthorizer + '_>> = Vec::new();
    for tamper in tampers {
        hosts.push(Box::new(Tampered {
            honest: &honest,
            tamper,
        }));
    }
    hosts.push(Box::new(NoResolver));
    for (index, host) in hosts.iter().enumerate() {
        let key = format!("closed-{index}");
        let refused = commit_action(
            &mut cell,
            &catalog,
            &call(&action, "user:alice", &key)?,
            host.as_ref(),
        )
        .expect_err("an unresolved input refuses");
        assert_eq!(refused.code(), UNAVAILABLE, "{refused}");
        let message = refused.to_string();
        assert!(
            !message.contains("secret") && !message.contains("eve") && !message.contains("owner"),
            "{message}"
        );
        assert_eq!(status(&cell)?, text("draft"));
        assert!(stored_record(&cell, &key)?.is_none());
    }
    // A principal the host did not verify resolves nothing.
    let unknown = commit_action(
        &mut cell,
        &catalog,
        &call(&action, "user:carol", "carol-1")?,
        &honest,
    )
    .expect_err("an unverified principal resolves nothing");
    assert_eq!(unknown.code(), UNAVAILABLE);
    assert!(unknown.to_string().contains("isApprover"), "{unknown}");
    Ok(())
}

/// CREATE-WRITER-1 × PRINCIPAL-INPUT-1 (#1760): the writer of a governed
/// create receives the declared principal inputs through the same host
/// resolution as a transition's decision, with the object the call creates
/// as its subject. The create commits for a verified holder of the role and
/// records the inputs on its call record; a principal without the role is
/// refused by the object's own invariant; an input that cannot be resolved
/// refuses `decision.principal_input_unavailable`. A refusal creates no
/// object and no call record, and an exact retry replays without resolving.
#[test]
fn a_governed_create_writer_reads_resolved_principal_inputs_and_fails_closed() -> TestResult {
    let directory = tempfile::tempdir()?;
    let definitions = types()?;
    let rules = [rule()?, advance_rule()?];
    let catalog = TypeCatalog::new(&definitions).with_action_rules(&rules);
    let action = file_advance()?;
    let host = Directory::new(&[("user:alice", &["approver"]), ("user:bob", &["member"])])?;
    let mut cell = open(&directory.path().join("create.sqlite"), &definitions)?;
    let advance = FactKey::new(ADVANCE, "ADV-1");
    let file =
        |actor: &str, key: &str| call_with(&action, actor, key, json!({"advance_id": "ADV-1"}));
    let untouched = |cell: &Cell, key: &str| -> TestResult {
        assert!(
            !cell.load(&advance)?.exists(),
            "{key}: no object is created"
        );
        assert!(
            stored_call(cell, "expense.advance.file", key)?.is_none(),
            "{key}: no call record is written"
        );
        Ok(())
    };

    // Unresolved: a principal the host did not verify, and a host answer
    // that omits a declared input. Nothing resolves, nothing is created.
    let unknown = commit_action(&mut cell, &catalog, &file("user:carol", "carol-1")?, &host)
        .expect_err("an unverified principal resolves nothing");
    assert_eq!(unknown.code(), UNAVAILABLE, "{unknown}");
    untouched(&cell, "carol-1")?;
    let incomplete = Tampered {
        honest: &host,
        tamper: |resolved| {
            resolved.values.remove("isApprover");
        },
    };
    let missing = commit_action(
        &mut cell,
        &catalog,
        &file("user:alice", "alice-0")?,
        &incomplete,
    )
    .expect_err("an incomplete answer resolves nothing");
    assert_eq!(missing.code(), UNAVAILABLE, "{missing}");
    assert!(missing.to_string().contains("isApprover"), "{missing}");
    untouched(&cell, "alice-0")?;

    // Resolved, but the verified principal does not hold the role: the
    // writer confirms nothing and the object's invariant refuses the create.
    let refused = commit_action(&mut cell, &catalog, &file("user:bob", "bob-1")?, &host)
        .expect_err("a non-approver files nothing");
    assert!(refused.code().ends_with("invariant_violation"), "{refused}");
    untouched(&cell, "bob-1")?;

    // Resolved for a holder of the role: the create commits with the
    // writer's value, and the receipt records the inputs as a transition's
    // does, with the created object as their subject.
    let resolutions = host.resolutions.get();
    let request = file("user:alice", "alice-1")?;
    let ActionOutcome::Committed { record, .. } =
        commit_action(&mut cell, &catalog, &request, &host)?
    else {
        panic!("the create commits");
    };
    assert_eq!(host.resolutions.get(), resolutions.saturating_add(1));
    let created = cell.load(&advance)?.values.ok_or("the advance exists")?;
    assert_eq!(
        created.get("issuer_confirmed"),
        Some(&RuntimeValue::Bool(true))
    );
    let recorded = record
        .principal_inputs
        .clone()
        .ok_or("the record has no principal inputs")?;
    assert_eq!(recorded.principal, "user:alice");
    assert_eq!(recorded.policy_version, "grants/1");
    assert_eq!(recorded.subject, advance);
    assert_eq!(
        recorded.values,
        JsonObject::from([
            ("actorId".to_owned(), text("user:alice")),
            (
                "financeRoles".to_owned(),
                RuntimeValue::Array(vec![text("approver")])
            ),
            ("isApprover".to_owned(), RuntimeValue::Bool(true)),
        ])
    );
    assert_eq!(
        stored_call(&cell, "expense.advance.file", "alice-1")?,
        Some(record.clone())
    );

    // An exact retry replays the recorded call and never resolves again,
    // also after the grant is revoked.
    host.roles.borrow_mut().clear();
    let ActionOutcome::Replayed { record: replayed } =
        commit_action(&mut cell, &catalog, &request, &host)?
    else {
        panic!("an exact retry replays");
    };
    assert_eq!(host.resolutions.get(), resolutions.saturating_add(1));
    assert_eq!(replayed, record);
    Ok(())
}
