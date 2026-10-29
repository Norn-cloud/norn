use serde_json::json;
use zen_expression::intellisense::IntelliSense;
use zen_expression::variable::VariableType;
use zen_expression::{Isolate, Variable};

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
use wasm_bindgen_test::wasm_bindgen_test as test;

fn eval(
    expression: &str,
    context: serde_json::Value,
) -> Result<Variable, zen_expression::IsolateError> {
    Isolate::with_environment(context.into()).run_standard(expression)
}

#[test]
fn tokens_return_bounded_ascii_runs() {
    assert_eq!(
        eval("tokens(s, 'alnum', 256, 256)", json!({ "s": "AéB_9" })).unwrap(),
        Variable::from(json!(["A", "B", "9"]))
    );
    assert_eq!(
        eval("tokens(s, 'alnum', 2, 1)", json!({ "s": "alpha-42-z" })).unwrap(),
        Variable::from(json!(["a", "4"]))
    );
    assert_eq!(
        eval("tokens(s, 'alnum', 0, 256)", json!({ "s": "alpha" })).unwrap(),
        Variable::from(json!([]))
    );
    assert_eq!(
        eval("tokens(s, 'alnum', 256, 0)", json!({ "s": "alpha" })).unwrap(),
        Variable::from(json!([]))
    );

    let long_token = "x".repeat(300);
    let truncated = eval("tokens(s, 'alnum', 256, 256)", json!({ "s": long_token })).unwrap();
    let truncated_tokens = truncated.as_array().unwrap();
    assert_eq!(truncated_tokens.borrow()[0].as_str().unwrap().len(), 256);

    let many_tokens = "a-".repeat(300);
    let capped = eval("tokens(s, 'alnum', 256, 256)", json!({ "s": many_tokens })).unwrap();
    assert_eq!(capped.as_array().unwrap().borrow().len(), 256);
}

#[test]
fn tokens_leave_nfc_normalization_to_the_caller() {
    let composed = eval("tokens(s, 'alnum', 256, 256)", json!({ "s": "AéB" })).unwrap();
    let decomposed = eval("tokens(s, 'alnum', 256, 256)", json!({ "s": "Ae\u{301}B" })).unwrap();
    assert_eq!(composed, Variable::from(json!(["A", "B"])));
    assert_eq!(decomposed, Variable::from(json!(["Ae", "B"])));
}

#[test]
fn take_counts_unicode_scalars_and_preserves_original_bytes() {
    assert_eq!(
        eval("take(s, 2)", json!({ "s": "a😀éz" })).unwrap(),
        Variable::from(json!("a😀"))
    );
    assert_eq!(
        eval("take(s, 0)", json!({ "s": "a😀éz" })).unwrap(),
        Variable::from(json!(""))
    );
    assert_eq!(
        eval("take(s, 2)", json!({ "s": "e\u{301}x" })).unwrap(),
        Variable::from(json!("e\u{301}"))
    );
}

#[test]
fn join_is_linear_and_bounded() {
    assert_eq!(
        eval("join(parts, '-')", json!({ "parts": ["AB", "42"] })).unwrap(),
        Variable::from(json!("AB-42"))
    );
    assert_eq!(
        eval("join(parts, '-')", json!({ "parts": [] })).unwrap(),
        Variable::from(json!(""))
    );

    let at_cap = json!({ "parts": ["x".repeat(8_192), "y".repeat(8_191)] });
    assert_eq!(
        eval("len(join(parts, '-'))", at_cap)
            .unwrap()
            .as_number()
            .unwrap(),
        rust_decimal::Decimal::from(16_384)
    );
    let too_large = json!({ "parts": ["x".repeat(8_193), "y".repeat(8_192)] });
    assert!(eval("join(parts, '-')", too_large).is_err());
    assert!(eval("join([1], '-')", json!({})).is_err());
}

#[test]
fn bounded_string_builtins_appear_in_completion_scope() {
    let mut intellisense = IntelliSense::new();
    let completions = intellisense.completions("", 0, &VariableType::Any);
    let labels: Vec<_> = completions
        .iter()
        .map(|entry| entry.label.as_str())
        .collect();
    for builtin in ["tokens", "take", "join"] {
        assert!(
            labels.contains(&builtin),
            "missing completion for {builtin}"
        );
    }
}

#[test]
fn dynamic_caps_are_checked_and_never_coerced() {
    for expression in [
        "tokens(s, 'letters', 1, 1)",
        "tokens(s, 'alnum', 1.5, 1)",
        "tokens(s, 'alnum', -1, 1)",
        "tokens(s, 'alnum', 257, 1)",
        "tokens(s, 'alnum', 1, 257)",
        "take(s, 1.5)",
        "take(s, -1)",
        "take(s, 16385)",
    ] {
        assert!(
            eval(expression, json!({ "s": "abc" })).is_err(),
            "{expression}"
        );
    }
}
