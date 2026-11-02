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
        "abs(input.x) > 0",
        "input['x' + 'y'] == 0",
        "input == input",
        "input.state < 'open'",
    ] {
        assert!(linear_guard(expression, &env()).is_err(), "{expression}");
    }
    assert!(linear_guard("true or 1 / 0 > 0", &env()).is_err());
}
#[test]
fn linear_profile_refuses_strings_outside_the_smt_unicode_repertoire() {
    let boundary = linear_guard("'\u{2ffff}' == '\u{2ffff}'", &env()).expect("SMT character");
    assert!(boundary.predicate().contains("\\u{2ffff}"));
    for ch in ['\u{30000}', '\u{e0001}', '\u{10ffff}'] {
        let source = format!("'{ch}' == '{ch}'");
        let error = linear_guard(&source, &env()).expect_err("unsupported SMT character");
        assert!(error.to_string().contains("unsupported_predicate"));
    }
}
proptest::proptest! {
    #[test]
    fn generated_out_of_repertoire_strings_never_get_an_smt_predicate(code in 0x3_0000u32..=0x10_ffff) {
        let ch = char::from_u32(code).expect("Unicode scalar");
        let source = format!("'{ch}' == '{ch}'");
        let error = linear_guard(&source, &env()).expect_err("unsupported SMT character");
        proptest::prop_assert!(error.to_string().contains("unsupported_predicate"));
    }
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
