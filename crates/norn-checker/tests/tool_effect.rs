#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test crate"
)]

//! A tool binding's declared effect (#1099): queries take no dedup contract;
//! a command takes a provider key, a reconcile query, or both, and nothing
//! else; each refusal names its stable code and payload pointer.

use norn_checker::codes;
use norn_checker::tool_effect::{
    EFFECT_INVALID, EFFECT_KEY_FIXED, EFFECT_KEY_NOT_INPUT, EFFECT_NO_DEDUP_CONTRACT,
    EFFECT_QUERY_CONTRACT, EFFECT_RECONCILE_NOT_QUERY, Effect, check_effect, check_reconcile,
};
use serde_json::{Value, json};

/// A provider command's input: a request reference it deduplicates by, an amount and a note.
fn submit_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "requestRef": { "type": "string" },
            "amount": { "type": "integer" },
            "note": { "type": ["string"] },
            "tags": { "type": ["string", "null"] }
        },
        "required": ["requestRef", "amount"]
    })
}

fn codes_of(
    result: Result<Effect, Vec<norn_checker::tool_effect::EffectRefusal>>,
) -> Vec<(&'static str, String)> {
    result
        .expect_err("refused")
        .into_iter()
        .map(|refusal| (refusal.code, refusal.pointer))
        .collect()
}

#[test]
fn a_query_admits_with_no_contract_and_refuses_one() {
    let schema = submit_schema();
    assert_eq!(
        check_effect(&json!({ "kind": "query" }), &schema, &json!({})),
        Ok(Effect::Query)
    );
    assert_eq!(Effect::Query.kind(), "query");
    assert_eq!(
        codes_of(check_effect(
            &json!({ "kind": "query", "providerKey": "requestRef", "reconcile": "b" }),
            &schema,
            &json!({})
        )),
        vec![
            (EFFECT_QUERY_CONTRACT, "/effect/providerKey".to_owned()),
            (EFFECT_QUERY_CONTRACT, "/effect/reconcile".to_owned()),
        ]
    );
}

#[test]
fn a_command_admits_with_a_provider_key_a_reconcile_query_or_both() {
    let schema = submit_schema();
    let command = |provider_key: Option<&str>, reconcile: Option<&str>| Effect::Command {
        provider_key: provider_key.map(str::to_owned),
        reconcile: reconcile.map(str::to_owned),
    };
    assert_eq!(
        check_effect(
            &json!({ "kind": "command", "providerKey": "requestRef" }),
            &schema,
            &json!({})
        ),
        Ok(command(Some("requestRef"), None))
    );
    assert_eq!(
        check_effect(
            &json!({ "kind": "command", "reconcile": "status-binding" }),
            &schema,
            &json!({})
        ),
        Ok(command(None, Some("status-binding")))
    );
    // `["string"]` is a string input too.
    let both = check_effect(
        &json!({ "kind": "command", "providerKey": "note", "reconcile": "status-binding" }),
        &schema,
        &json!({ "amount": { "fixed": 5 } }),
    );
    assert_eq!(both, Ok(command(Some("note"), Some("status-binding"))));
    assert_eq!(both.expect("admitted").kind(), "command");
}

#[test]
fn a_command_without_a_dedup_contract_is_refused() {
    assert_eq!(
        codes_of(check_effect(
            &json!({ "kind": "command" }),
            &submit_schema(),
            &json!({})
        )),
        vec![(EFFECT_NO_DEDUP_CONTRACT, "/effect".to_owned())]
    );
}

#[test]
fn the_provider_key_is_an_own_string_input_the_binding_does_not_fix() {
    let schema = submit_schema();
    for field in ["missing", "amount", "tags", "constructor", "toString"] {
        assert_eq!(
            codes_of(check_effect(
                &json!({ "kind": "command", "providerKey": field }),
                &schema,
                &json!({})
            )),
            vec![(EFFECT_KEY_NOT_INPUT, "/effect/providerKey".to_owned())],
            "{field}"
        );
    }
    assert_eq!(
        codes_of(check_effect(
            &json!({ "kind": "command", "providerKey": "requestRef" }),
            &schema,
            &json!({ "requestRef": { "fixed": "r-1" } })
        )),
        vec![(EFFECT_KEY_FIXED, "/effect/providerKey".to_owned())]
    );
    // A schema without properties has no inputs to carry the key.
    assert_eq!(
        codes_of(check_effect(
            &json!({ "kind": "command", "providerKey": "requestRef" }),
            &json!({ "type": "object" }),
            &json!({})
        )),
        vec![(EFFECT_KEY_NOT_INPUT, "/effect/providerKey".to_owned())]
    );
}

#[test]
fn a_malformed_effect_is_refused_with_every_problem() {
    let schema = submit_schema();
    for effect in [
        json!(null),
        json!("query"),
        json!([]),
        json!({}),
        json!({ "kind": "write" }),
        json!({ "kind": 1 }),
    ] {
        let refused = codes_of(check_effect(&effect, &schema, &json!({})));
        assert_eq!(refused.len(), 1, "{effect}");
        assert_eq!(refused[0].0, EFFECT_INVALID, "{effect}");
    }
    assert_eq!(
        codes_of(check_effect(
            &json!({ "kind": "command", "providerKey": " ", "reconcile": 7, "retry": true }),
            &schema,
            &json!({})
        )),
        vec![
            (EFFECT_INVALID, "/effect/retry".to_owned()),
            (EFFECT_INVALID, "/effect/providerKey".to_owned()),
            (EFFECT_INVALID, "/effect/reconcile".to_owned()),
        ]
    );
}

#[test]
fn a_reconcile_binding_must_exist_and_be_a_query() {
    assert_eq!(
        check_reconcile("status-binding", Some(&Effect::Query)),
        Ok(())
    );
    let command = Effect::Command {
        provider_key: Some("requestRef".to_owned()),
        reconcile: None,
    };
    for target in [Some(&command), None] {
        let refused = check_reconcile("other", target).expect_err("refused");
        assert_eq!(
            (refused.code, refused.pointer.as_str()),
            (EFFECT_RECONCILE_NOT_QUERY, "/effect/reconcile")
        );
    }
}

#[test]
fn every_effect_code_is_registered() {
    for code in [
        EFFECT_INVALID,
        EFFECT_QUERY_CONTRACT,
        EFFECT_NO_DEDUP_CONTRACT,
        EFFECT_KEY_NOT_INPUT,
        EFFECT_KEY_FIXED,
        EFFECT_RECONCILE_NOT_QUERY,
    ] {
        assert!(codes::lookup(code).is_some(), "{code}");
    }
}
