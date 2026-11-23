use std::collections::{BTreeMap, BTreeSet};

use norn_expr::{EntityDefinitions, EnvOptions, Expression, Type, ZenTypeEnv};
use norn_ir::{
    InterfaceDef, InterfaceName, PropertyId, UnitRef, UnitSystem, metric::DimensionPath,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn compile(source: &str) -> TestResult<Expression> {
    Expression::compile(source).map_err(|err| format!("compile `{source}`: {err:?}").into())
}

fn assert_refused(env: &ZenTypeEnv, source: &str, expected: &Type, code: &str) -> TestResult {
    let Err(err) = compile(source)?.check_typed(env, expected) else {
        return Err(format!("{source} -> {expected:?} must refuse with {code}").into());
    };
    assert!(
        err.iter().any(|d| d.code == code),
        "{source} -> {expected:?} expected {code}, got {err:?}"
    );
    Ok(())
}

fn binding_env() -> TestResult<ZenTypeEnv> {
    let bound_mass = Type::DecimalBinding {
        group: "mass".into(),
    };
    let grams = Type::Decimal {
        scale: 3,
        unit: Some(UnitRef {
            system: UnitSystem::Ucum,
            code: "g".into(),
        }),
        currency_source: None,
        per_unit: None,
        rounding: norn_ir::Rounding::Refuse,
    };
    let unresolved_money = Type::Decimal {
        scale: 2,
        unit: None,
        currency_source: Some(DimensionPath {
            links: Vec::new(),
            property: PropertyId("currency".into()),
        }),
        per_unit: None,
        rounding: norn_ir::Rounding::Refuse,
    };
    let records = BTreeMap::from([(
        "Line".into(),
        BTreeMap::from([("quantity".into(), bound_mass.clone())]),
    )]);
    let fields = BTreeMap::from([
        ("quantity".into(), bound_mass.clone()),
        ("other_mass".into(), bound_mass.clone()),
        (
            "volume".into(),
            Type::DecimalBinding {
                group: "volume".into(),
            },
        ),
        ("quantities".into(), bound_mass.list()),
        ("lines".into(), Type::Record("Line".into()).list()),
        ("grams".into(), grams.clone()),
        ("grams_list".into(), grams.list()),
        ("unresolved_money".into(), unresolved_money.clone()),
        ("unresolved_moneys".into(), unresolved_money.list()),
        ("currency".into(), Type::enumeration(["USD", "EUR"])),
        ("count".into(), Type::Integer),
        ("opaque".into(), Type::Json),
    ]);
    ZenTypeEnv::input_only_with_records(records, fields, EnvOptions::default())
        .map_err(|err| format!("binding env: {err:?}").into())
}

#[test]
fn decimal_binding_admits_unit_invariant_comparisons_against_literal_zero() -> TestResult {
    let env = binding_env()?;
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
        let checked = compile(source)?.check_typed(&env, &Type::Bool);
        assert!(checked.is_ok(), "{source} should be admitted: {checked:?}");
    }
    Ok(())
}

#[test]
fn decimal_binding_refuses_bound_vs_bound_same_group_operations_join_and_destination() -> TestResult
{
    let env = binding_env()?;
    for source in [
        "input.quantity == input.other_mass",
        "input.quantity != input.other_mass",
        "input.quantity > input.other_mass",
        "input.quantity + input.other_mass > 0",
        "input.quantity - input.other_mass == 0",
        "input.quantity * input.other_mass > 0",
        "input.quantity / input.other_mass > 0",
    ] {
        assert_refused(&env, source, &Type::Bool, "core.values.unit_mismatch")?;
    }

    for source in [
        "(input.count > 0 ? input.quantity : input.other_mass) > 0",
        "(input.count > 0 ? input.quantity : 0) > 0",
    ] {
        assert_refused(&env, source, &Type::Bool, "expr.type.error")?;
    }

    let bound_mass = Type::DecimalBinding {
        group: "mass".into(),
    };
    for (source, expected) in [
        ("input.quantity", &bound_mass),
        ("0", &bound_mass),
        ("input.quantity", &Type::Integer),
    ] {
        assert_refused(&env, source, expected, "core.values.unit_mismatch")?;
    }
    Ok(())
}

#[test]
fn decimal_binding_refuses_non_zero_bare_literals_mixed_groups_and_arithmetic_with_literals()
-> TestResult {
    let env = binding_env()?;
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
        assert_refused(&env, source, &Type::Bool, "core.values.unit_mismatch")?;
    }
    Ok(())
}

#[test]
fn unresolved_currency_source_remains_fail_closed_even_against_literal_zero() -> TestResult {
    let env = binding_env()?;
    assert_refused(
        &env,
        "input.unresolved_money > 0",
        &Type::Bool,
        "core.values.unit_mismatch",
    )
}

#[test]
fn unbodied_or_non_scalar_json_property_remains_fail_closed_opaque() -> TestResult {
    let env = binding_env()?;
    assert_refused(
        &env,
        "input.opaque != null",
        &Type::Bool,
        "expr.read.opaque",
    )
}

#[test]
fn decimal_binding_refuses_sum_rounding_truncation_interval_and_temporal_bypasses() -> TestResult {
    let env = binding_env()?;
    for source in [
        "sum(input.quantities) > 0",
        "sum(map(input.lines, #.quantity)) > 0",
        "sum([input.quantity]) > 0",
        "sum(input.unresolved_moneys) > 0",
        "floor(input.quantity) == 0",
        "ceil(input.quantity) == 0",
        "round(input.quantity) == 0",
        "round(input.quantity, 2) == 0",
        "trunc(input.quantity) == 0",
        "trunc(input.quantity, 2) == 0",
        "round(input.quantity, 2, 'half_even') == 0",
        "floor(input.unresolved_money) == 0",
        "round(input.unresolved_money, 2, 'half_even') == 0",
        "input.quantity in [1..5]",
        "input.quantity in [0..5]",
        "add_days(date('2026-01-01'), input.quantity) == date('2026-01-01')",
    ] {
        assert_refused(&env, source, &Type::Bool, "core.values.unit_mismatch")?;
    }

    for source in [
        "min(input.quantities) > 0",
        "max(input.quantities) > 0",
        "avg(input.quantities) > 0",
        "median(input.quantities) > 0",
    ] {
        let Err(err) = compile(source)?.check_typed(&env, &Type::Bool) else {
            return Err(format!("{source} must be refused").into());
        };
        assert!(
            err.iter()
                .any(|d| d.code == "expr.construct.unsupported"
                    || d.code == "core.values.unit_mismatch"),
            "{source} expected unsupported or unit mismatch, got {err:?}"
        );
    }
    Ok(())
}

#[test]
fn decimal_binding_preserves_unit_invariant_unary_sign_and_abs_zero_checks() -> TestResult {
    let env = binding_env()?;
    for source in [
        "-input.quantity < 0",
        "-input.quantity <= 0",
        "+input.quantity > 0",
        "-(input.quantity) == 0",
        "round(input.grams, 2, 'half_even') > 0",
    ] {
        let checked = compile(source)?.check_typed(&env, &Type::Bool);
        assert!(checked.is_ok(), "{source} should be admitted: {checked:?}");
    }

    // `abs` is unit-invariant around zero (so unit algebra admits `abs(Bound) > 0`
    // without `core.values.unit_mismatch`, even though the exact scaled VM
    // currently restricts function calls to `len` and 3-arg `round`), while
    // `abs(Bound) > 1` still refuses with `core.values.unit_mismatch`.
    for source in [
        "abs(input.quantity) > 0",
        "abs(input.quantity) == 0",
        "abs(-input.quantity) >= 0",
        "sum(input.grams_list) > 0",
    ] {
        let Err(err) = compile(source)?.check_typed(&env, &Type::Bool) else {
            return Err(format!("{source} is outside exact scaled VM function set").into());
        };
        assert!(
            !err.iter().any(|d| d.code == "core.values.unit_mismatch"),
            "{source} must not fail unit algebra, got {err:?}"
        );
        assert!(
            err.iter().any(|d| d.code == "core.values.inexact"),
            "{source} expected only exact-function gate core.values.inexact, got {err:?}"
        );
    }

    assert_refused(
        &env,
        "abs(input.quantity) > 1",
        &Type::Bool,
        "core.values.unit_mismatch",
    )
}

fn interface_def(json: &str) -> TestResult<InterfaceDef> {
    Ok(serde_json::from_str(json)?)
}

fn interface_env(interfaces: &[InterfaceDef], self_interface: &str) -> TestResult<ZenTypeEnv> {
    let defs = EntityDefinitions {
        property_types: &[],
        interfaces,
        object_types: &[],
        extensions: &[],
    };
    ZenTypeEnv::from_artifact_set_for_checking(&defs, &BTreeSet::new(), EnvOptions::default())
        .map_err(|err| format!("interface env: {err:?}"))?
        .bind_interface_self(&InterfaceName(self_interface.to_owned()))
        .map_err(|err| format!("bind self: {err:?}").into())
}

fn test_interfaces() -> TestResult<[InterfaceDef; 5]> {
    Ok([
        interface_def(
            r#"{
                "name": "core.Conversion",
                "properties": [
                    {"id": "input", "type": {"kind": "decimal-binding", "group": "conversion"}},
                    {"id": "output", "type": {"kind": "decimal-binding", "group": "conversion"}},
                    {"id": "loss", "type": {"kind": "decimal-binding", "group": "conversion"}},
                    {"id": "flag", "type": {"kind": "boolean"}}
                ],
                "links": [
                    {"id": "other", "target": "core.Conversion", "cardinality": {"from": "many", "to": "one", "required": true}},
                    {"id": "others", "target": "core.Conversion", "cardinality": {"from": "many", "to": "many", "required": false}}
                ]
            }"#,
        )?,
        interface_def(
            r#"{
                "name": "toy.ChildConversion",
                "extends": ["core.Conversion"],
                "properties": [
                    {"id": "child_qty", "type": {"kind": "decimal-binding", "group": "conversion"}}
                ]
            }"#,
        )?,
        interface_def(
            r#"{
                "name": "toy.I1",
                "properties": [
                    {"id": "a", "type": {"kind": "decimal-binding", "group": "g"}},
                    {"id": "flag", "type": {"kind": "boolean"}}
                ],
                "links": [
                    {"id": "peer", "target": "toy.I2", "cardinality": {"from": "many", "to": "one", "required": true}}
                ]
            }"#,
        )?,
        interface_def(
            r#"{
                "name": "toy.I2",
                "properties": [
                    {"id": "b", "type": {"kind": "decimal-binding", "group": "g"}}
                ]
            }"#,
        )?,
        interface_def(
            r#"{
                "name": "toy.I3",
                "extends": ["toy.I1", "toy.I2"],
                "properties": []
            }"#,
        )?,
    ])
}

#[test]
fn interface_decimal_binding_admits_same_owner_same_instance_and_refuses_owner_mismatches()
-> TestResult {
    let all = test_interfaces()?;
    let conv_env = interface_env(&all, "core.Conversion")?;
    for source in [
        "self.input == self.output + self.loss",
        "self.input - self.output == self.loss",
        "self.input - self.output >= 0",
        "self.input >= self.output",
        "(self.flag ? self.input : self.output) == self.loss",
        "self.other.input == self.other.output + self.other.loss",
        "self.input > 0",
        "-self.input < 0",
    ] {
        let checked = compile(source)?.check_typed(&conv_env, &Type::Bool);
        assert!(checked.is_ok(), "{source} should be admitted: {checked:?}");
    }

    let child_env = interface_env(&all, "toy.ChildConversion")?;
    assert!(
        compile("self.input == self.output + self.loss")?
            .check_typed(&child_env, &Type::Bool)
            .is_ok(),
        "inherited same-declaring-interface fields on same instance must be admitted"
    );

    let i1_env = interface_env(&all, "toy.I1")?;
    let i3_env = interface_env(&all, "toy.I3")?;
    for (env, source) in [
        (&child_env, "self.input == self.child_qty"),
        (&child_env, "self.input + self.child_qty > 0"),
        (&child_env, "self.input - self.child_qty == 0"),
        (&i1_env, "self.a == self.peer.b"),
        (&i1_env, "self.a + self.peer.b > 0"),
        (&i1_env, "self.a - self.peer.b == 0"),
        (&i3_env, "self.a == self.b"),
        (&i3_env, "self.a + self.b > 0"),
        (&i3_env, "self.a - self.b == 0"),
        (&i3_env, "self.a > self.b"),
    ] {
        assert_refused(env, source, &Type::Bool, "core.values.unit_mismatch")?;
    }
    assert_refused(
        &i3_env,
        "(self.flag ? self.a : self.b) > 0",
        &Type::Bool,
        "expr.type.error",
    )
}

#[test]
fn interface_decimal_binding_refuses_cross_instance_sum_rounding_and_destinations() -> TestResult {
    let all = test_interfaces()?;
    let conv_env = interface_env(&all, "core.Conversion")?
        .bind_operand("a", Type::Record("core.Conversion".into()))
        .map_err(|err| format!("bind a: {err:?}"))?
        .bind_operand("b", Type::Record("core.Conversion".into()))
        .map_err(|err| format!("bind b: {err:?}"))?;
    for source in [
        "self.input == self.other.input",
        "self.input + self.other.input > 0",
        "self.input - self.other.output == 0",
        "a.input == b.input",
        "a.input + b.output > 0",
        "all(self.others, #.input == self.input)",
        "all(self.others as x, all(self.others as y, x.input == y.input))",
        "sum(map(self.others, #.input)) > 0",
        "sum([self.input, self.output]) > 0",
        "floor(self.input) == 0",
        "ceil(self.input) == 0",
        "round(self.input) == 0",
        "trunc(self.input) == 0",
        "round(self.input, 2, 'half_even') == 0",
        "self.input * self.output > 0",
        "self.input / self.output > 0",
    ] {
        assert_refused(&conv_env, source, &Type::Bool, "core.values.unit_mismatch")?;
    }
    assert_refused(
        &conv_env,
        "(self.flag ? self.input : self.other.input) > 0",
        &Type::Bool,
        "expr.type.error",
    )?;
    assert_refused(
        &conv_env,
        "0",
        &Type::DecimalBinding {
            group: "conversion".into(),
        },
        "core.values.unit_mismatch",
    )
}
