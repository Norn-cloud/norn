//! Honesty, coverage, and universal wire-roundtrip tests for `norn-testkit`.
//!
//! Covers:
//! - `law_example_runtime_parity` reporting `EvidenceLevel::Unknown` when zero
//!   law examples run (never falsely claiming `RuntimeEnforced`).
//! - `run_pack_dst` refusing `cases == 0` and reporting `EvidenceLevel::Unknown`
//!   when a pack has zero lifecycle targets.
//! - `check_direct_lifecycle_write` exercising a distinct target state.
//! - `analyze_static_gaps` flagging non-terminal states whose only outgoing
//!   transition is a self-loop (`s -> s`) as `DeadEndNonTerminalState`.
//! - `analyze_static_gaps` rejecting unrelated-type `compensation` targets for
//!   immutable financial types while accepting linked/same-type reversals.
//! - `ObservedPdpAuthorizer` failing closed on slice reads by default and
//!   enforcing real `Pdp` read policies when configured with
//!   `with_read_authorizers`.
//! - Universal wire roundtrip for `Decimal`, `i64` beyond `±2^53`, and JSON
//!   nesting depths `1..=256`.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use norn_ir::canonical::{
    CanonicalJsonError, JsonValue as CanonicalJsonValue, canonical_json_exact_of, parse_json_tokens,
};
use norn_ir::{ArtifactEnvelope, Decimal, ObjectTypeName, RuntimeValue};
use norn_kernel::authz::{
    AccessRequest, ActorKind, AuthzSchema, Binding, CapabilityGrant, Pdp, ResourceInput, ScopeKey,
    UnboundProfile, VerifiedPrincipal, compile_all_projected,
};
use norn_kernel::cell_host::{
    CellCommit, CellHost, CommitError, CommitReceipt, DueTimer, FactKey, FactState, JsonObject,
    LinkKey, LinkState, Observed, Performed,
};
use norn_kernel::pack_mbt_support::cedar_projection::{
    project_cedar_schema_with_catalog, project_host_access_schema,
};
use norn_kernel::{
    ActionAuthorizer, ActionDenial, ActionReadAuthzRequest, ActionReadTypeAuthzRequest, FactEvent,
    LogicalTime,
};
use norn_testkit::cell_host::MemoryCell;
use norn_testkit::pack_mbt::coverage::{DomainCoverage, ObservedPdpAuthorizer, PolicyRuleTarget};
use norn_testkit::pack_mbt::driver::run_pack_mbt;
use norn_testkit::pack_mbt::dst::{PackDstConfig, run_pack_dst};
use norn_testkit::pack_mbt::static_gaps::{StructuralGap, analyze_static_gaps};
use norn_testkit::pack_mbt::{AdmittedPack, EvidenceLevel, PackHost};
use norn_testkit::proptest::prelude::*;
use serde_json::{Value, json};

struct MemoryPackHost {
    cell: MemoryCell<FactEvent, JsonObject, JsonObject>,
    now: LogicalTime,
}

impl MemoryPackHost {
    fn open() -> Self {
        Self {
            cell: MemoryCell::new(|cmd| cmd.payload.clone()),
            now: LogicalTime::from_millis(1_000),
        }
    }
}

impl CellHost for MemoryPackHost {
    type Event = FactEvent;
    type Command = JsonObject;
    type Input = JsonObject;
    type Error = Infallible;

    fn load(&self, key: &FactKey) -> Result<FactState, Infallible> {
        self.cell.load(key)
    }

    fn load_link(&self, key: &LinkKey) -> Result<LinkState, Infallible> {
        self.cell.load_link(key)
    }

    fn commit(
        &mut self,
        commit: CellCommit<FactEvent, JsonObject, JsonObject>,
    ) -> Result<CommitReceipt, CommitError<Infallible>> {
        self.cell.commit(commit)
    }

    fn perform(&mut self) -> Result<Option<Performed<JsonObject>>, Infallible> {
        self.cell.perform()
    }

    fn observe(&mut self) -> Result<Option<Observed<JsonObject>>, Infallible> {
        self.cell.observe()
    }

    fn clock(&mut self) -> Result<Option<DueTimer<JsonObject>>, Infallible> {
        self.cell.clock()
    }

    fn publish(&mut self, receipt: &CommitReceipt) -> Result<(), Infallible> {
        self.cell.publish(receipt)
    }
}

impl PackHost for MemoryPackHost {
    fn ledger_len(&self) -> usize {
        self.cell.ledger().len()
    }

    fn logical_now(&self) -> LogicalTime {
        self.now
    }

    fn advance_clock(&mut self, delta_millis: u64) {
        self.now = LogicalTime::from_millis(self.now.as_millis().saturating_add(delta_millis));
    }

    fn verify_ledger(&self) -> Result<(), String> {
        Ok(())
    }

    fn reopen(&mut self) -> Result<(), String> {
        Ok(())
    }
}

fn decode_pack(name: &str, raw: Vec<Value>) -> Result<AdmittedPack, String> {
    let envelopes: Vec<ArtifactEnvelope> = raw
        .into_iter()
        .map(|val| serde_json::from_value(val).map_err(|err| format!("envelope: {err}")))
        .collect::<Result<Vec<_>, _>>()?;
    AdmittedPack::from_envelopes(name, &envelopes)
}

#[test]
fn law_example_runtime_parity_reports_unknown_when_zero_examples_run() -> Result<(), String> {
    let pack = decode_pack(
        "no_examples",
        vec![json!({
            "irVersion": "0.1", "artifactId": "ex.Order.entity", "version": "1.0.0",
            "kind": "entity", "namespace": "ex", "name": "Order", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "entityVersion": "2.0",
                "objectType": {
                    "name": "ex.Order",
                    "identity": [{"name": "primary", "properties": ["order_id"]}],
                    "properties": [
                        {"id": "order_id", "type": {"kind": "text"}},
                        {
                            "id": "status",
                            "type": {"kind": "enum", "values": [{"value": "draft"}, {"value": "active"}]},
                            "default": "draft"
                        }
                    ],
                    "lifecycles": [{
                        "property": "status",
                        "initial": ["draft"],
                        "terminal": ["active"],
                        "transitions": [{"id": "activate", "from": ["draft"], "to": "active"}]
                    }]
                }
            }
        })],
    )?;

    let report = run_pack_mbt(&pack, |_| Ok(MemoryPackHost::open()))?;
    assert_eq!(report.law_examples_checked, 0);
    assert!(
        report
            .evidence
            .contains(&("law_example_runtime_parity", EvidenceLevel::Unknown)),
        "expected Unknown evidence when zero law examples ran: {:?}",
        report.evidence
    );
    Ok(())
}

#[test]
fn run_pack_dst_rejects_zero_cases_and_reports_unknown_on_empty_targets() -> Result<(), String> {
    let pack = decode_pack(
        "no_lifecycle",
        vec![json!({
            "irVersion": "0.1", "artifactId": "plain.Note.entity", "version": "1.0.0",
            "kind": "entity", "namespace": "plain", "name": "Note", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "entityVersion": "2.0",
                "objectType": {
                    "name": "plain.Note",
                    "identity": [{"name": "primary", "properties": ["note_id"]}],
                    "properties": [{"id": "note_id", "type": {"kind": "text"}}]
                }
            }
        })],
    )?;

    let zero_cases = run_pack_dst(
        &pack,
        PackDstConfig {
            seed: [7; 32],
            cases: 0,
            max_steps: 4,
            entities_per_type: 1,
        },
        |_| Ok(MemoryPackHost::open()),
    );
    assert!(zero_cases.is_err(), "cases == 0 must be refused");

    let empty = run_pack_dst(
        &pack,
        PackDstConfig {
            seed: [7; 32],
            cases: 4,
            max_steps: 4,
            entities_per_type: 1,
        },
        |_| Ok(MemoryPackHost::open()),
    )?;
    assert_eq!(empty.cases_run, 0);
    assert_eq!(empty.evidence, EvidenceLevel::Unknown);
    Ok(())
}

#[test]
fn static_gaps_flags_self_loop_only_non_terminal_state_as_dead_end() -> Result<(), String> {
    let pack = decode_pack(
        "self_loop_dead_end",
        vec![json!({
            "irVersion": "0.1", "artifactId": "sl.Task.entity", "version": "1.0.0",
            "kind": "entity", "namespace": "sl", "name": "Task", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {
                "entityVersion": "2.0",
                "objectType": {
                    "name": "sl.Task",
                    "identity": [{"name": "primary", "properties": ["task_id"]}],
                    "properties": [
                        {"id": "task_id", "type": {"kind": "text"}},
                        {
                            "id": "status",
                            "type": {
                                "kind": "enum",
                                "values": [{"value": "open"}, {"value": "limbo"}, {"value": "closed"}]
                            },
                            "default": "open"
                        }
                    ],
                    "lifecycles": [{
                        "property": "status",
                        "initial": ["open"],
                        "terminal": ["closed"],
                        "transitions": [
                            {"id": "stall", "from": ["open"], "to": "limbo"},
                            {"id": "spin", "from": ["limbo"], "to": "limbo"},
                            {"id": "finish", "from": ["open"], "to": "closed"}
                        ]
                    }]
                }
            }
        })],
    )?;

    let gaps = analyze_static_gaps(&pack);
    assert!(
        gaps.gaps.iter().any(|gap| matches!(
            gap,
            StructuralGap::DeadEndNonTerminalState { object_type, state, .. }
                if object_type == "sl.Task" && state == "limbo"
        )),
        "expected DeadEndNonTerminalState for self-loop-only state `limbo`: {gaps}"
    );
    Ok(())
}

fn immutable_financial_entity() -> Value {
    json!({
        "irVersion": "0.1", "artifactId": "fin.LedgerEntry.entity", "version": "1.0.0",
        "kind": "entity", "namespace": "fin", "name": "LedgerEntry", "lifecycle": "active",
        "scope": {}, "dependencies": [], "provenance": {"source": "system"},
        "payload": {
            "entityVersion": "2.0",
            "objectType": {
                "name": "fin.LedgerEntry",
                "immutable": true,
                "identity": [{"name": "primary", "properties": ["entry_id"]}],
                "properties": [
                    {"id": "entry_id", "type": {"kind": "text"}},
                    {"id": "amount", "type": {"kind": "decimal", "scale": 2}}
                ]
            }
        }
    })
}

fn post_entry_action(compensation_action_id: &str) -> Value {
    json!({
        "irVersion": "0.1", "artifactId": "fin.entry.post.action", "version": "1.0.0",
        "kind": "action", "namespace": "fin", "name": "Post entry", "lifecycle": "active",
        "scope": {}, "dependencies": [], "provenance": {"source": "system"},
        "payload": {
            "actionId": "fin.entry.post",
            "scopeRequired": ["orgId"],
            "idempotent": true,
            "ledgered": true,
            "inputSchema": {"type": "object", "properties": {"entry_id": {"type": "string"}}, "required": ["entry_id"]},
            "outputSchema": {"type": "object", "properties": {"entry_id": {"type": "string"}}, "required": ["entry_id"]},
            "writes": [{"entityKind": "fin.LedgerEntry", "change": "created"}],
            "compensation": {"actionId": compensation_action_id}
        }
    })
}

fn has_missing_financial_reversal(pack: &AdmittedPack, target: &str) -> bool {
    analyze_static_gaps(pack).gaps.iter().any(|gap| {
        matches!(
            gap,
            StructuralGap::ImmutableFinancialMissingReversal { object_type, .. }
                if object_type == target
        )
    })
}

#[test]
fn static_gaps_rejects_unrelated_write_compensation() -> Result<(), String> {
    let pack = decode_pack(
        "unrelated_comp",
        vec![
            immutable_financial_entity(),
            json!({
                "irVersion": "0.1", "artifactId": "audit.Note.entity", "version": "1.0.0",
                "kind": "entity", "namespace": "audit", "name": "Note", "lifecycle": "active",
                "scope": {}, "dependencies": [], "provenance": {"source": "system"},
                "payload": {
                    "entityVersion": "2.0",
                    "objectType": {
                        "name": "audit.Note",
                        "identity": [{"name": "primary", "properties": ["note_id"]}],
                        "properties": [{"id": "note_id", "type": {"kind": "text"}}]
                    }
                }
            }),
            post_entry_action("audit.note.create"),
            json!({
                "irVersion": "0.1", "artifactId": "audit.note.create.action", "version": "1.0.0",
                "kind": "action", "namespace": "audit", "name": "Create note", "lifecycle": "active",
                "scope": {}, "dependencies": [], "provenance": {"source": "system"},
                "payload": {
                    "actionId": "audit.note.create",
                    "scopeRequired": ["orgId"],
                    "idempotent": true,
                    "ledgered": true,
                    "inputSchema": {"type": "object", "properties": {"note_id": {"type": "string"}}, "required": ["note_id"]},
                    "outputSchema": {"type": "object", "properties": {"note_id": {"type": "string"}}, "required": ["note_id"]},
                    "writes": [{"entityKind": "audit.Note", "change": "created"}]
                }
            }),
        ],
    )?;
    assert!(
        has_missing_financial_reversal(&pack, "fin.LedgerEntry"),
        "unrelated audit.Note write must not verify financial reversal"
    );
    Ok(())
}

#[test]
fn static_gaps_rejects_unrelated_quantity_effect_compensation() -> Result<(), String> {
    let pack = decode_pack(
        "unrelated_qty_comp",
        vec![
            immutable_financial_entity(),
            post_entry_action("inv.stock.reserve"),
            json!({
                "irVersion": "0.1", "artifactId": "inv.stock.reserve.action", "version": "1.0.0",
                "kind": "action", "namespace": "inv", "name": "Reserve stock", "lifecycle": "active",
                "scope": {}, "dependencies": [], "provenance": {"source": "system"},
                "payload": {
                    "actionId": "inv.stock.reserve",
                    "scopeRequired": ["orgId"],
                    "idempotent": true,
                    "ledgered": true,
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "res_id": {"type": "string"},
                            "balance_versions": {"type": "object"}
                        },
                        "required": ["res_id", "balance_versions"]
                    },
                    "outputSchema": {"type": "object", "properties": {"res_id": {"type": "string"}}, "required": ["res_id"]},
                    "quantityEffect": {
                        "ledgerId": "inv.stock",
                        "balanceVersionsField": "balance_versions",
                        "operation": {
                            "kind": "reserve",
                            "reservationIdField": "res_id",
                            "templateId": "hold_stock",
                            "parameters": {},
                            "ttlMs": 60_000
                        }
                    }
                }
            }),
        ],
    )?;
    assert!(
        has_missing_financial_reversal(&pack, "fin.LedgerEntry"),
        "unrelated quantityEffect reservation must not verify financial reversal"
    );
    Ok(())
}

#[test]
fn static_gaps_accepts_linked_financial_reversal_compensation() -> Result<(), String> {
    let pack = decode_pack(
        "linked_comp",
        vec![
            immutable_financial_entity(),
            json!({
                "irVersion": "0.1", "artifactId": "fin.CreditMemo.entity", "version": "1.0.0",
                "kind": "entity", "namespace": "fin", "name": "CreditMemo", "lifecycle": "active",
                "scope": {}, "dependencies": [], "provenance": {"source": "system"},
                "payload": {
                    "entityVersion": "2.0",
                    "objectType": {
                        "name": "fin.CreditMemo",
                        "identity": [{"name": "primary", "properties": ["memo_id"]}],
                        "properties": [{"id": "memo_id", "type": {"kind": "text"}}],
                        "links": [{"id": "reverses", "target": "fin.LedgerEntry", "cardinality": {"to": "one", "from": "many"}}]
                    }
                }
            }),
            post_entry_action("fin.credit.issue"),
            json!({
                "irVersion": "0.1", "artifactId": "fin.credit.issue.action", "version": "1.0.0",
                "kind": "action", "namespace": "fin", "name": "Issue credit memo", "lifecycle": "active",
                "scope": {}, "dependencies": [], "provenance": {"source": "system"},
                "payload": {
                    "actionId": "fin.credit.issue",
                    "scopeRequired": ["orgId"],
                    "idempotent": true,
                    "ledgered": true,
                    "inputSchema": {"type": "object", "properties": {"memo_id": {"type": "string"}, "entry_id": {"type": "string"}}, "required": ["memo_id", "entry_id"]},
                    "outputSchema": {"type": "object", "properties": {"memo_id": {"type": "string"}}, "required": ["memo_id"]},
                    "createLinks": [{"link": "reverses", "targetType": "fin.LedgerEntry", "targetIdField": "entry_id"}],
                    "writes": [{"entityKind": "fin.CreditMemo", "change": "created"}]
                }
            }),
        ],
    )?;
    assert!(
        !has_missing_financial_reversal(&pack, "fin.LedgerEntry"),
        "linked financial reversal should satisfy reversal coverage"
    );
    Ok(())
}

fn build_read_policy_fixture() -> Result<(AdmittedPack, Arc<Pdp>, ScopeKey, ScopeKey), String> {
    let pack = decode_pack(
        "read_authz",
        vec![
            json!({
                "irVersion": "0.1", "artifactId": "sec.Doc.entity", "version": "1.0.0",
                "kind": "entity", "namespace": "sec", "name": "Doc", "lifecycle": "active",
                "scope": {}, "dependencies": [], "provenance": {"source": "system"},
                "payload": {
                    "entityVersion": "2.0",
                    "objectType": {
                        "name": "sec.Doc",
                        "identity": [{"name": "primary", "properties": ["doc_id"]}],
                        "properties": [{"id": "doc_id", "type": {"kind": "text"}}]
                    }
                }
            }),
            json!({
                "irVersion": "0.1", "artifactId": "sec.doc.touch.action", "version": "1.0.0",
                "kind": "action", "namespace": "sec", "name": "Touch doc", "lifecycle": "active",
                "scope": {}, "dependencies": [], "provenance": {"source": "system"},
                "payload": {
                    "actionId": "sec.doc.touch",
                    "scopeRequired": ["orgId"],
                    "idempotent": true,
                    "ledgered": true,
                    "inputSchema": {"type": "object", "properties": {"doc_id": {"type": "string"}}, "required": ["doc_id"]},
                    "outputSchema": {"type": "object", "properties": {"doc_id": {"type": "string"}}, "required": ["doc_id"]},
                    "writes": [{"entityKind": "sec.Doc", "change": "modified"}]
                }
            }),
            json!({
                "irVersion": "0.1", "artifactId": "sec.access.policy", "version": "1.0.0",
                "kind": "policy", "namespace": "sec", "name": "Doc read access", "lifecycle": "active",
                "scope": {}, "dependencies": [], "provenance": {"source": "system"},
                "payload": {
                    "accessVersion": "0.1",
                    "policyId": "sec.access",
                    "rules": [{
                        "ruleId": "reader_read",
                        "actions": ["norn.read"],
                        "resourceType": "sec.Doc",
                        "scope": {"dimensions": ["org"]},
                        "allOf": [{"role": {"anyOf": ["reader"]}}]
                    }]
                }
            }),
        ],
    )?;

    let action_roots: Vec<_> = pack.actions.iter().map(|a| a.root.clone()).collect();
    let projection = project_cedar_schema_with_catalog(
        &pack.object_types,
        &pack.interfaces,
        &pack.property_types,
        &action_roots,
    );
    let host_schema = project_host_access_schema(&pack.object_types);
    let schema =
        AuthzSchema::from_projected_cedarschema_fragments(&[&projection.schema, &host_schema])
            .map_err(|err| format!("schema: {err}"))?;
    let policy_set = compile_all_projected(&schema, &pack.access_policies, &projection)
        .map_err(|err| format!("compile: {err}"))?;
    let home = ScopeKey {
        org: "org-home".to_owned(),
        tenant: None,
        workspace: None,
        environment: None,
        business_unit: None,
    };
    let foreign = ScopeKey {
        org: "org-foreign".to_owned(),
        tenant: None,
        workspace: None,
        environment: None,
        business_unit: None,
    };
    let grant = |subject: &str, scope: &ScopeKey| CapabilityGrant {
        subject_id: subject.to_owned(),
        actor_kind: ActorKind::User,
        agent_session_id: None,
        scope: Some(scope.clone()),
        capabilities: vec!["norn.read".to_owned()],
        binding: None,
    };
    let pdp = Arc::new(
        Pdp::new(
            policy_set,
            &[
                grant("alice", &home),
                grant("bob", &home),
                grant("alice", &foreign),
            ],
        )
        .map_err(|err| format!("pdp: {err}"))?,
    );
    Ok((pack, pdp, home, foreign))
}

fn make_read_authorizer(
    pdp: &Arc<Pdp>,
    coverage: &Arc<Mutex<DomainCoverage>>,
    deny_rule_hint: Option<PolicyRuleTarget>,
    roles: Vec<String>,
    caller_scope: ScopeKey,
    resource_scope: ScopeKey,
) -> impl ActionAuthorizer {
    let r_roles = roles.clone();
    let r_scope = caller_scope.clone();
    let r_res_scope = resource_scope.clone();
    ObservedPdpAuthorizer::new(
        Arc::clone(pdp),
        Arc::clone(coverage),
        deny_rule_hint,
        |_| Err(ActionDenial::Invisible),
    )
    .with_read_authorizers(
        move |req| {
            Ok(AccessRequest::new(
                &VerifiedPrincipal::new(
                    req.actor,
                    ActorKind::User,
                    None,
                    roles.clone(),
                    BTreeMap::new(),
                ),
                Some(caller_scope.clone()),
                "norn.read",
                "norn.read",
                ResourceInput {
                    object_type: req.target.object_type.as_str().to_owned(),
                    id: req.target.key.clone(),
                    scope: resource_scope.clone(),
                    attributes: BTreeMap::new(),
                },
                BTreeMap::new(),
                Binding::Unbound(UnboundProfile::HostedAction),
            ))
        },
        move |req| {
            Ok(AccessRequest::new(
                &VerifiedPrincipal::new(
                    req.actor,
                    ActorKind::User,
                    None,
                    r_roles.clone(),
                    BTreeMap::new(),
                ),
                Some(r_scope.clone()),
                "norn.read",
                "norn.read",
                ResourceInput {
                    object_type: req.object_type.as_str().to_owned(),
                    id: req.object_type.as_str().to_owned(),
                    scope: r_res_scope.clone(),
                    attributes: BTreeMap::new(),
                },
                BTreeMap::new(),
                Binding::Unbound(UnboundProfile::HostedAction),
            ))
        },
    )
}

fn assert_read_authz_coverage(
    coverage: &Arc<Mutex<DomainCoverage>>,
    action_hint: &PolicyRuleTarget,
) -> Result<(), String> {
    let cov = coverage
        .lock()
        .map_err(|err| format!("coverage lock: {err}"))?;
    assert_eq!(
        cov.cedar_denies.get(action_hint).copied().unwrap_or(0),
        0,
        "read denials must never credit the action rule's deny_rule_hint: {:?}",
        cov.cedar_denies
    );
    assert!(
        cov.cedar_allows
            .values()
            .copied()
            .fold(0u64, u64::saturating_add)
            >= 2,
        "expected PDP read allow hits to be recorded: {:?}",
        cov.cedar_allows
    );
    assert!(
        cov.cedar_denies
            .iter()
            .filter(|(target, _)| target.policy_id == "norn.read")
            .map(|(_, count)| *count)
            .fold(0u64, u64::saturating_add)
            >= 2,
        "expected PDP read deny hits under norn.read: {:?}",
        cov.cedar_denies
    );
    Ok(())
}

#[test]
fn observed_pdp_authorizer_fails_closed_on_reads_and_enforces_pdp_read_rules() -> Result<(), String>
{
    let (pack, pdp, home, foreign) = build_read_policy_fixture()?;
    let action = pack
        .actions
        .first()
        .ok_or_else(|| "missing action".to_owned())?;
    let coverage = Arc::new(Mutex::new(DomainCoverage::default()));
    let default_authz =
        ObservedPdpAuthorizer::new(Arc::clone(&pdp), Arc::clone(&coverage), None, |_| {
            Err(ActionDenial::Invisible)
        });

    let scope_map = BTreeMap::from([("orgId".to_owned(), "org-home".to_owned())]);
    let input = JsonObject::new();
    let doc_type = ObjectTypeName("sec.Doc".to_owned());
    let target = FactKey::new("sec.Doc", "doc-1");
    let read_req = ActionReadAuthzRequest {
        action: &action.root,
        scope: &scope_map,
        actor: "alice",
        input: &input,
        declared_type: &doc_type,
        target: &target,
        target_values: None,
    };
    let read_type_req = ActionReadTypeAuthzRequest {
        action: &action.root,
        scope: &scope_map,
        actor: "alice",
        input: &input,
        object_type: &doc_type,
    };

    assert_eq!(
        default_authz.authorize_read(&read_req),
        Err(ActionDenial::Invisible)
    );
    assert_eq!(
        default_authz.authorize_read_type(&read_type_req),
        Err(ActionDenial::Invisible)
    );

    let action_hint = PolicyRuleTarget {
        policy_id: "sec.access".to_owned(),
        rule_id: "action_rule".to_owned(),
    };

    let allowed = make_read_authorizer(
        &pdp,
        &coverage,
        Some(action_hint.clone()),
        vec!["reader".to_owned()],
        home.clone(),
        home.clone(),
    );
    assert_eq!(allowed.authorize_read(&read_req), Ok(()));
    assert_eq!(allowed.authorize_read_type(&read_type_req), Ok(()));

    let guest = make_read_authorizer(
        &pdp,
        &coverage,
        Some(action_hint.clone()),
        vec!["guest".to_owned()],
        home.clone(),
        home.clone(),
    );
    assert_eq!(
        guest.authorize_read(&read_req),
        Err(ActionDenial::Invisible)
    );
    assert_eq!(
        guest.authorize_read_type(&read_type_req),
        Err(ActionDenial::Invisible)
    );

    let foreign_reader = make_read_authorizer(
        &pdp,
        &coverage,
        Some(action_hint.clone()),
        vec!["reader".to_owned()],
        foreign,
        home,
    );
    assert_eq!(
        foreign_reader.authorize_read(&read_req),
        Err(ActionDenial::Invisible)
    );
    assert_read_authz_coverage(&coverage, &action_hint)
}

fn nested_value(depth: usize) -> Value {
    let mut current = Value::String("leaf".to_owned());
    for level in 0..depth {
        if level.is_multiple_of(2) {
            let mut map = serde_json::Map::new();
            map.insert("k".to_owned(), current);
            current = Value::Object(map);
        } else {
            current = Value::Array(vec![current]);
        }
    }
    current
}

fn nested_canonical_tree(depth: usize) -> CanonicalJsonValue {
    let mut current = CanonicalJsonValue::String("leaf".to_owned());
    for level in 0..depth {
        if level.is_multiple_of(2) {
            current = CanonicalJsonValue::Object(BTreeMap::from([("k".to_owned(), current)]));
        } else {
            current = CanonicalJsonValue::Array(vec![current]);
        }
    }
    current
}

fn decimal_wire_text(mantissa: i128, scale: u32) -> String {
    let sign = if mantissa < 0 { "-" } else { "" };
    let digits = mantissa.unsigned_abs().to_string();
    let scale_usize = usize::try_from(scale).unwrap_or(0);
    if scale_usize == 0 {
        return format!("{sign}{digits}");
    }
    if digits.len() <= scale_usize {
        let zeros = "0".repeat(scale_usize.saturating_sub(digits.len()));
        format!("{sign}0.{zeros}{digits}")
    } else {
        let split = digits.len().saturating_sub(scale_usize);
        let (whole, frac) = digits.split_at(split);
        format!("{sign}{whole}.{frac}")
    }
}

#[test]
fn canonical_json_exact_of_enforces_exact_256_depth_boundary() -> Result<(), String> {
    let encoded_256 =
        canonical_json_exact_of(&nested_value(256)).map_err(|err| format!("depth 256: {err}"))?;
    let decoded_256 =
        parse_json_tokens(&encoded_256).map_err(|err| format!("decode 256: {err}"))?;
    assert_eq!(decoded_256, nested_canonical_tree(256));

    let over = canonical_json_exact_of(&nested_value(257));
    assert!(
        matches!(
            over,
            Err(CanonicalJsonError::NestingTooDeep {
                depth: 257,
                limit: 256,
                ..
            })
        ),
        "expected NestingTooDeep {{ depth: 257, limit: 256 }}, got {over:?}"
    );
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn universal_wire_roundtrip_preserves_decimals_large_ints_and_depth_256(
        large_pos in 9_007_199_254_740_993i64..=i64::MAX,
        large_neg in i64::MIN..=-9_007_199_254_740_993i64,
        mantissa in -999_999_999_999_999i128..=999_999_999_999_999i128,
        scale in 0u32..=18u32,
        depth in 1usize..=256usize,
    ) {
        // 1. Exact i64 values strictly beyond IEEE-754 ±2^53 safe integers.
        for n in [large_pos, large_neg] {
            let dec = Decimal::parse_wire(&n.to_string())
                .map_err(|err| TestCaseError::fail(format!("parse int {n}: {err}")))?;
            let rv = RuntimeValue::Number(dec);
            let encoded = canonical_json_exact_of(&rv)
                .map_err(|err| TestCaseError::fail(format!("canonical int {n}: {err}")))?;
            let decoded: RuntimeValue = serde_json::from_slice(&encoded)
                .map_err(|err| TestCaseError::fail(format!("decode int {n}: {err}")))?;
            prop_assert_eq!(decoded, rv);
        }

        // 2. Arbitrary Decimal wire strings roundtrip without float coercion.
        let wire = decimal_wire_text(mantissa, scale);
        let dec = Decimal::parse_wire(&wire)
            .map_err(|err| TestCaseError::fail(format!("parse_wire {wire}: {err}")))?;
        let rv_dec = RuntimeValue::Number(dec);
        let encoded_dec = canonical_json_exact_of(&rv_dec)
            .map_err(|err| TestCaseError::fail(format!("canonical dec {wire}: {err}")))?;
        let decoded_dec: RuntimeValue = serde_json::from_slice(&encoded_dec)
            .map_err(|err| TestCaseError::fail(format!("decode dec {wire}: {err}")))?;
        prop_assert_eq!(decoded_dec, rv_dec);

        // 3. Valid JSON nesting depths 1..=256 encode and decode structurally intact.
        let deep = nested_value(depth);
        let encoded_deep = canonical_json_exact_of(&deep)
            .map_err(|err| TestCaseError::fail(format!("canonical depth {depth}: {err}")))?;
        let decoded_deep = parse_json_tokens(&encoded_deep)
            .map_err(|err| TestCaseError::fail(format!("decode depth {depth}: {err}")))?;
        prop_assert_eq!(decoded_deep, nested_canonical_tree(depth));
    }
}
