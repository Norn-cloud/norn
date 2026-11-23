#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    missing_docs,
    reason = "decimal-binding zero-comparison admission tests"
)]

use std::collections::BTreeMap;

use norn_expr::{EnvOptions, Expression, Type, ZenTypeEnv};
use norn_ir::{PropertyId, UnitRef, UnitSystem, metric::DimensionPath};

fn binding_env() -> ZenTypeEnv {
    let fields = BTreeMap::from([
        (
            "quantity".into(),
            Type::DecimalBinding {
                group: "mass".into(),
            },
        ),
        (
            "other_mass".into(),
            Type::DecimalBinding {
                group: "mass".into(),
            },
        ),
        (
            "volume".into(),
            Type::DecimalBinding {
                group: "volume".into(),
            },
        ),
        (
            "grams".into(),
            Type::Decimal {
                scale: 3,
                unit: Some(UnitRef {
                    system: UnitSystem::Ucum,
                    code: "g".into(),
                }),
                currency_source: None,
                per_unit: None,
                rounding: norn_ir::Rounding::Refuse,
            },
        ),
        (
            "unresolved_money".into(),
            Type::Decimal {
                scale: 2,
                unit: None,
                currency_source: Some(DimensionPath {
                    links: Vec::new(),
                    property: PropertyId("currency".into()),
                }),
                per_unit: None,
                rounding: norn_ir::Rounding::Refuse,
            },
        ),
        ("currency".into(), Type::enumeration(["USD", "EUR"])),
        ("count".into(), Type::Integer),
        ("opaque".into(), Type::Json),
    ]);
    ZenTypeEnv::input_only(fields, EnvOptions::default()).expect("binding env")
}

#[test]
fn decimal_binding_admits_unit_invariant_comparisons_against_literal_zero() {
    let env = binding_env();
    for source in [
        "input.quantity > 0",
        "input.quantity >= 0",
        "input.quantity < 0",
        "input.quantity <= 0",
        "input.quantity == 0",
        "input.quantity != 0",
        "0 < input.quantity",
        "0 <= input.quantity",
        "0 != input.quantity",
        "input.quantity > 0.0",
        "input.quantity > 0.00",
        "input.quantity >= -0",
    ] {
        let checked = Expression::compile(source)
            .expect("compiles")
            .check_typed(&env, &Type::Bool);
        assert!(checked.is_ok(), "{source} should be admitted: {checked:?}");
    }
}

#[test]
fn decimal_binding_refuses_bound_vs_bound_same_group_operations_join_and_destination() {
    let env = binding_env();
    for source in [
        "input.quantity == input.other_mass",
        "input.quantity != input.other_mass",
        "input.quantity > input.other_mass",
        "input.quantity + input.other_mass > 0",
        "input.quantity - input.other_mass == 0",
        "input.quantity * input.other_mass > 0",
        "input.quantity / input.other_mass > 0",
    ] {
        let err = Expression::compile(source)
            .expect("compiles")
            .check_typed(&env, &Type::Bool)
            .expect_err(&format!("{source} must refuse Bound vs Bound"));
        assert!(
            err.iter().any(|d| d.code == "core.values.unit_mismatch"),
            "{source} expected core.values.unit_mismatch, got {err:?}"
        );
    }

    for source in [
        "(input.count > 0 ? input.quantity : input.other_mass) > 0",
        "(input.count > 0 ? input.quantity : 0) > 0",
    ] {
        let err = Expression::compile(source)
            .expect("compiles")
            .check_typed(&env, &Type::Bool)
            .expect_err(&format!("{source} must refuse Bound in join/ternary"));
        assert!(
            err.iter().any(|d| d.code == "expr.type.error"),
            "{source} expected expr.type.error from join refusal, got {err:?}"
        );
    }

    let bound_mass = Type::DecimalBinding {
        group: "mass".into(),
    };
    for (source, expected) in [
        ("input.quantity", &bound_mass),
        ("0", &bound_mass),
        ("input.quantity", &Type::Integer),
    ] {
        let err = Expression::compile(source)
            .expect("compiles")
            .check_typed(&env, expected)
            .expect_err(&format!(
                "{source} -> {expected:?} must refuse Bound destination"
            ));
        assert!(
            err.iter().any(|d| d.code == "core.values.unit_mismatch"),
            "{source} -> {expected:?} expected core.values.unit_mismatch, got {err:?}"
        );
    }
}

#[test]
fn decimal_binding_refuses_non_zero_bare_literals_mixed_groups_and_arithmetic_with_literals() {
    let env = binding_env();
    for source in [
        "input.quantity > 1",
        "input.quantity >= 1",
        "input.quantity == 1",
        "input.quantity != 0.5",
        "1 < input.quantity",
        "input.quantity > input.count",
        "input.quantity > input.volume",
        "input.quantity == input.grams",
        "input.quantity + 0 == 0",
        "input.quantity - 0 == 0",
        "input.quantity + 1 > 0",
        "input.quantity * 1 > 0",
        "input.quantity / 1 > 0",
        "input.quantity + input.volume > 0",
        "input.quantity + input.grams > 0",
    ] {
        let err = Expression::compile(source)
            .expect("compiles")
            .check_typed(&env, &Type::Bool)
            .expect_err(&format!("{source} must be refused"));
        assert!(
            err.iter().any(|d| d.code == "core.values.unit_mismatch"),
            "{source} expected core.values.unit_mismatch, got {err:?}"
        );
    }
}

#[test]
fn unresolved_currency_source_remains_fail_closed_even_against_literal_zero() {
    let env = binding_env();
    let err = Expression::compile("input.unresolved_money > 0")
        .expect("compiles")
        .check_typed(&env, &Type::Bool)
        .expect_err("unresolved currency source must fail closed against zero");
    assert!(
        err.iter().any(|d| d.code == "core.values.unit_mismatch"),
        "expected core.values.unit_mismatch, got {err:?}"
    );
}

#[test]
fn unbodied_or_non_scalar_json_property_remains_fail_closed_opaque() {
    let env = binding_env();
    let err = Expression::compile("input.opaque != null")
        .expect("compiles")
        .check_typed(&env, &Type::Bool)
        .expect_err("opaque json read must fail closed");
    assert!(
        err.iter().any(|d| d.code == "expr.read.opaque"),
        "expected expr.read.opaque, got {err:?}"
    );
}
