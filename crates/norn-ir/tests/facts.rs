//! Regression coverage for declared Datalog fact projections and explicit gaps.

#![allow(clippy::expect_used, reason = "typed projection fixtures fail fast")]

use norn_ir::facts::{Basis, Definition, Gap, Predicate, Source, Value, project};
use norn_ir::{ActionRoot, ObjectTypeDef, ScopeRef};
use serde_json::json;

fn source() -> Source {
    Source {
        ir_version: "0.1".to_owned(),
        artifact_id: "acme.invoice".to_owned(),
        artifact_version: "2.3.0".to_owned(),
        scope: ScopeRef {
            org_id: Some("أكمي".to_owned()),
            tenant_id: Some("tenant-a".to_owned()),
            workspace_id: None,
            environment_id: Some("sandbox".to_owned()),
            business_unit_id: Some("cairo".to_owned()),
        },
    }
}

#[test]
fn object_fixture_preserves_names_scopes_and_declared_only_metadata() {
    let object: ObjectTypeDef = serde_json::from_value(json!({
        "name":"acme.Invoice", "extends":"core.Document",
        "properties":[{"id":"label","type":{"kind":"text"},"constraints":{"nullable":true}}, {"id":"status","type":{"kind":"enum","values":[{"value":"draft"},{"value":"posted"}]}}],
        "links":[{"id":"parent","target":"acme.Invoice","cardinality":{"to":"one","from":"many","required":true},"acyclic":true}],
        "lifecycles":[{"property":"status","initial":["draft"],"terminal":["posted"],"transitions":[{"id":"post","from":["draft"],"to":"posted","guards":["self.label != null"]}]}]
    })).expect("typed object");
    let projection = project(source(), Definition::ObjectType(&object));
    assert_eq!(projection.basis, Basis::Declared);
    assert_eq!(projection.source, source());
    assert_eq!(projection.facts.len(), 6);
    for fact in &projection.facts {
        assert_eq!(fact.values.len(), fact.predicate.arity());
        assert_eq!(fact.values.first(), Some(&Value::Scope(source().scope)));
    }
    let scope = Value::Scope(source().scope);
    assert_eq!(
        projection
            .facts
            .iter()
            .find(|fact| fact.predicate == Predicate::Link)
            .expect("link")
            .values,
        vec![
            scope.clone(),
            text("acme.Invoice"),
            text("parent"),
            text("acme.Invoice"),
            text("one"),
            text("many"),
            Value::Bool(true),
            Value::Bool(true)
        ]
    );
    assert_eq!(
        projection
            .facts
            .iter()
            .find(|fact| fact.predicate == Predicate::Transition)
            .expect("transition")
            .values,
        vec![
            scope,
            text("acme.Invoice"),
            text("status"),
            text("post"),
            text("draft"),
            text("posted")
        ]
    );
    assert!(projection.incomplete.contains(&Gap::EffectiveDefinitions));
    assert!(projection.incomplete.contains(&Gap::RecordCardinality));
    assert!(projection.incomplete.contains(&Gap::TypeDisjointness));
    assert!(projection.incomplete.contains(&Gap::Expressions));
    let bytes = norn_ir::canonical::canonical_json_of(&projection).expect("canonical facts");
    assert_eq!(
        serde_json::from_slice::<norn_ir::facts::Projection>(&bytes).expect("facts round trip"),
        projection
    );
}

fn text(value: &str) -> Value {
    Value::Text(value.to_owned())
}

#[test]
fn projection_does_not_validate_names_or_erase_duplicate_declarations() {
    let object: ObjectTypeDef = serde_json::from_value(json!({
        "name":"not a qualified name", "extends":"missing.Parent",
        "properties":[{"id":"same","type":{"kind":"boolean"}},{"id":"same","type":{"kind":"boolean"}}]
    })).expect("IR does not semantically validate");
    let projection = project(source(), Definition::ObjectType(&object));
    let fields: Vec<_> = projection
        .facts
        .iter()
        .filter(|fact| fact.predicate == Predicate::Field)
        .collect();
    assert_eq!(fields.len(), 2);
    assert_eq!(fields.first(), fields.last());
    assert!(
        projection
            .facts
            .iter()
            .any(|fact| fact.values.contains(&text("not a qualified name")))
    );
    assert!(
        projection
            .facts
            .iter()
            .any(|fact| fact.values.contains(&text("missing.Parent")))
    );
}

#[test]
fn action_fixture_distinguishes_declared_entity_writes_from_exact_transition_field() {
    let action: ActionRoot = serde_json::from_value(json!({
        "actionId":"invoice.post","scopeRequired":["orgId","tenantId"],"idempotent":true,"ledgered":true,"inputSchema":{},"outputSchema":{},
        "writes":[{"entityKind":"acme.Invoice","change":"modified"}],
        "effectProgram":{"kind":"fire_transition","objectIdField":"id","objectKind":"acme.Invoice","entityArtifactId":"acme.invoice","entityArtifactVersion":"2.3.0","lifecycle":"status","transition":"post"}
    })).expect("typed action");
    let projection = project(source(), Definition::Action(&action));
    assert_eq!(
        projection
            .facts
            .iter()
            .map(|fact| fact.predicate)
            .collect::<Vec<_>>(),
        vec![Predicate::Writes, Predicate::WritesField]
    );
    assert_eq!(
        projection
            .facts
            .iter()
            .find(|fact| fact.predicate == Predicate::WritesField)
            .expect("field")
            .values,
        vec![
            Value::Scope(source().scope),
            text("invoice.post"),
            text("acme.Invoice"),
            text("status")
        ]
    );
    assert!(projection.incomplete.contains(&Gap::FieldWrites));
}

#[test]
fn mapping_fixture_retains_declared_source_and_target_without_inferring_exactness() {
    let mapping = serde_json::from_value(json!({
        "format":"norn.mapping/1", "source":{"schemaId":"erp.schema","version":"1.0.0","sha256":"abc"}, "interfaces":["core.document"],
        "maps":[{"id":"invoice","source":{"dataset":"Sales Invoice"},"target":{"entity":"acme.invoice","interface":"core.document"},"when":"input.enabled","subject":"input.name","sourceRecord":"input.name"}]
    })).expect("typed mapping");
    let projection = project(source(), Definition::Mapping(&mapping));
    assert_eq!(
        projection.facts.first().expect("map").values,
        vec![
            Value::Scope(source().scope),
            text("invoice"),
            text("Sales Invoice"),
            text("acme.invoice"),
            text("core.document")
        ]
    );
    assert!(projection.incomplete.contains(&Gap::MappingSemantics));
}

#[test]
fn policy_and_law_metadata_do_not_infer_role_permissions_or_prohibitions() {
    let policy = serde_json::from_value(json!({
        "accessVersion":"0.1","policyId":"invoice.access","rules":[{"ruleId":"approve","actions":["invoice.post"],"resourceType":"acme.Invoice","scope":{"dimensions":["org","tenant"]},"allOf":[{"role":{"anyOf":["manager"]}},{"context":{"name":"approved","op":"eq","value":true}}]}]
    })).expect("typed policy");
    let law = serde_json::from_value(json!({
        "lawVersion":"0.1","id":"invoice.never_unapproved","owner":"acme","severity":"error","rationale":"reviewed","provenance":{"source":"system"},
        "scope":{"root":{"kind":"app","id":"billing"},"objectTypes":["acme.Invoice"]},"subject":"acme.Invoice","condition":"self.approved == false","modality":{"kind":"never"},"response":{"kind":"action","artifactId":"invoice.post"}
    })).expect("typed law");
    for (definition, predicate, gap) in [
        (Definition::Policy(&policy), Predicate::Policy, Gap::RoleCan),
        (Definition::Law(&law), Predicate::Law, Gap::LawForbids),
    ] {
        let projection = project(source(), definition);
        assert_eq!(projection.facts.len(), 1);
        assert_eq!(
            projection.facts.first().expect("identity").predicate,
            predicate
        );
        assert!(projection.incomplete.contains(&gap));
    }
}
