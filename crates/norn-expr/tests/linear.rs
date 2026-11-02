#![allow(
    clippy::expect_used,
    missing_docs,
    reason = "linear profile correspondence fixtures"
)]
use norn_expr::{EnvOptions, Type, ZenTypeEnv, linear_guard, linear_symbol};
use std::collections::BTreeMap;
fn env() -> ZenTypeEnv {
    ZenTypeEnv::input_only(
        BTreeMap::from([
            ("x".into(), Type::Number),
            ("y".into(), Type::Number),
            ("flag".into(), Type::Bool),
            (
                "state".into(),
                Type::Enum {
                    values: vec!["open".into(), "closed".into()],
                },
            ),
        ]),
        EnvOptions::default(),
    )
    .expect("typed inputs")
}
#[test]
fn linear_profile_retains_exact_literals_identity_and_boolean_guards() {
    let guard = linear_guard(
        "input.x + 0.10 <= 2 * input.y and input.state == 'open' and not input.flag",
        &env(),
    )
    .expect("linear guard");
    assert!(guard.predicate().contains("(/ 1 10)"));
    assert_eq!(guard.variables().len(), 4);
    assert!(guard.predicate().contains("\\u{6f}\\u{70}\\u{65}\\u{6e}"));
    assert_ne!(
        linear_symbol(&["self.a".into()]),
        linear_symbol(&["self".into(), "a".into()])
    );
}
#[test]
fn linear_profile_refuses_calls_nonlinearity_and_dynamic_or_opaque_operands() {
    for expression in [
        "input.x * input.y > 0",
        "input.x / input.y > 0",
        "input.x / 3 * 3 == 1",
        "input.x / 10 > 0",
        "abs(input.x) > 0",
        "input['x' + 'y'] == 0",
        "input == input",
        "input.state < 'open'",
    ] {
        assert!(linear_guard(expression, &env()).is_err(), "{expression}");
    }
    assert!(linear_guard("true or 1 / 0 > 0", &env()).is_err());
}
proptest::proptest! {
    #[test]
    fn every_generated_affine_guard_is_in_profile(a in proptest::num::i16::ANY, b in proptest::num::i16::ANY) {
        let source = format!("{a} * input.x + {b} <= input.y");
        proptest::prop_assert!(linear_guard(&source, &env()).is_ok());
    }
    #[test]
    fn path_symbols_preserve_segment_boundaries(a in "[a-z_\\.]{0,20}", b in "[a-z_\\.]{0,20}") {
        let split = vec![a.clone(), b.clone()];
        let joined = vec![format!("{a}.{b}")];
        proptest::prop_assert_ne!(linear_symbol(&split), linear_symbol(&joined));
    }
}

#[test]
fn linear_profile_keeps_type_diagnostics() {
    let error = linear_guard("input.missing == 1", &env()).expect_err("unknown field");
    assert!(error.to_string().contains("missing"));
}

#[test]
fn decimal_binding_requires_a_concrete_implementor_type() {
    for (ty, admitted) in [
        (Type::DecimalBinding { group: "mass".into() }, false),
        (Type::Decimal { scale: 3, unit: None }, true),
    ] {
        let env = ZenTypeEnv::input_only(BTreeMap::from([("x".into(), ty)]), EnvOptions::default()).expect("typed input");
        assert_eq!(linear_guard("input.x == input.x", &env).is_ok(), admitted);
    }
}
