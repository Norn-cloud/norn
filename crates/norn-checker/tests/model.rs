//! `ModelIR` (#1171 M1): a model is admitted only when its payload is
//! well-formed, its trust is backed by contracts that fit its outputs, and its
//! input type and features resolve in the set or the installed base; a named
//! predictions type (M4) must hold every output with the same type and
//! nullability, plus `subject` and `model_version`.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration scenarios"
)]
use norn_checker::{EntityArtifactSetContext, check_artifact, check_artifact_set};
use norn_ir::ArtifactEnvelope;
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn envelope(id: &str, kind: &str, payload: &Value) -> ArtifactEnvelope {
    let (namespace, name) = id.split_once('.').expect("qualified id");
    serde_json::from_value(json!({
        "irVersion": "0.1", "artifactId": id, "kind": kind, "namespace": namespace,
        "name": name, "version": "1.0.0", "lifecycle": "draft", "scope": {},
        "dependencies": [], "provenance": { "source": "system" }, "payload": payload,
    }))
    .expect("envelope")
}

fn entity(id: &str, object_type: &Value) -> ArtifactEnvelope {
    envelope(
        id,
        "entity",
        &json!({ "entityVersion": "2.0", "objectType": object_type }),
    )
}

/// `sales.Account`, the target of the customer's `account` link.
fn account() -> ArtifactEnvelope {
    entity(
        "sales.Account.entity",
        &json!({ "name": "sales.Account", "properties": [ { "id": "code", "type": { "kind": "text" } } ] }),
    )
}

/// `sales.Customer`: one property of every admitted kind, a timestamp, a
/// shared `ref` and a link.
fn customer() -> ArtifactEnvelope {
    entity(
        "sales.Customer.entity",
        &json!({
            "name": "sales.Customer",
            "properties": [
                { "id": "tenure_months", "type": { "kind": "integer" } },
                { "id": "monthly_spend", "type": { "kind": "decimal", "scale": 2 } },
                { "id": "plan", "type": { "kind": "enum", "values": [ { "value": "basic" }, { "value": "pro" } ] } },
                { "id": "region", "type": { "kind": "text" } },
                { "id": "churned", "type": { "kind": "boolean" } },
                { "id": "signed_up", "type": { "kind": "timestamp", "timezone": "utc" } }
            ],
            "links": [
                { "id": "account", "target": "sales.Account", "cardinality": { "to": "one", "from": "many" } }
            ]
        }),
    )
}

fn churn_payload() -> Value {
    json!({
        "modelId": "sales.churn_model",
        "title": "Churn risk",
        "version": "1.0.0",
        "input": { "kind": "objectType", "objectType": "sales.Customer" },
        "features": ["tenure_months", "monthly_spend", "plan", "region"],
        "target": "churned",
        "outputs": [
            { "id": "churn_probability", "type": { "kind": "decimal", "scale": 4 }, "nullable": false },
            { "id": "risk_band", "type": { "kind": "enum", "values": [ { "value": "low" }, { "value": "high" } ] }, "nullable": true },
            { "id": "will_churn", "type": { "kind": "boolean" }, "nullable": false }
        ],
        "trust": "contract_checked",
        "contracts": [
            { "id": "probability", "output": "churn_probability", "rule": { "kind": "range", "min": "0", "max": "1" } },
            { "id": "finite", "output": "churn_probability", "rule": { "kind": "finite" } },
            { "id": "band", "output": "risk_band", "rule": { "kind": "oneOf", "values": ["low", "high"] } },
            { "id": "decided", "output": "will_churn", "rule": { "kind": "notNull" } }
        ],
        "method": { "label": "logistic regression", "version": "sklearn 1.6" }
    })
}

fn model(payload: &Value) -> ArtifactEnvelope {
    envelope("sales.churn_model.model", "model", payload)
}

/// `churn_payload` with `pointer` replaced by `value`.
fn with(pointer: &str, value: Value) -> Value {
    let mut payload = churn_payload();
    *payload.pointer_mut(pointer).expect("member") = value;
    payload
}

fn codes(findings: &[norn_checker::Diagnostic]) -> BTreeSet<String> {
    findings
        .iter()
        .filter(|finding| finding.is_error())
        .map(|finding| finding.code.as_str().to_owned())
        .collect()
}

fn local_codes(payload: &Value) -> BTreeSet<String> {
    codes(&check_artifact(&model(payload)))
}

fn set_codes(candidates: &[ArtifactEnvelope], base: &[ArtifactEnvelope]) -> BTreeSet<String> {
    codes(&check_artifact_set(
        candidates,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base,
            tenant: None,
        },
    ))
}

/// Only the model's own findings, so a scenario names exactly one refusal.
fn model_set_codes(candidates: &[ArtifactEnvelope], base: &[ArtifactEnvelope]) -> BTreeSet<String> {
    set_codes(candidates, base)
        .into_iter()
        .filter(|code| code.starts_with("model."))
        .collect()
}

fn one(code: &str) -> BTreeSet<String> {
    BTreeSet::from([code.to_owned()])
}

#[test]
fn a_contract_checked_model_over_its_type_is_admitted() {
    assert_eq!(local_codes(&churn_payload()), BTreeSet::new());
    assert_eq!(
        set_codes(&[account(), customer(), model(&churn_payload())], &[]),
        BTreeSet::new()
    );
}

#[test]
fn an_installed_input_type_resolves_and_an_opaque_model_needs_no_contract() {
    let mut payload = with("/trust", json!("opaque"));
    let object = payload.as_object_mut().expect("payload object");
    object.remove("contracts");
    object.remove("target");
    assert_eq!(local_codes(&payload), BTreeSet::new());
    assert_eq!(
        set_codes(&[model(&payload)], &[account(), customer()]),
        BTreeSet::new()
    );
}

#[test]
fn inherited_extension_and_shared_ref_properties_are_features() {
    let base = entity(
        "sales.Party.entity",
        &json!({ "name": "sales.Party", "properties": [ { "id": "score", "type": { "kind": "integer" } } ] }),
    );
    let lead = entity(
        "sales.Lead.entity",
        &json!({
            "name": "sales.Lead", "extends": "sales.Party",
            "properties": [ { "id": "grade", "type": { "kind": "ref", "name": "sales.grade" } } ]
        }),
    );
    let grades = envelope(
        "sales.grades.entity",
        "entity",
        &json!({ "entityVersion": "2.0", "propertyTypes": [
            { "name": "sales.grade", "type": { "kind": "enum", "values": [ { "value": "a" }, { "value": "b" } ] } }
        ] }),
    );
    let payload = json!({
        "modelId": "sales.lead_model", "title": "Lead", "version": "0.1.0",
        "input": { "kind": "objectType", "objectType": "sales.Lead" },
        "features": ["score", "grade"],
        "outputs": [ { "id": "won", "type": { "kind": "boolean" }, "nullable": false } ],
        "trust": "opaque", "method": { "label": "rules" }
    });
    assert_eq!(
        model_set_codes(&[lead, model(&payload)], &[base, grades]),
        BTreeSet::new()
    );
}

#[test]
fn an_unknown_input_type_is_refused() {
    let payload = with(
        "/input",
        json!({ "kind": "objectType", "objectType": "sales.Prospect" }),
    );
    assert_eq!(
        model_set_codes(&[account(), customer(), model(&payload)], &[]),
        one("model.admission.unknown_input")
    );
}

#[test]
fn an_unknown_feature_or_target_is_refused() {
    let feature = with("/features/3", json!("lifetime_value"));
    assert_eq!(
        model_set_codes(&[account(), customer(), model(&feature)], &[]),
        one("model.admission.unknown_feature")
    );
    let target = with("/target", json!("cancelled"));
    assert_eq!(
        model_set_codes(&[account(), customer(), model(&target)], &[]),
        one("model.admission.unknown_feature")
    );
}

#[test]
fn a_link_a_timestamp_or_a_text_target_is_not_an_admitted_kind() {
    for (pointer, value) in [
        ("/features/3", json!("account")),
        ("/features/3", json!("signed_up")),
        ("/target", json!("region")),
    ] {
        let mut payload = with(pointer, value);
        if pointer == "/target" {
            // `region` stops being a feature, so only its kind is refused.
            payload["features"] = json!(["tenure_months", "monthly_spend", "plan"]);
        }
        assert_eq!(
            model_set_codes(&[account(), customer(), model(&payload)], &[]),
            one("model.admission.feature_kind"),
            "{pointer}"
        );
    }
}

#[test]
fn the_target_is_never_a_feature() {
    assert_eq!(
        local_codes(&with("/features/3", json!("churned"))),
        one("model.validation.target_in_features")
    );
}

#[test]
fn a_contract_must_fit_its_output() {
    for rule in [
        json!({ "kind": "range", "min": "0" }),
        json!({ "kind": "finite" }),
        json!({ "kind": "oneOf", "values": ["true"] }),
    ] {
        let payload = with(
            "/contracts/3",
            json!({ "id": "decided", "output": "will_churn", "rule": rule }),
        );
        assert_eq!(
            local_codes(&payload),
            one("model.validation.invalid_contract"),
            "{rule}"
        );
    }
    for (pointer, value) in [
        ("/contracts/0/rule", json!({ "kind": "range" })),
        (
            "/contracts/0/rule",
            json!({ "kind": "range", "min": "2", "max": "1" }),
        ),
        (
            "/contracts/2/rule",
            json!({ "kind": "oneOf", "values": ["medium"] }),
        ),
        ("/contracts/2/output", json!("segment")),
    ] {
        assert_eq!(
            local_codes(&with(pointer, value)),
            one("model.validation.invalid_contract"),
            "{pointer}"
        );
    }
}

#[test]
fn contract_checked_trust_needs_a_contract_and_native_trust_is_refused() {
    let mut payload = churn_payload();
    payload
        .as_object_mut()
        .expect("payload object")
        .remove("contracts");
    assert_eq!(
        local_codes(&payload),
        one("model.validation.trust_without_contract")
    );
    for trust in ["native", "proven"] {
        assert_eq!(
            local_codes(&with("/trust", json!(trust))),
            one("model.validation.unsupported_trust")
        );
    }
}

#[test]
fn ids_are_unique_and_well_formed() {
    let duplicate = with(
        "/outputs/2",
        json!({ "id": "churn_probability", "type": { "kind": "boolean" }, "nullable": false }),
    );
    assert!(local_codes(&duplicate).contains("model.validation.duplicate_id"));
    assert_eq!(
        local_codes(&with("/features/1", json!("tenure_months"))),
        one("model.validation.duplicate_id")
    );
    assert_eq!(
        local_codes(&with("/modelId", json!("churn"))),
        one("model.validation.invalid_id")
    );
    assert_eq!(
        local_codes(&with("/features", json!([]))),
        one("model.validation.invalid_payload")
    );
}

#[test]
fn an_output_is_an_inline_scalar_column() {
    for value in [
        json!({ "kind": "timestamp", "timezone": "utc" }),
        json!({ "kind": "ref", "name": "sales.grade" }),
        json!({ "kind": "decimal", "scale": 2, "unitSource": ["unit"] }),
    ] {
        // Opaque and without contracts, so only the column itself is refused.
        let mut payload = with("/outputs/0/type", value.clone());
        payload["trust"] = json!("opaque");
        payload
            .as_object_mut()
            .expect("payload object")
            .remove("contracts");
        assert_eq!(
            local_codes(&payload),
            one("model.validation.invalid_output"),
            "{value}"
        );
    }
}

#[test]
fn an_unknown_member_or_a_free_expression_is_refused() {
    let mut payload = churn_payload();
    payload
        .as_object_mut()
        .expect("payload object")
        .insert("script".into(), json!("return 1"));
    assert_eq!(
        local_codes(&payload),
        one("model.validation.invalid_payload")
    );
    assert_eq!(
        local_codes(&with(
            "/contracts/0/rule",
            json!({ "kind": "expression", "source": "x > 0" })
        )),
        one("model.validation.invalid_payload")
    );
}

/// `sales.ChurnPrediction`: where `churn_payload`'s predictions are written,
/// with `property` replaced by `value` (or removed when `value` is null).
fn churn_prediction(property: Option<(&str, Value)>) -> ArtifactEnvelope {
    let mut properties = vec![
        json!({ "id": "prediction_id", "type": { "kind": "text" } }),
        json!({ "id": "subject", "type": { "kind": "text" } }),
        json!({ "id": "model_version", "type": { "kind": "text" } }),
        json!({ "id": "churn_probability", "type": { "kind": "decimal", "scale": 4 } }),
        json!({ "id": "risk_band", "type": { "kind": "enum", "values": [
            { "value": "low" }, { "value": "medium" }, { "value": "high" } ] },
            "constraints": { "nullable": true } }),
        json!({ "id": "will_churn", "type": { "kind": "boolean" } }),
    ];
    if let Some((id, value)) = property {
        properties.retain(|property| property["id"] != id);
        if !value.is_null() {
            properties.push(value);
        }
    }
    entity(
        "sales.ChurnPrediction.entity",
        &json!({ "name": "sales.ChurnPrediction", "properties": properties }),
    )
}

/// `churn_payload` writing its predictions to `predictions`.
fn predicting_to(predictions: Value) -> Value {
    let mut payload = churn_payload();
    payload["predictions"] = predictions;
    payload
}

fn with_predictions() -> Value {
    predicting_to(json!({ "objectType": "sales.ChurnPrediction" }))
}

#[test]
fn a_predictions_type_with_every_output_is_admitted() {
    let payload = with_predictions();
    assert_eq!(local_codes(&payload), BTreeSet::new());
    assert_eq!(
        set_codes(
            &[
                account(),
                customer(),
                churn_prediction(None),
                model(&payload)
            ],
            &[]
        ),
        BTreeSet::new()
    );
    // Installed, the type resolves from the base.
    assert_eq!(
        model_set_codes(
            &[model(&payload)],
            &[account(), customer(), churn_prediction(None)]
        ),
        BTreeSet::new()
    );
}

#[test]
fn a_predictions_type_that_does_not_fit_the_outputs_is_refused() {
    let cases = [
        ("subject", Value::Null),
        (
            "model_version",
            json!({ "id": "model_version", "type": { "kind": "integer" } }),
        ),
        ("will_churn", Value::Null),
        (
            "churn_probability",
            json!({ "id": "churn_probability", "type": { "kind": "decimal", "scale": 2 } }),
        ),
        (
            "churn_probability",
            json!({ "id": "churn_probability", "type": { "kind": "integer" } }),
        ),
        (
            "risk_band",
            json!({ "id": "risk_band", "type": { "kind": "enum", "values": [ { "value": "low" } ] },
                    "constraints": { "nullable": true } }),
        ),
        (
            "risk_band",
            json!({ "id": "risk_band", "type": { "kind": "enum", "values": [ { "value": "low" }, { "value": "high" } ] } }),
        ),
        (
            "will_churn",
            json!({ "id": "will_churn", "type": { "kind": "boolean" }, "constraints": { "nullable": true } }),
        ),
    ];
    for (property, value) in cases {
        let prediction = churn_prediction(Some((property, value.clone())));
        assert_eq!(
            model_set_codes(
                &[
                    account(),
                    customer(),
                    prediction,
                    model(&with_predictions())
                ],
                &[]
            ),
            one("model.admission.prediction_type_mismatch"),
            "{property}: {value}"
        );
    }
}

#[test]
fn an_unknown_or_malformed_predictions_type_is_refused() {
    let unknown = predicting_to(json!({ "objectType": "sales.Forecast" }));
    assert_eq!(
        model_set_codes(&[account(), customer(), model(&unknown)], &[]),
        one("model.admission.prediction_type_mismatch")
    );
    assert_eq!(
        local_codes(&predicting_to(json!({ "objectType": "forecast" }))),
        one("model.validation.invalid_id")
    );
    assert_eq!(
        local_codes(&predicting_to(
            json!({ "objectType": "sales.ChurnPrediction", "writer": "x" })
        )),
        one("model.validation.invalid_payload")
    );
}

/// PRIVACY-3: a marking catalog declaring `pii.contact` with `mask`.
fn markings(mask: &Value) -> ArtifactEnvelope {
    envelope(
        "sales.markings.entity",
        "entity",
        &json!({ "entityVersion": "2.0", "markings": [
            { "id": "pii.contact", "label": "Contact", "severity": "high", "category": "pii.contact", "mask": mask },
            { "id": "finance.internal", "label": "Internal", "severity": "low", "category": "finance", "mask": { "kind": "redacted" } }
        ] }),
    )
}

/// `sales.Customer` whose `region` is marked by `marking`, directly.
fn marked_customer(marking: &str) -> ArtifactEnvelope {
    let mut customer = customer();
    let mut payload: Value =
        serde_json::to_value(customer.payload.to_json_value()).expect("payload");
    payload["objectType"]["properties"][3]["markings"] = json!([marking]);
    customer = entity("sales.Customer.entity", &payload["objectType"]);
    customer
}

fn refusal(candidates: &[ArtifactEnvelope]) -> Option<String> {
    check_artifact_set(
        candidates,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &[],
            tenant: None,
        },
    )
    .into_iter()
    .find(|finding| finding.code.as_str() == "model.law.personal_data_never_trains")
    .map(|finding| finding.message)
}

#[test]
fn personal_data_never_trains_a_model_unless_pseudonymized_or_dropped() {
    let redacted = json!({ "kind": "redacted" });
    let message = refusal(&[
        account(),
        marked_customer("pii.contact"),
        markings(&redacted),
        model(&churn_payload()),
    ])
    .expect("a feature carrying pii.contact is refused");
    assert!(message.contains("sales.Customer.region"), "{message}");
    assert!(
        message.contains("`pii.contact` via sales.Customer.region"),
        "{message}"
    );
    assert!(
        message.contains("pseudonym") && message.contains("dropped"),
        "{message}"
    );
    // A pseudonymized marking passes: the model trains on pseudonyms.
    let pseudonym = json!({ "kind": "pseudonym", "subject": "Customer" });
    assert_eq!(
        refusal(&[
            account(),
            marked_customer("pii.contact"),
            markings(&pseudonym),
            model(&churn_payload())
        ]),
        None
    );
    // So does dropping the field.
    let dropped = with(
        "/features",
        json!(["tenure_months", "monthly_spend", "plan"]),
    );
    assert_eq!(
        refusal(&[
            account(),
            marked_customer("pii.contact"),
            markings(&redacted),
            model(&dropped)
        ]),
        None
    );
    // A marking outside the personal-data category is not this law's concern.
    assert_eq!(
        refusal(&[
            account(),
            marked_customer("finance.internal"),
            markings(&redacted),
            model(&churn_payload())
        ]),
        None
    );
}

#[test]
fn the_refusal_follows_mapping_lineage_from_a_classified_source_field() {
    let redacted = json!({ "kind": "redacted" });
    let source = envelope(
        "crm.source.source",
        "source_schema",
        &json!({ "fields": [
            { "dataset": "contacts", "name": "postal_region", "ordinal": 0, "nativeType": "varchar",
              "typeCandidate": { "type": "string", "origin": "declared" }, "markings": ["pii.contact"] }
        ] }),
    );
    let mapping = envelope(
        "crm.contacts.mapping",
        "mapping",
        &json!({ "format": "norn.mapping/1", "maps": [{
            "id": "customer", "source": { "dataset": "contacts" },
            "target": { "entity": "sales.Customer.entity" },
            "properties": [{ "property": "region", "value": "lower(input.postal_region)" }]
        }] }),
    );
    let candidates = [
        account(),
        customer(),
        markings(&redacted),
        source,
        mapping,
        model(&churn_payload()),
    ];
    let message = check_artifact_set(
        &candidates,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &[],
            tenant: None,
        },
    )
    .into_iter()
    .find(|finding| finding.code.as_str() == "model.law.personal_data_never_trains")
    .map(|finding| finding.message)
    .expect("lineage carries the source field's marking to the feature");
    assert!(
        message.contains("crm.source.source: contacts.postal_region → mapping crm.contacts.mapping → sales.Customer.region"),
        "{message}"
    );
}
