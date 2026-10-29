//! Semantic checks of compiled policies with `cedar-policy`'s own
//! `Authorizer`: default-deny, the mandatory scope conjunct, roles, directional
//! `rel`, and typed context values. The kernel PDP (T5-AZ-3, #2578) wraps the
//! same evaluation.

use std::str::FromStr;

use cedar_policy::{Authorizer, Context, Decision, Entities, EntityUid, Request};
use norn_ir::access::AccessIr;
use serde_json::{Value, json};

use crate::{AuthzSchema, ValidatedPolicySet, compile};

const PROJECTED: &str = include_str!("../tests/fixtures/order.cedarschema");
const ORDER_ACCESS: &str = include_str!("../tests/fixtures/order_access.json");
const MARKING_SCHEMA: &str = r#"
namespace norn {
  entity Marking = {
    scope: Norn::Scope,
    id: String
  };
}
namespace Norn {
  action "norn.reveal" appliesTo {
    principal: [Actor],
    resource: [norn::Marking],
    context: { scope: Scope }
  };
  action "norn.declassify" appliesTo {
    principal: [Actor],
    resource: [norn::Marking],
    context: { scope: Scope }
  };
}
"#;

fn schema() -> AuthzSchema {
    AuthzSchema::from_projected_cedarschema(PROJECTED).expect("projected schema loads")
}

fn compiled(access: &str) -> ValidatedPolicySet {
    let access: AccessIr = serde_json::from_str(access).expect("AccessIR decodes");
    compile(&schema(), &access).expect("compiles")
}

fn uid(text: &str) -> EntityUid {
    EntityUid::from_str(text).expect("entity uid")
}

/// One request against the order fixture.
struct Case {
    principal_scope: Value,
    principal_roles: Vec<&'static str>,
    resource_scope: Value,
    watchers: Vec<&'static str>,
    /// Principal-side link role `accounts` (sales order ids); omitted when
    /// empty (the attribute is optional).
    accounts: Vec<&'static str>,
    request_scope: Value,
    action: &'static str,
    mfa: bool,
    channel: &'static str,
}

fn tenant_scope() -> Value {
    json!({ "org": "o1", "tenant": "t1", "environment": "prod" })
}

impl Case {
    fn approve() -> Self {
        Self {
            principal_scope: tenant_scope(),
            principal_roles: vec!["approver"],
            resource_scope: tenant_scope(),
            watchers: vec![],
            accounts: vec![],
            request_scope: tenant_scope(),
            action: "commerce.order.approve",
            mfa: true,
            channel: "web",
        }
    }

    fn decide(&self, policies: &ValidatedPolicySet) -> Decision {
        let schema = policies.schema().cedar();
        let roles: Vec<Value> = self
            .principal_roles
            .iter()
            .map(|role| json!({ "type": "Norn::Role", "id": role }))
            .collect();
        let watchers: Vec<Value> = self
            .watchers
            .iter()
            .map(|id| json!({ "__entity": { "type": "Norn::Actor", "id": id } }))
            .collect();
        let mut alice_attrs = json!({ "scope": self.principal_scope });
        if !self.accounts.is_empty() {
            let accounts: Vec<Value> = self
                .accounts
                .iter()
                .map(|id| json!({ "__entity": { "type": "commerce::SalesOrder", "id": id } }))
                .collect();
            alice_attrs["accounts"] = Value::Array(accounts);
        }
        let mut entities = vec![
            json!({
                "uid": { "type": "Norn::Actor", "id": "alice" },
                "attrs": alice_attrs,
                "parents": roles,
            }),
            json!({
                "uid": { "type": "Norn::Actor", "id": "bob" },
                "attrs": { "scope": self.principal_scope },
                "parents": [],
            }),
            json!({
                "uid": { "type": "commerce::SalesOrder", "id": "so1" },
                "attrs": {
                    "scope": self.resource_scope,
                    "customer": { "__entity": { "type": "Norn::Actor", "id": "bob" } },
                    "watchers": watchers,
                    "total": 10,
                },
                "parents": [],
            }),
        ];
        for role in ["approver", "auditor", "finance.manager", "intruder"] {
            entities.push(json!({
                "uid": { "type": "Norn::Role", "id": role },
                "attrs": {},
                "parents": [],
            }));
        }
        let entities = Entities::from_json_str(&Value::Array(entities).to_string(), Some(schema))
            .expect("entities conform to the schema");

        let action = uid(&format!("Norn::Action::{:?}", self.action));
        let context = if self.action == "commerce.order.approve" {
            json!({ "scope": self.request_scope, "mfa": self.mfa, "channel": self.channel })
        } else {
            json!({ "scope": self.request_scope })
        };
        let context = Context::from_json_str(&context.to_string(), Some((schema, &action)))
            .expect("context conforms to the schema");
        let request = Request::new(
            uid(r#"Norn::Actor::"alice""#),
            action,
            uid(r#"commerce::SalesOrder::"so1""#),
            context,
            Some(schema),
        )
        .expect("request conforms to the schema");
        Authorizer::new()
            .is_authorized(&request, policies.cedar_policies(), &entities)
            .decision()
    }
}

#[test]
fn approve_is_allowed_only_when_every_conjunct_holds() {
    let policies = compiled(ORDER_ACCESS);
    assert_eq!(Case::approve().decide(&policies), Decision::Allow);

    let deny = |case: Case, why: &str| {
        assert_eq!(case.decide(&policies), Decision::Deny, "{why}");
    };
    deny(
        Case {
            principal_roles: vec!["intruder"],
            ..Case::approve()
        },
        "no permitted role",
    );
    deny(
        Case {
            mfa: false,
            ..Case::approve()
        },
        "context mfa is false",
    );
    deny(
        Case {
            channel: "api \"legacy\"",
            ..Case::approve()
        },
        "excluded channel (escaped literal)",
    );
    deny(
        Case {
            resource_scope: json!({ "org": "o1", "tenant": "t2", "environment": "prod" }),
            ..Case::approve()
        },
        "resource in another tenant",
    );
    deny(
        Case {
            principal_scope: json!({ "org": "o2", "tenant": "t1", "environment": "prod" }),
            ..Case::approve()
        },
        "principal in another org",
    );
    deny(
        Case {
            request_scope: json!({ "org": "o1", "tenant": "t1", "environment": "dev" }),
            ..Case::approve()
        },
        "request in another environment",
    );
}

#[test]
fn absent_scope_dimension_is_never_a_wildcard() {
    let policies = compiled(ORDER_ACCESS);
    // Principal bound to the whole org (no tenant) acting on a tenant order.
    let org_wide = Case {
        principal_scope: json!({ "org": "o1", "environment": "prod" }),
        ..Case::approve()
    };
    assert_eq!(org_wide.decide(&policies), Decision::Deny);

    // All three agree but omit a dimension the rule requires (`tenant`).
    let org = json!({ "org": "o1", "environment": "prod" });
    let missing_required = Case {
        principal_scope: org.clone(),
        resource_scope: org.clone(),
        request_scope: org,
        ..Case::approve()
    };
    assert_eq!(missing_required.decide(&policies), Decision::Deny);
}

#[test]
fn directional_rel_and_role_rules() {
    let policies = compiled(ORDER_ACCESS);
    let view = Case {
        action: "commerce.order.view",
        principal_roles: vec![],
        ..Case::approve()
    };
    assert_eq!(
        Case {
            watchers: vec![],
            ..view
        }
        .decide(&policies),
        Decision::Deny,
        "not a watcher, no role"
    );
    let view = Case {
        action: "commerce.order.view",
        principal_roles: vec![],
        ..Case::approve()
    };
    assert_eq!(
        Case {
            watchers: vec!["alice"],
            ..view
        }
        .decide(&policies),
        Decision::Allow,
        "watcher via resource.watchers"
    );
    let auditor = Case {
        action: "commerce.order.view",
        principal_roles: vec!["auditor"],
        ..Case::approve()
    };
    assert_eq!(auditor.decide(&policies), Decision::Allow, "auditor role");
}

#[test]
fn empty_policy_denies_everything() {
    let policies = compiled(r#"{ "accessVersion": "0.1", "policyId": "p", "rules": [] }"#);
    assert!(policies.is_empty());
    assert_eq!(Case::approve().decide(&policies), Decision::Deny);
    let view = Case {
        action: "commerce.order.view",
        watchers: vec!["alice"],
        ..Case::approve()
    };
    assert_eq!(view.decide(&policies), Decision::Deny);
}

#[test]
fn hostile_role_ids_stay_literal() {
    // A role id that would close the literal and widen the condition if it
    // were spliced as Cedar text.
    let access = json!({
        "accessVersion": "0.1",
        "policyId": "p",
        "rules": [{
            "ruleId": "r\")\npermit(principal,action,resource);//",
            "actions": ["commerce.order.view"],
            "resourceType": "commerce.SalesOrder",
            "scope": { "dimensions": ["org"] },
            "allOf": [{ "role": { "anyOf": ["x\"] || true || principal in [Norn::Role::\"y"] } }]
        }]
    });
    let policies = compiled(&access.to_string());
    assert_eq!(policies.len(), 1);
    let view = Case {
        action: "commerce.order.view",
        principal_roles: vec!["approver"],
        ..Case::approve()
    };
    assert_eq!(view.decide(&policies), Decision::Deny);
}

#[test]
fn principal_side_rel_uses_the_projected_principal_link() {
    let access = json!({
        "accessVersion": "0.1",
        "policyId": "p",
        "rules": [{
            "ruleId": "own_accounts",
            "actions": ["commerce.order.view"],
            "resourceType": "commerce.SalesOrder",
            "scope": { "dimensions": ["org", "tenant"] },
            "allOf": [{ "rel": { "from": "principal", "role": "accounts" } }]
        }]
    });
    let policies = compiled(&access.to_string());
    assert!(
        policies
            .to_cedar()
            .contains(r#"(principal has "accounts" && resource in principal["accounts"])"#)
    );
    let view = |accounts: Vec<&'static str>| Case {
        action: "commerce.order.view",
        principal_roles: vec![],
        accounts,
        ..Case::approve()
    };
    assert_eq!(view(vec!["so1"]).decide(&policies), Decision::Allow);
    assert_eq!(view(vec!["so2"]).decide(&policies), Decision::Deny);
    assert_eq!(view(vec![]).decide(&policies), Decision::Deny);
}

mod pdp {
    //! The PDP's lowering of Norn-typed inputs (roles, link attributes, typed
    //! context) against the order fixture.

    use std::collections::BTreeMap;

    use crate::{
        AccessRequest, ActorKind, AttrValue, AuthzSchema, CapabilityGrant, ConjunctOutcome,
        Delegation, DenyReason, EntityRef, Pdp, PdpDecision, PrincipalInput, ResourceInput,
        ScopeKey, compile,
    };

    use super::{MARKING_SCHEMA, ORDER_ACCESS, PROJECTED, compiled};

    fn scope() -> ScopeKey {
        ScopeKey {
            org: "o1".to_owned(),
            tenant: Some("t1".to_owned()),
            workspace: None,
            environment: Some("prod".to_owned()),
            business_unit: None,
        }
    }

    fn pdp() -> Pdp {
        let grant = CapabilityGrant {
            subject_id: "alice".to_owned(),
            actor_kind: ActorKind::User,
            agent_session_id: None,
            scope: Some(scope()),
            capabilities: vec!["order.approve".to_owned()],
            binding: None,
        };
        Pdp::new(compiled(ORDER_ACCESS), &[grant]).expect("pdp")
    }

    fn approve(roles: &[&str], mfa: bool) -> AccessRequest {
        AccessRequest {
            principal: PrincipalInput {
                subject_id: "alice".to_owned(),
                actor_kind: ActorKind::User,
                agent_session_id: None,
                roles: roles.iter().map(|role| (*role).to_owned()).collect(),
                attributes: BTreeMap::new(),
            },
            scope: Some(scope()),
            capability: "order.approve".to_owned(),
            action_id: "commerce.order.approve".to_owned(),
            resource: ResourceInput {
                object_type: "commerce.SalesOrder".to_owned(),
                id: "so1".to_owned(),
                scope: scope(),
                attributes: BTreeMap::from([
                    (
                        "customer".to_owned(),
                        AttrValue::Entity(EntityRef::Actor("bob".to_owned())),
                    ),
                    ("watchers".to_owned(), AttrValue::Set(Vec::new())),
                    ("total".to_owned(), AttrValue::Long(10)),
                ]),
            },
            context: BTreeMap::from([
                ("mfa".to_owned(), AttrValue::Bool(mfa)),
                ("channel".to_owned(), AttrValue::String("web".to_owned())),
            ]),
            binding: None,
            delegation: Delegation::Direct,
        }
    }

    fn marking_pdp() -> Pdp {
        let access: crate::access::AccessIr = serde_json::from_value(serde_json::json!({
            "accessVersion": "0.1",
            "policyId": "marking_access",
            "rules": [{
                "ruleId": "reveal_contacts",
                "actions": ["norn.reveal"],
                "resourceType": "norn.Marking",
                "scope": { "dimensions": ["org", "tenant", "environment"] },
                "allOf": [{ "marking": { "anyOf": ["pii.contact"] } }]
            }, {
                "ruleId": "declassify_contacts",
                "actions": ["norn.declassify"],
                "resourceType": "norn.Marking",
                "scope": { "dimensions": ["org", "tenant", "environment"] },
                "allOf": [{ "marking": { "anyOf": ["pii.contact"] } }]
            }]
        }))
        .expect("marking AccessIR decodes");
        let schema =
            AuthzSchema::from_projected_cedarschema_fragments(&[PROJECTED, MARKING_SCHEMA])
                .expect("marking schema loads");
        let policies = compile(&schema, &access).expect("marking policy compiles");
        let grant = CapabilityGrant {
            subject_id: "alice".to_owned(),
            actor_kind: ActorKind::User,
            agent_session_id: None,
            scope: Some(scope()),
            capabilities: vec!["norn.reveal".to_owned(), "norn.declassify".to_owned()],
            binding: None,
        };
        Pdp::new(policies, &[grant]).expect("marking PDP")
    }

    #[test]
    fn roles_and_typed_context_flow_through_the_pdp() {
        let pdp = pdp();
        assert_eq!(
            pdp.decide(&approve(&["approver"], true)),
            PdpDecision::Allow
        );
        assert_eq!(
            pdp.decide(&approve(&["approver", "approver"], true)),
            PdpDecision::Allow
        );
        assert_eq!(
            pdp.decide(&approve(&["intruder"], true)),
            PdpDecision::Deny(DenyReason::Access)
        );
        assert_eq!(
            pdp.decide(&approve(&["approver"], false)),
            PdpDecision::Deny(DenyReason::Access)
        );
    }

    #[test]
    fn nonconforming_entities_deny() {
        let pdp = pdp();
        let mut request = approve(&["approver"], true);
        request.resource.attributes.remove("total");
        assert_eq!(
            pdp.decide(&request),
            PdpDecision::Deny(DenyReason::AccessError)
        );
        let mut request = approve(&["approver"], true);
        request
            .context
            .insert("mfa".to_owned(), AttrValue::String("yes".to_owned()));
        assert_eq!(
            pdp.decide(&request),
            PdpDecision::Deny(DenyReason::AccessError)
        );
    }

    #[test]
    fn explanation_decision_matches_the_production_decision_for_each_short_circuit() {
        let pdp = pdp();

        let allow = approve(&["approver"], true);
        let explanation = pdp.explain(&allow);
        assert_eq!(explanation.decision, pdp.decide(&allow));
        assert_eq!(explanation.decision, PdpDecision::Allow);
        assert_eq!(explanation.scope, ConjunctOutcome::Allow);
        assert_eq!(explanation.capability, ConjunctOutcome::Allow);
        assert_eq!(explanation.delegation, ConjunctOutcome::Allow);
        assert_eq!(explanation.access, ConjunctOutcome::Allow);
        assert!(!explanation.determining_policies.is_empty());

        let deny = approve(&["intruder"], true);
        let explanation = pdp.explain(&deny);
        assert_eq!(explanation.decision, pdp.decide(&deny));
        assert_eq!(explanation.decision, PdpDecision::Deny(DenyReason::Access));
        assert_eq!(explanation.access, ConjunctOutcome::Deny);

        let mut capability_miss = approve(&["approver"], true);
        capability_miss.capability = "order.read".to_owned();
        let explanation = pdp.explain(&capability_miss);
        assert_eq!(explanation.decision, pdp.decide(&capability_miss));
        assert_eq!(
            explanation.decision,
            PdpDecision::Deny(DenyReason::Capability)
        );
        assert_eq!(explanation.capability, ConjunctOutcome::Deny);
        assert_eq!(explanation.delegation, ConjunctOutcome::NotEvaluated);

        let mut error = approve(&["approver"], true);
        error.resource.attributes.remove("total");
        let explanation = pdp.explain(&error);
        assert_eq!(explanation.decision, pdp.decide(&error));
        assert_eq!(
            explanation.decision,
            PdpDecision::Deny(DenyReason::AccessError)
        );
        assert_eq!(explanation.access, ConjunctOutcome::Error);
        assert!(!explanation.diagnostics.is_empty());

        let delegated = AccessRequest {
            delegation: Delegation::Biscuit("token".to_owned()),
            ..allow.clone()
        };
        let explanation = pdp.explain(&delegated);
        assert_eq!(explanation.decision, pdp.decide(&delegated));
        assert_eq!(
            explanation.decision,
            PdpDecision::Deny(DenyReason::Delegation)
        );
        assert_eq!(explanation.delegation, ConjunctOutcome::Deny);
        assert_eq!(explanation.access, ConjunctOutcome::NotEvaluated);

        let absent_scope = AccessRequest {
            scope: None,
            ..allow.clone()
        };
        let explanation = pdp.explain(&absent_scope);
        assert_eq!(explanation.decision, pdp.decide(&absent_scope));
        assert_eq!(
            explanation.decision,
            PdpDecision::Deny(DenyReason::AbsentScope)
        );
        assert_eq!(explanation.scope, ConjunctOutcome::Deny);
        assert_eq!(explanation.capability, ConjunctOutcome::NotEvaluated);

        let mut invalid = allow.clone();
        invalid
            .context
            .insert("scope".to_owned(), AttrValue::String("forged".to_owned()));
        let explanation = pdp.explain(&invalid);
        assert_eq!(explanation.decision, pdp.decide(&invalid));
        assert_eq!(
            explanation.decision,
            PdpDecision::Deny(DenyReason::InvalidRequest)
        );
        assert_eq!(explanation.scope, ConjunctOutcome::Error);

        let mut out_of_scope = allow;
        out_of_scope.resource.scope.tenant = Some("other".to_owned());
        let explanation = pdp.explain(&out_of_scope);
        assert_eq!(explanation.decision, pdp.decide(&out_of_scope));
        assert_eq!(explanation.decision, PdpDecision::Deny(DenyReason::Access));
        assert_eq!(explanation.access, ConjunctOutcome::Deny);
    }

    #[test]
    fn marking_decisions_use_exact_scoped_capability_and_access_condition() {
        let pdp = marking_pdp();
        let request = approve(&["approver"], true);
        let allowed = pdp.explain_marking(&request, "pii.contact", "norn.reveal");
        assert_eq!(allowed.decision, PdpDecision::Allow);
        assert_eq!(allowed.access, ConjunctOutcome::Allow);
        assert_eq!(
            pdp.decide_marking(&request, "pii.health", "norn.reveal"),
            PdpDecision::Deny(DenyReason::Access),
        );
        assert_eq!(
            pdp.decide_marking(&request, "pii.health", "norn.declassify"),
            PdpDecision::Deny(DenyReason::Access),
        );
        assert_eq!(
            pdp.decide_marking(&request, "pii.contact", "norn.declassify"),
            PdpDecision::Allow,
        );
        assert_eq!(
            pdp.decide_marking(&request, "pii.contact", "norn.read"),
            PdpDecision::Deny(DenyReason::InvalidRequest),
        );

        for id in [
            "PII/Contact",
            "PII.contact",
            "pii..contact",
            ".pii",
            "pii.",
            "1pii",
            "pii.1contact",
            "pii-contact",
            "pii.é",
            "pii contact",
        ] {
            assert_eq!(
                pdp.decide_marking(&request, id, "norn.reveal"),
                PdpDecision::Deny(DenyReason::InvalidRequest),
                "{id}"
            );
            assert_eq!(
                pdp.decide_marking(&request, id, "norn.declassify"),
                PdpDecision::Deny(DenyReason::InvalidRequest),
                "{id}"
            );
        }

        let mut out_of_scope = request;
        out_of_scope.resource.scope.tenant = Some("other".to_owned());
        assert_eq!(
            pdp.decide_marking(&out_of_scope, "pii.contact", "norn.reveal"),
            PdpDecision::Deny(DenyReason::Access),
        );
    }

    #[test]
    fn explanation_maps_slash_containing_cedar_ids_to_source_access_rule_ids() {
        let mut access: serde_json::Value =
            serde_json::from_str(ORDER_ACCESS).expect("fixture JSON parses");
        access["policyId"] = serde_json::json!("policy/part");
        access["rules"][1]["ruleId"] = serde_json::json!("rule/part");
        let access: crate::access::AccessIr =
            serde_json::from_value(access).expect("AccessIR decodes");
        let policies = crate::compile(&super::schema(), &access).expect("policies compile");
        let grant = CapabilityGrant {
            subject_id: "alice".to_owned(),
            actor_kind: ActorKind::User,
            agent_session_id: None,
            scope: Some(scope()),
            capabilities: vec!["order.approve".to_owned()],
            binding: None,
        };
        let pdp = Pdp::new(policies, &[grant]).expect("pdp");

        let explanation = pdp.explain(&approve(&["approver"], true));
        let determining = explanation
            .determining_policies
            .iter()
            .find(|policy| policy.cedar_policy_id == "policy/part/rule/part")
            .expect("matching policy contributes to the allow");
        assert_eq!(determining.policy_id.as_deref(), Some("policy/part"));
        assert_eq!(determining.rule_id.as_deref(), Some("rule/part"));
    }

    #[test]
    fn cedar_error_denies_but_preserves_the_other_permits_reason() {
        use crate::admit::{CompiledPolicy, admit};

        let head = concat!(
            "permit (\n",
            "  principal is Norn::Actor,\n",
            "  action in [Norn::Action::\"commerce.order.approve\"],\n",
            "  resource is commerce::SalesOrder\n",
            ")",
        );
        let scope_condition = concat!(
            "principal.scope == context.scope &&\n",
            "  resource.scope == context.scope &&\n",
            "  context.scope has tenant",
        );
        let witness = "Norn::ScopeWitness::\"scope\".scope";
        let scope_probe = format!(
            concat!(
                "{head}\nwhen {{\n",
                "  principal.scope == {witness} &&\n",
                "  resource.scope == {witness} &&\n",
                "  context.scope == {witness}\n",
                "}};\n",
            ),
            head = head,
            witness = witness,
        );
        let policy = |rule_id: &str, condition: &str| CompiledPolicy {
            id: format!("policy/{rule_id}"),
            source_policy_id: "policy".to_owned(),
            source_rule_id: rule_id.to_owned(),
            text: format!("{head}\nwhen {{\n  {condition}\n}};\n"),
            scope_probe: scope_probe.clone(),
            uses_entitlements: false,
        };
        let policies = admit(
            &super::schema(),
            vec![
                policy("always_allow", scope_condition),
                policy(
                    "overflow",
                    &format!("{scope_condition} && resource.total + 1 > 0"),
                ),
            ],
        )
        .expect("test policies pass strict admission");
        let grant = CapabilityGrant {
            subject_id: "alice".to_owned(),
            actor_kind: ActorKind::User,
            agent_session_id: None,
            scope: Some(scope()),
            capabilities: vec!["order.approve".to_owned()],
            binding: None,
        };
        let pdp = Pdp::new(policies, &[grant]).expect("pdp");
        let mut request = approve(&["approver"], true);
        request
            .resource
            .attributes
            .insert("total".to_owned(), AttrValue::Long(i64::MAX));

        let explanation = pdp.explain(&request);
        assert_eq!(
            explanation.decision,
            PdpDecision::Deny(DenyReason::AccessError)
        );
        assert_eq!(explanation.access, ConjunctOutcome::Error);
        assert!(!explanation.diagnostics.is_empty());
        assert!(explanation.determining_policies.iter().any(|policy| {
            policy.cedar_policy_id == "policy/always_allow"
                && policy.policy_id.as_deref() == Some("policy")
                && policy.rule_id.as_deref() == Some("always_allow")
        }));
    }
}
