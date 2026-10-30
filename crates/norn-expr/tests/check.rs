#![allow(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    missing_docs,
    reason = "integration-test crate"
)]
#![deny(
    clippy::match_wildcard_for_single_variants,
    clippy::format_collect,
    clippy::format_push_string,
    clippy::inefficient_to_string,
    clippy::large_types_passed_by_value,
    clippy::needless_pass_by_value,
    clippy::large_futures
)]

//! `check_expression` under the `norn-expr/4` allowlist against the closed
//! `norn.expr.env/1` environment (T1-EXPR-2, #2519): accepted expressions
//! with their proved types; must-reject fixtures with stable codes, including
//! every counterexample from both review rounds and the zen adoption review's
//! F2 `o[k] - 1`; and a generated soundness check (every accepted expression
//! evaluates to a value of its checked type on conforming inputs).

mod support;

use norn_expr::check::{CHECKER_PROFILE, codes};
use norn_expr::{
    ConstructionSource, EnvOptions, EvaluationBudget, Expression, InferredType, Number, Type,
    UnaryTest, Value, ZenTypeEnv, check_expression, check_expression_with,
    check_expression_with_sources, check_unary, check_unary_with,
};
use norn_ir::{ObjectTypeDef, ResourceLimitProfile, TextFormat, TimezonePolicy};
use support::{Model, env_for, fields, name, roomy_limits, slice_bytes, within_slice_limits};

fn check(
    env: &ZenTypeEnv,
    source: &str,
    expected: &Type,
) -> Result<InferredType, Vec<&'static str>> {
    let expr = Expression::compile(source).map_err(|_compile_error| vec![codes::SYNTAX])?;
    check_expression(&expr, env, expected)
        .map_err(|diagnostics| diagnostics.iter().map(|d| d.code).collect())
}

/// A type fixture: checked under a limit profile with unlimited decide fuel,
/// so it tests typing only. The cost bound is tested in `tests/cost.rs` and by
/// the generated soundness checks below, under `norn.limits/1`.
fn accepts(self_type: &str, source: &str, expected: &Type) -> InferredType {
    let expr = Expression::compile(source).unwrap_or_else(|err| panic!("{source}: {err}"));
    check_expression_with(&expr, &env_for(self_type), expected, &roomy_limits())
        .map(norn_expr::CheckedExpression::into_inferred)
        .unwrap_or_else(|diagnostics| {
            let codes: Vec<_> = diagnostics.iter().map(|d| d.code).collect();
            panic!("`{source}` should check, got {codes:?}")
        })
}

fn rejects(self_type: &str, source: &str, expected: &Type, code: &str) {
    match check(&env_for(self_type), source, expected) {
        Ok(ty) => panic!("`{source}` must be rejected with {code}, but checked as {ty}"),
        Err(found) => assert!(
            found.contains(&code),
            "`{source}`: expected {code}, got {found:?}"
        ),
    }
}

fn decimal(scale: u64) -> Type {
    Type::Decimal { scale, unit: None }
}

fn temporal_input_env() -> ZenTypeEnv {
    Model::retail()
        .env()
        .bind_input(fields([
            ("date_a", Type::Date),
            ("date_b", Type::Date),
            ("maybe_date", Type::Date.optional()),
            (
                "timestamp_utc",
                Type::Timestamp {
                    timezone: TimezonePolicy::Utc,
                },
            ),
            (
                "timestamp_offset",
                Type::Timestamp {
                    timezone: TimezonePolicy::Offset,
                },
            ),
            ("text", Type::text()),
            ("flag", Type::Bool),
        ]))
        .expect("bind temporal inputs")
}

const SKU: &str = "retail.Sku";
const ORDER: &str = "commerce.SalesOrder";

#[test]
fn profile_and_codes_are_registered() {
    assert_eq!(CHECKER_PROFILE, "norn-zen/6");
    let mut unique = codes::ALL.to_vec();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), codes::ALL.len());
    assert!(codes::ALL.contains(&codes::UNSUPPORTED_CONSTRUCT));
}

#[test]
fn construction_sources_own_every_member_and_use_byte_spans_after_unicode() {
    let expression = Expression::compile("({'label': 'λ', values: [(1), 1.5, (self.stock)]})")
        .expect("construction compiles");
    let env = env_for(SKU);
    let limits = roomy_limits();
    let (checked, construction) =
        check_expression_with_sources(&expression, &env, &Type::Json, &limits)
            .expect("construction checks");
    assert_eq!(
        checked,
        check_expression_with(&expression, &env, &Type::Json, &limits).expect("same legacy check")
    );
    assert_eq!(
        construction,
        Some(ConstructionSource::Record(
            [
                (
                    "label".to_owned(),
                    ConstructionSource::Scalar("'λ'".to_owned()),
                ),
                (
                    "values".to_owned(),
                    ConstructionSource::List(vec![
                        ConstructionSource::Scalar("1".to_owned()),
                        ConstructionSource::Scalar("1.5".to_owned()),
                        ConstructionSource::Scalar("self.stock".to_owned()),
                    ]),
                ),
            ]
            .into_iter()
            .collect()
        ))
    );
}

#[test]
fn construction_sources_preserve_arithmetic_without_claiming_an_integer_proof() {
    let expression = Expression::compile("((1 + 1))").expect("arithmetic compiles");
    let (_, construction) =
        check_expression_with_sources(&expression, &env_for(SKU), &Type::Number, &roomy_limits())
            .expect("arithmetic checks");
    assert_eq!(
        construction,
        Some(ConstructionSource::Scalar("1 + 1".to_owned()))
    );
    let duplicate = Expression::compile("{count: 1, count: 2}").expect("duplicate syntax compiles");
    assert!(
        check_expression_with_sources(&duplicate, &env_for(SKU), &Type::Json, &roomy_limits())
            .is_err()
    );
    let deep = format!("{}true", "not ".repeat(200));
    let expression = Expression::compile(&deep).expect("nested expression compiles");
    let found =
        check_expression_with_sources(&expression, &env_for(SKU), &Type::Bool, &roomy_limits())
            .expect_err("bounds apply before projecting constructions");
    assert!(
        found
            .iter()
            .any(|finding| finding.code == codes::BOUND_DEPTH)
    );
}

#[test]
fn well_typed_expressions_check() {
    let s = |v: &str| InferredType::Const(v.to_owned());
    let cases: &[(&str, &str, Type, InferredType)] = &[
        (SKU, "self.price * 2", decimal(2), InferredType::Number),
        (SKU, "self.price / 2", decimal(2), InferredType::Number),
        (SKU, "-self.price + 1", Type::Number, InferredType::Number),
        (
            SKU,
            "self.status == 'active'",
            Type::Bool,
            InferredType::Bool,
        ),
        (
            SKU,
            "self.stock > 0 and not self.is_taxable",
            Type::Bool,
            InferredType::Bool,
        ),
        (SKU, "self.weight ?? 0", Type::Number, InferredType::Number),
        (SKU, "self.weight == null", Type::Bool, InferredType::Bool),
        (SKU, "upper(self.name)", Type::text(), InferredType::String),
        (
            SKU,
            "self.launched_at",
            Type::text(),
            InferredType::Timestamp,
        ),
        (
            SKU,
            "self.launched_at",
            Type::Timestamp {
                timezone: TimezonePolicy::Utc,
            },
            InferredType::Timestamp,
        ),
        (
            SKU,
            "self.launch_date",
            Type::Json,
            InferredType::Optional(Box::new(InferredType::Date)),
        ),
        (
            SKU,
            "[self.launched_at, self.launched_at]",
            Type::Json,
            InferredType::List(Box::new(InferredType::Timestamp)),
        ),
        (
            SKU,
            "true ? self.launched_at : self.launched_at",
            Type::Json,
            InferredType::Timestamp,
        ),
        (
            SKU,
            "{at: self.launched_at}",
            Type::Json,
            InferredType::Record([(String::from("at"), InferredType::Timestamp)].into()),
        ),
        (SKU, "self['price']", decimal(2), InferredType::Number),
        (SKU, "len(self.name) >= 3", Type::Bool, InferredType::Bool),
        (
            SKU,
            "round(self.price, 1)",
            Type::Number,
            InferredType::Number,
        ),
        (SKU, "'a'", Type::text(), s("a")),
        (
            SKU,
            "self.stock > 0 ? 'in' : 'out'",
            Type::text(),
            InferredType::Enum(vec!["in".into(), "out".into()]),
        ),
        (
            ORDER,
            "self.customer.tier == 'gold'",
            Type::Bool,
            InferredType::Bool,
        ),
        (ORDER, "len(self.lines) > 0", Type::Bool, InferredType::Bool),
        (
            ORDER,
            "sum(map(self.lines, #.amount))",
            decimal(2),
            InferredType::Number,
        ),
        (
            ORDER,
            "count(self.lines, #.quantity > 1)",
            Type::Integer,
            InferredType::Number,
        ),
        (
            ORDER,
            "some(self.lines as line, line.quantity > 1)",
            Type::Bool,
            InferredType::Bool,
        ),
        (
            ORDER,
            "(self.customer.note ?? '') == ''",
            Type::Bool,
            InferredType::Bool,
        ),
        (
            ORDER,
            "map(self.customer.orders, #.customer.email)",
            Type::text().list(),
            InferredType::List(Box::new(InferredType::String)),
        ),
        (
            SKU,
            "{a: 1, b: [self.name]}",
            Type::Json,
            InferredType::Record(
                [
                    ("a".to_owned(), InferredType::Number),
                    (
                        "b".to_owned(),
                        InferredType::List(Box::new(InferredType::String)),
                    ),
                ]
                .into(),
            ),
        ),
    ];
    for (self_type, source, expected, inferred) in cases {
        assert_eq!(
            &accepts(self_type, source, expected),
            inferred,
            "`{source}`"
        );
    }
}

#[test]
fn temporal_kinds_survive_inference_and_comparison_checks() {
    let env = temporal_input_env();
    assert_eq!(
        check(&env, "input.date_a", &Type::text()),
        Ok(InferredType::Date),
        "date values may be projected to JSON string outputs without losing their kind"
    );
    assert_eq!(
        check(&env, "input.timestamp_utc", &Type::text()),
        Ok(InferredType::Timestamp)
    );
    assert_eq!(
        check(
            &env,
            "input.date_a",
            &Type::Text {
                format: Some(TextFormat::Uuid),
            }
        ),
        Err(vec![codes::RESULT_MISMATCH]),
        "temporal JSON string projection does not discard a declared text format"
    );
    assert_eq!(
        check(
            &env,
            "input.flag ? input.date_a : input.date_b",
            &Type::Json
        ),
        Ok(InferredType::Date)
    );
    assert_eq!(
        check(&env, "[input.date_a, input.date_b]", &Type::Json),
        Ok(InferredType::List(Box::new(InferredType::Date)))
    );
    assert_eq!(
        check(&env, "[input.date_a, input.date_b]", &Type::text().list()),
        Ok(InferredType::List(Box::new(InferredType::Date))),
        "a string-shaped output list must not erase semantic element kinds"
    );
    assert_eq!(
        check(
            &env,
            "{date: input.date_a, at: input.timestamp_utc}",
            &Type::Json
        ),
        Ok(InferredType::Record(
            [
                (String::from("at"), InferredType::Timestamp),
                (String::from("date"), InferredType::Date),
            ]
            .into()
        ))
    );
    assert_eq!(
        check(&env, "input.date_a == input.date_b", &Type::Bool),
        Ok(InferredType::Bool)
    );
    assert_eq!(
        check(
            &env,
            "input.timestamp_utc == input.timestamp_offset",
            &Type::Bool
        ),
        Ok(InferredType::Bool),
        "the declared timezone policy does not change timestamp instant equality"
    );

    for source in [
        "input.date_a == input.text",
        "input.date_a == input.timestamp_utc",
        "input.date_a < input.date_b",
        "input.date_a + ''",
        "len(input.timestamp_utc)",
        "input.flag ? input.date_a : input.text",
        "[input.date_a, input.text]",
        "([input.date_a, input.date_b])[0] == input.text",
        "input.maybe_date ?? input.text",
    ] {
        let expression = Expression::compile(source).expect("compile temporal expression");
        let diagnostics = check_expression(&expression, &env, &Type::Json)
            .expect_err("temporal/text mixing or temporal ordering must be refused");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == codes::TYPE_ERROR),
            "`{source}`: expected a temporal type error, got {diagnostics:?}"
        );
    }
}

#[test]
fn evaluator_compares_same_kind_temporal_wire_values() {
    let env = temporal_input_env();
    let source = "input.date_a == input.date_b and input.timestamp_utc == input.timestamp_offset";
    let expression = Expression::compile(source).expect("compile");
    check_expression(&expression, &env, &Type::Bool).expect("same-kind timestamp equality");
    let context = Value::object([(
        "input",
        Value::object([
            ("date_a", Value::from("2026-02-28")),
            ("date_b", Value::from("2026-02-28")),
            (
                "timestamp_utc",
                Value::from(
                    norn_expr::temporal::canonical_timestamp("2026-01-01T02:00:00+02:00")
                        .expect("canonicalize offset timestamp"),
                ),
            ),
            ("timestamp_offset", Value::from("2026-01-01T00:00:00Z")),
        ]),
    )]);
    let evaluation = expression
        .evaluate(&context, EvaluationBudget::default())
        .expect("evaluate canonical timestamp equality");
    assert_eq!(evaluation.value(), &Value::Bool(true));
}

#[test]
fn empty_lists_need_an_expected_type() {
    accepts(ORDER, "[]", &Type::Integer.list());
    accepts(SKU, "self.stock > 0 ? [] : [1]", &Type::Json);
    rejects(ORDER, "[]", &Type::Json, codes::UNTYPED_EMPTY_LIST);
    rejects(ORDER, "len([])", &Type::Number, codes::UNTYPED_EMPTY_LIST);
}

#[test]
fn enum_results_check_against_literal_unions() {
    let status = Type::enumeration(["active", "discontinued", "draft"]);
    accepts(SKU, "self.status", &status);
    accepts(SKU, "'draft'", &status);
    accepts(SKU, "self.stock > 0 ? 'active' : 'draft'", &status);
    rejects(SKU, "'retired'", &status, codes::RESULT_MISMATCH);
    rejects(SKU, "self.name", &status, codes::RESULT_MISMATCH);
    rejects(
        SKU,
        "self.status == 'retired'",
        &Type::Bool,
        codes::CONSTANT_COMPARISON,
    );
}

#[test]
fn result_must_match_the_expected_type() {
    rejects(SKU, "self.price", &Type::Bool, codes::RESULT_MISMATCH);
    rejects(SKU, "self.name", &Type::Integer, codes::RESULT_MISMATCH);
    rejects(SKU, "self.weight", &decimal(3), codes::RESULT_MISMATCH);
    accepts(SKU, "self.weight", &decimal(3).optional());
    accepts(SKU, "self.launch_date", &Type::Date.optional());
    rejects(SKU, "'2026-01-01'", &Type::Date, codes::RESULT_MISMATCH);
    rejects(
        SKU,
        "self.launched_at",
        &Type::Text {
            format: Some(TextFormat::Uuid),
        },
        codes::RESULT_MISMATCH,
    );
    rejects(
        SKU,
        "'2026-01-01T00:00:00Z'",
        &Type::Timestamp {
            timezone: TimezonePolicy::Utc,
        },
        codes::RESULT_MISMATCH,
    );
    accepts(SKU, "null", &Type::text().optional());
    rejects(SKU, "null", &Type::text(), codes::RESULT_MISMATCH);
    rejects(
        ORDER,
        "map(self.lines, #.quantity)",
        &Type::Bool.list(),
        codes::RESULT_MISMATCH,
    );
}

/// #2419: only the identity tuple and the derived facts are readable.
#[test]
fn actor_operand_is_closed() {
    for source in [
        "actor.actorKind == 'user'",
        "actor.subjectId != ''",
        "actor.agentSessionId == null",
        "actor.capability == 'retail.sku.update'",
        "all(actor.roles, # != 'guest')",
        "actor.authMethod == 'oidc'",
        "actor.scope.orgId == 'o1' and (actor.scope.tenantId ?? '') == 't1'",
    ] {
        accepts(SKU, source, &Type::Bool);
    }
    rejects(
        SKU,
        "actor.actorKind == 'robot'",
        &Type::Bool,
        codes::CONSTANT_COMPARISON,
    );
    for source in ["actor.email", "actor.userId", "actor.scope.region"] {
        rejects(SKU, source, &Type::Json, codes::UNKNOWN_MEMBER);
    }
    rejects(SKU, "actor[self.name]", &Type::Json, codes::DYNAMIC_READ);
    rejects(
        SKU,
        "actor.scope.tenantId == 't1'",
        &Type::Bool,
        codes::NULLABLE,
    );
}

/// Zen adoption review F2: upstream strict analysis types `o[k] - 1` as
/// `Number` with no diagnostics, and evaluation fails. Norn rejects it.
#[test]
fn f2_dynamic_key_counterexample_is_rejected() {
    use zen_expression::intellisense::IntelliSense;
    use zen_expression::variable::VariableType;

    let upstream_env = VariableType::from(&serde_json::json!({"o": {"x": "text"}, "k": "x"}));
    let analysis = IntelliSense::new()
        .with_strict(true)
        .analyze("o[k] - 1", &upstream_env);
    assert_eq!(analysis.return_type, VariableType::Number);
    assert!(analysis.diagnostics.is_empty());

    let model = Model::retail();
    let mut object_types: Vec<ObjectTypeDef> = model.object_types.clone();
    object_types.push(
        serde_json::from_value(serde_json::json!({
            "name": "probe.O",
            "properties": [{ "id": "x", "type": { "kind": "text" } }]
        }))
        .expect("probe type"),
    );
    let defs = norn_expr::EntityDefinitions {
        property_types: &model.property_types,
        interfaces: &model.interfaces,
        object_types: &object_types,
        extensions: &model.extensions,
    };
    let env = ZenTypeEnv::from_artifact_set(&defs, EnvOptions::default())
        .expect("env")
        .bind_input(fields([
            ("o", Type::Record("probe.O".into())),
            ("k", Type::text()),
        ]))
        .expect("input");
    assert_eq!(
        check(&env, "input.o[input.k] - 1", &Type::Number),
        Err(vec![codes::DYNAMIC_READ])
    );
    let found = check(&env, "o[k] - 1", &Type::Number).expect_err("must reject");
    assert!(found.contains(&codes::UNKNOWN_VARIABLE), "{found:?}");
    assert_eq!(
        check(&env, "input.o['x'] - 1", &Type::Number),
        Err(vec![codes::TYPE_ERROR])
    );
}

/// Every counterexample from review rounds 1 and 2 of PR #2852.
#[test]
fn review_counterexamples_are_rejected() {
    use codes::*;
    let cases: &[(&str, &str)] = &[
        // Round 1: fallible conversions.
        ("string(actor)", UNSUPPORTED_CONSTRUCT),
        ("number('x')", UNSUPPORTED_CONSTRUCT),
        ("number(self.name)", UNSUPPORTED_CONSTRUCT),
        // Round 1: record join losing field absence.
        (
            "(actor.actorKind == 'user' ? {a: 1} : {}).a + 1",
            TYPE_ERROR,
        ),
        ("[{a: 1}, {}]", TYPE_ERROR),
        // Removed legacy date methods and d() registry entries fail to compile.
        ("self.launched_at.year()", SYNTAX),
        ("d(self.name, 'UTC').year()", SYNTAX),
        // Round 2: template interpolation of a list.
        ("`${self.order_lines}`", UNSUPPORTED_CONSTRUCT),
        ("`${self.name}`", UNSUPPORTED_CONSTRUCT),
        // Round 2: the empty-array wildcard in joins.
        ("[[], [{a: 1}]]", UNTYPED_EMPTY_LIST),
        (
            "(self.stock > 0 ? {a: []} : {a: [{b: 1}]}).a",
            UNTYPED_EMPTY_LIST,
        ),
        // Round 2: the clock through removed legacy d() forms.
        ("d('UTC', 'UTC')", SYNTAX),
        ("d('2024-02-29', 'UTC').add('1y').year()", SYNTAX),
        ("d('2026-03-29 02:30:00', 'Europe/Berlin').hour()", SYNTAX),
        // Round 2: null inputs to values/merge/mergeDeep.
        ("values(null)", UNSUPPORTED_CONSTRUCT),
        ("merge([null])", UNSUPPORTED_CONSTRUCT),
        ("mergeDeep([{a: 1}, null])", UNSUPPORTED_CONSTRUCT),
        ("keys(self)", UNSUPPORTED_CONSTRUCT),
        // Round 2: unchecked slices and indexing.
        ("self.name[0:100]", UNSUPPORTED_CONSTRUCT),
        ("self.order_lines[0]", INDEX),
        // Round 2: date and interval results, also nested.
        ("[1..2]", UNSUPPORTED_CONSTRUCT),
        ("{a: [1..2]}", UNSUPPORTED_CONSTRUCT),
        ("[d(self.launched_at, 'UTC')]", SYNTAX),
        ("{a: d(self.launched_at, 'UTC')}", SYNTAX),
        // Everything else outside the allowlist.
        ("rand(10)", UNSUPPORTED_CONSTRUCT),
        ("d()", SYNTAX),
        ("date()", SYNTAX),
        ("matches(self.name, '^W')", UNSUPPORTED_CONSTRUCT),
        ("max([1, self.price])", UNSUPPORTED_CONSTRUCT),
        (
            "avg(map(self.order_lines, #.quantity))",
            UNSUPPORTED_CONSTRUCT,
        ),
        ("self.stock % 2", UNSUPPORTED_CONSTRUCT),
        ("self.stock ^ 2", UNSUPPORTED_CONSTRUCT),
        ("a = 1; a", UNSUPPORTED_CONSTRUCT),
        ("$root.self.price", UNSUPPORTED_CONSTRUCT),
        ("round(self.price, self.stock)", TYPE_ERROR),
        ("round(self.price, 29)", TYPE_ERROR),
        ("'a' + 1", TYPE_ERROR),
    ];
    for (source, code) in cases {
        rejects(SKU, source, &Type::Json, code);
    }
}

/// T1-EXPR-3a (#2520): one accept/reject pair (or more) per checker rule,
/// each rejection with its stable code.
#[test]
fn every_rule_has_positive_and_negative_fixtures() {
    use codes::*;
    let accepted: &[(&str, &str, Type)] = &[
        // literals
        (SKU, "null", Type::text().optional()),
        (SKU, "true", Type::Bool),
        (SKU, "1.25", decimal(2)),
        (SKU, "'x'", Type::text()),
        (SKU, "[1, 2]", Type::Number.list()),
        (SKU, "{a: 'x', b: [true]}", Type::Json),
        // field select, by name and by literal key
        (SKU, "self.price", decimal(2)),
        (SKU, "self['stock']", Type::Integer),
        (SKU, "({a: 1}).a", Type::Number),
        // index into a list literal, in bounds
        (SKU, "[self.name, 'x'][1]", Type::text()),
        (SKU, "([1, 2, 3])[2]", Type::Number),
        (SKU, "[[1], [2, 3]][0]", Type::Number.list()),
        // null coalescing
        (SKU, "self.weight ?? 0", Type::Number),
        (SKU, "(actor.agentSessionId ?? '') + '!'", Type::text()),
        // operators
        (SKU, "self.price * 2 - self.stock / 4", Type::Number),
        (SKU, "self.name + ' ' + self.sku_code", Type::text()),
        (SKU, "self.stock >= 0 and not self.is_taxable", Type::Bool),
        (SKU, "self.status != 'draft'", Type::Bool),
        (SKU, "self.stock > 0 ? 'a' : 'b'", Type::text()),
        // membership and ranges
        (SKU, "self.status in ['draft', 'active']", Type::Bool),
        (SKU, "self.status not in ['draft']", Type::Bool),
        (SKU, "'admin' in actor.roles", Type::Bool),
        (SKU, "self.stock in [1, 2, 3]", Type::Bool),
        (SKU, "self.is_taxable in [true]", Type::Bool),
        (SKU, "self.stock in [0..10]", Type::Bool),
        (SKU, "self.price in (0..self.stock]", Type::Bool),
        (SKU, "self.stock not in [0..10)", Type::Bool),
        (
            ORDER,
            "some(self.lines, #.quantity in [1..(self.customer.tier == 'gold' ? 9 : 3)])",
            Type::Bool,
        ),
    ];
    for (self_type, source, expected) in accepted {
        accepts(self_type, source, expected);
    }
    let rejected: &[(&str, &str)] = &[
        // literals
        ("[1, 'x']", TYPE_ERROR),
        ("[]", UNTYPED_EMPTY_LIST),
        ("{a: 1, a: 2}", TYPE_ERROR),
        // field select
        ("self.nope", UNKNOWN_MEMBER),
        // `$` is bound only in a unary test.
        ("$ > 1", UNKNOWN_VARIABLE),
        ("({a: 1}).b", UNKNOWN_MEMBER),
        ("self[self.name]", DYNAMIC_READ),
        // index: only a literal in-bounds index into a list literal
        ("[1, 2][2]", INDEX),
        ("[1, 2][-1]", INDEX),
        ("[1, 2][0.5]", INDEX),
        ("[1, 2][self.stock]", INDEX),
        ("actor.roles[0]", INDEX),
        ("map(self.order_lines, #.quantity)[0]", INDEX),
        ("self.name[0]", UNSUPPORTED_CONSTRUCT),
        // Sol LOW on #2861: a string key into a list is an index read.
        ("[1]['0']", INDEX),
        ("actor.roles['0']", INDEX),
        // zen's strict analysis types every index read as nullable, so the
        // cross-check rejects an index result used as an operand.
        ("[1, 2, 3][2] + 1", UPSTREAM),
        // null coalescing
        ("self.price ?? 0", TYPE_ERROR),
        ("self.weight ?? 'x'", TYPE_ERROR),
        ("self.weight + 1", NULLABLE),
        // operators
        ("self.name + 1", TYPE_ERROR),
        ("1 + self.name", TYPE_ERROR),
        ("self.name - 'x'", TYPE_ERROR),
        ("self.is_taxable + self.is_taxable", TYPE_ERROR),
        ("self.name < 'x'", TYPE_ERROR),
        ("self.stock == 'x'", TYPE_ERROR),
        ("self.stock and true", TYPE_ERROR),
        ("self.stock % 2", UNSUPPORTED_CONSTRUCT),
        // membership and ranges
        ("self.status in ['retired']", CONSTANT_COMPARISON),
        ("self.stock in []", CONSTANT_COMPARISON),
        ("self.stock in ['1']", TYPE_ERROR),
        ("self.name in [1..2]", TYPE_ERROR),
        ("self.stock in [1..self.name]", TYPE_ERROR),
        ("self.weight in [1, 2]", NULLABLE),
        ("self.stock in [1..self.weight]", NULLABLE),
        ("'a' in {a: 1}", TYPE_ERROR),
        ("[1] in [[1]]", TYPE_ERROR),
        ("[1..2]", UNSUPPORTED_CONSTRUCT),
        ("self.stock > 0 ? [1..2] : [3..4]", UNSUPPORTED_CONSTRUCT),
        (
            "d(self.launched_at, 'UTC') in [d(self.launched_at, 'UTC')..d(self.launched_at, 'UTC')]",
            SYNTAX,
        ),
    ];
    for (source, code) in rejected {
        rejects(SKU, source, &Type::Json, code);
    }
}

/// T1-EXPR-3b (#2521): `#` closures. `one` and `flatMap` are total on a
/// boolean (resp. non-null list) body and are admitted in `norn-expr/4`;
/// nested closures bind `#` to the innermost element.
#[test]
fn closures_type_check() {
    use codes::*;
    let accepted: &[(&str, &str, Type, InferredType)] = &[
        (
            ORDER,
            "one(self.lines, #.quantity > 1)",
            Type::Bool,
            InferredType::Bool,
        ),
        (
            ORDER,
            "flatMap(self.lines, [#.quantity, #.amount])",
            Type::Number.list(),
            InferredType::List(Box::new(InferredType::Number)),
        ),
        (
            ORDER,
            "flatMap(self.lines as l, map(l.item.order_lines, #.quantity))",
            Type::Number.list(),
            InferredType::List(Box::new(InferredType::Number)),
        ),
        (
            ORDER,
            "some(self.customer.orders, all(#.lines, #.quantity > 0))",
            Type::Bool,
            InferredType::Bool,
        ),
        (
            ORDER,
            "map(self.lines as l, count(actor.roles, # == l.item.name))",
            Type::Number.list(),
            InferredType::List(Box::new(InferredType::Number)),
        ),
    ];
    for (self_type, source, expected, inferred) in accepted {
        assert_eq!(
            &accepts(self_type, source, expected),
            inferred,
            "`{source}`"
        );
    }
    // `flatMap` splices one level: the result is `map`'s element type.
    let InferredType::List(nested) =
        accepts(ORDER, "map(self.customer.orders, #.lines)", &Type::Json)
    else {
        panic!("map of a list is a list");
    };
    assert_eq!(
        accepts(ORDER, "flatMap(self.customer.orders, #.lines)", &Type::Json),
        *nested
    );
    let rejected: &[(&str, &str)] = &[
        ("one(self.lines, #.quantity)", TYPE_ERROR),
        ("flatMap(self.lines, #.quantity)", TYPE_ERROR),
        ("flatMap(self.lines, #.order)", NULLABLE),
        ("map(self.lines, #.order)", NULLABLE),
        ("filter(self.customer.note, # == 'x')", NULLABLE),
        ("some(self.number, # == 'x')", TYPE_ERROR),
        ("some(self.lines, #.nope)", UNKNOWN_MEMBER),
        ("all(self.lines, 1)", TYPE_ERROR),
        ("#", UNKNOWN_VARIABLE),
        ("map(self.lines, line.quantity)", UNKNOWN_VARIABLE),
    ];
    for (source, code) in rejected {
        rejects(ORDER, source, &Type::Json, code);
    }
}

/// #2520 acceptance: a deliberately ill-typed guard is rejected with a stable
/// code and the span of the offending operand.
#[test]
fn ill_typed_guard_is_rejected_with_a_stable_code() {
    let env = env_for(SKU);
    let expr = Expression::compile("self.stock > 0 and self.name").expect("compile");
    let diagnostics = check_expression(&expr, &env, &Type::Bool).expect_err("reject");
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert_eq!(diagnostics[0].code, "expr.type.error");
    assert_eq!(diagnostics[0].span, (19, 28));
}

#[test]
fn nullability_is_enforced() {
    rejects(SKU, "self.weight + 1", &Type::Number, codes::NULLABLE);
    rejects(SKU, "self.weight > 1", &Type::Bool, codes::NULLABLE);
    rejects(SKU, "len(self.launch_date)", &Type::Number, codes::NULLABLE);
    rejects(SKU, "self.price ?? 0", &Type::Number, codes::TYPE_ERROR);
    rejects(SKU, "self.weight ?? 'none'", &Type::Json, codes::TYPE_ERROR);
    rejects(
        SKU,
        "self.price == null",
        &Type::Bool,
        codes::CONSTANT_COMPARISON,
    );
    accepts(SKU, "(self.weight ?? 0) > 1", &Type::Bool);
    // An optional to-one link is a nullable read.
    rejects(
        ORDER,
        "map(self.lines, #.order.number)",
        &Type::Json,
        codes::NULLABLE,
    );
}

#[test]
fn dynamic_and_unknown_reads_are_rejected() {
    rejects(SKU, "self[self.name]", &Type::Json, codes::DYNAMIC_READ);
    rejects(SKU, "self.nope", &Type::Json, codes::UNKNOWN_MEMBER);
    rejects(SKU, "zzz + 1", &Type::Number, codes::UNKNOWN_VARIABLE);
    rejects(SKU, "self.attributes", &Type::Json, codes::OPAQUE_READ);
    rejects(
        SKU,
        "self.attributes.color == 'red'",
        &Type::Bool,
        codes::OPAQUE_READ,
    );
    rejects(SKU, "self", &Type::Json, codes::OPAQUE_READ);
}

#[test]
fn links_traverse_only_to_the_link_depth() {
    accepts(
        ORDER,
        "map(self.customer.orders, #.customer.email)",
        &Type::text().list(),
    );
    rejects(
        ORDER,
        "map(self.customer.orders, len(#.customer.orders))",
        &Type::Json,
        codes::UNKNOWN_MEMBER,
    );
    let model = Model::retail();
    let shallow =
        ZenTypeEnv::from_artifact_set(&model.definitions(), EnvOptions { max_link_depth: 1 })
            .expect("env")
            .bind_self(&name(ORDER))
            .expect("self");
    assert!(check(&shallow, "self.customer.email", &Type::text()).is_ok());
    assert_eq!(
        check(&shallow, "len(self.customer.orders)", &Type::Json),
        Err(vec![codes::UNKNOWN_MEMBER])
    );
}

#[test]
fn diagnostics_carry_spans() {
    let env = env_for(SKU);
    let expr = Expression::compile("1 + self.nope").expect("compile");
    let diagnostics = check_expression(&expr, &env, &Type::Number).expect_err("reject");
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].code, codes::UNKNOWN_MEMBER);
    assert_eq!(diagnostics[0].span, (4, 13));
}

// ---------------------------------------------------------------------------
// Generated soundness check
// ---------------------------------------------------------------------------

fn conforms(value: &Value, ty: &InferredType) -> bool {
    match (ty, value) {
        (InferredType::Optional(_) | InferredType::Null, Value::Null) => true,
        (InferredType::Optional(inner), value) => conforms(value, inner),
        (InferredType::Bool, Value::Bool(_)) | (InferredType::Number, Value::Number(_)) => true,
        (InferredType::String, Value::String(_)) => true,
        (InferredType::Date, Value::String(found)) => {
            norn_expr::temporal::validate_date(found).is_ok()
        }
        (InferredType::Timestamp, Value::String(found)) => {
            norn_expr::temporal::canonical_timestamp(found)
                .is_ok_and(|canonical| canonical == *found)
        }
        (InferredType::Const(expected), Value::String(found)) => expected == found,
        (InferredType::Enum(values), Value::String(found)) => values.contains(found),
        (InferredType::List(item), Value::Array(items)) => items.iter().all(|v| conforms(v, item)),
        (InferredType::Record(fields), Value::Object(entries)) => {
            entries.len() == fields.len()
                && entries
                    .iter()
                    .all(|(k, v)| fields.get(k).is_some_and(|t| conforms(v, t)))
        }
        _ => false,
    }
}

fn contexts() -> Vec<Value> {
    let num = |text: &str| Value::Number(text.parse::<Number>().expect("number"));
    let line = |quantity: i64, amount: &str| {
        Value::object([
            ("quantity", Value::from(quantity)),
            ("amount", num(amount)),
            ("order", Value::Null),
            (
                "item",
                Value::object([
                    ("sku_code", Value::from("SKU-1")),
                    ("name", Value::from("Widget")),
                    ("order_lines", Value::Array(Vec::new())),
                ]),
            ),
        ])
    };
    let sku = |name: &str,
               price: &str,
               weight: Value,
               stock: i64,
               lines: Vec<Value>,
               session: Value,
               roles: Vec<Value>| {
        Value::object([
            (
                "self",
                Value::object([
                    ("sku_code", Value::from("SKU-1")),
                    ("name", Value::from(name)),
                    ("price", num(price)),
                    ("weight", weight),
                    ("status", Value::from("draft")),
                    ("launched_at", Value::from("2024-02-29T00:00:00Z")),
                    ("launch_date", Value::Null),
                    ("shelf_life", Value::from("PT1S")),
                    ("is_taxable", Value::Bool(stock > 0)),
                    ("stock", Value::from(stock)),
                    ("order_lines", Value::Array(lines)),
                ]),
            ),
            (
                "actor",
                Value::object([
                    ("actorKind", Value::from("system")),
                    ("subjectId", Value::from("")),
                    ("agentSessionId", session),
                    ("capability", Value::from("c")),
                    ("roles", Value::Array(roles)),
                    ("authMethod", Value::from("token")),
                    (
                        "scope",
                        Value::object([("orgId", Value::from("o")), ("tenantId", Value::Null)]),
                    ),
                ]),
            ),
        ])
    };
    let mut contexts = vec![
        sku("", "0", Value::Null, 0, Vec::new(), Value::Null, Vec::new()),
        sku(
            "Widget",
            "-12.50",
            num("0.001"),
            3,
            vec![line(1, "0.10"), line(5, "-3")],
            Value::from("s1"),
            vec![Value::from("admin"), Value::from("")],
        ),
        sku(
            "  ümlaut ",
            "79228162514264.337593543950335",
            num("0"),
            -1,
            vec![line(0, "0")],
            Value::from(""),
            vec![Value::from("x")],
        ),
    ];
    if let Some(Value::Object(root)) = contexts.get_mut(1)
        && let Some(Value::Object(self_value)) = root.get_mut("self")
    {
        self_value.insert("launch_date".to_owned(), Value::from("2024-02-29"));
    }
    // Max-cardinality slices (T1-EXPR-4): the static cost estimate must bound
    // the metered cost at the edges of the `norn.limits/1` slice bounds.
    let many_lines = (0..1_000)
        .map(|i| line(i % 7, if i % 2 == 0 { "1.5" } else { "-2" }))
        .collect();
    // `ΐ` is 2 bytes and upper-cases to 3 characters, 6 bytes: the worst
    // case of `upper`.
    let long_name = "\u{390}".repeat(40_000);
    let many_roles = (0..40_000)
        .map(|i| Value::from(if i % 3 == 0 { "admin" } else { "" }))
        .collect();
    let edges = [
        sku(
            "Widget",
            "1",
            num("2"),
            5,
            many_lines,
            Value::from("s"),
            vec![Value::from("admin")],
        ),
        sku(
            &long_name,
            "1",
            Value::Null,
            1,
            vec![line(2, "1")],
            Value::from(long_name.as_str()),
            vec![Value::from(long_name.as_str())],
        ),
        sku(
            "x",
            "1",
            Value::Null,
            0,
            Vec::new(),
            Value::Null,
            many_roles,
        ),
    ];
    for edge in &edges {
        assert!(
            within_slice_limits(edge),
            "an edge context must be a conforming slice ({} bytes)",
            slice_bytes(edge)
        );
    }
    contexts.extend(edges);
    contexts
}

/// Candidate expressions generated from the grammar (and deliberately from
/// ill-typed combinations of it). Every one the checker accepts must evaluate
/// to a value conforming to its inferred type on every conforming context,
/// and use at most its static cost estimate (T1-EXPR-4): evaluated under a
/// budget of exactly the estimate, it never exhausts it, also on the
/// max-cardinality edge contexts.
#[test]
fn generated_accepted_expressions_are_total_and_typed() {
    let atoms = [
        // numbers
        "self.price",
        "self.stock",
        "0",
        "-1.5",
        "(self.weight ?? 0)",
        "len(self.name)",
        "len(self.order_lines)",
        "sum(map(self.order_lines, #.quantity))",
        "count(self.order_lines, #.quantity > 1)",
        "abs(self.price)",
        "round(self.price, 2)",
        "floor(self.price)",
        "ceil(self.stock)",
        "trunc(self.price)",
        // strings
        "self.name",
        "'x'",
        "self.status",
        "actor.actorKind",
        "self.launch_date",
        "lower(self.name)",
        "upper(self.sku_code)",
        "trim(self.name)",
        "(actor.agentSessionId ?? '')",
        "self.launched_at",
        // bools
        "self.is_taxable",
        "true",
        "self.weight == null",
        "actor.agentSessionId != null",
        "startsWith(self.name, 'W')",
        "self.status in ['draft', 'active']",
        "'admin' in actor.roles",
        "self.stock in [0..3)",
        "self.price not in (self.stock..1]",
        "endsWith(self.name, actor.subjectId)",
        "contains(self.name, 'id')",
        "some(self.order_lines, #.quantity > 1)",
        "all(actor.roles, # != '')",
        "none(actor.roles as r, r == 'admin')",
        "one(self.order_lines, #.quantity > 1)",
        "flatMap(self.order_lines, [#.quantity, #.amount])",
        "flatMap(self.order_lines as l, map(actor.roles, l.quantity))",
        // lists, records, nullables
        "actor.roles",
        "map(self.order_lines, #.amount)",
        "filter(self.order_lines, #.quantity > 0)",
        "[1, self.stock]",
        "[self.name, 'y']",
        "[self.name, 'y'][1]",
        "[[1], [self.stock, 2]][1]",
        "['draft', 'active']",
        "[0, self.price]",
        "[true]",
        "{a: self.stock}",
        "self.weight",
        "null",
    ];
    let binary = [
        "+", "-", "*", "==", "!=", "<", ">=", "and", "or", "??", "in", "not in",
    ];
    let mut candidates: Vec<String> = Vec::new();
    for a in atoms {
        candidates.push(a.to_owned());
        candidates.push(format!("-{a}"));
        candidates.push(format!("not {a}"));
        for f in ["len", "lower", "abs", "sum", "floor"] {
            candidates.push(format!("{f}({a})"));
        }
        candidates.push(format!("map(actor.roles, {a})"));
        candidates.push(format!("filter(self.order_lines, {a})"));
        for b in atoms {
            for op in binary {
                candidates.push(format!("({a}) {op} ({b})"));
            }
            candidates.push(format!("self.is_taxable ? ({a}) : ({b})"));
            candidates.push(format!("[{a}, {b}]"));
            candidates.push(format!("startsWith({a}, {b})"));
            candidates.push(format!("({a}) in [({b})..10]"));
            candidates.push(format!("({a}) in (-1..({b}))"));
            candidates.push(format!("[{a}, {b}][1]"));
        }
    }
    // Candidates are independent: workers check interleaved slices, each with
    // its own environment, and the totals are order-independent.
    let workers = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
    let (accepted, rejected, peak, admitted) = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|worker| {
                let sources: Vec<&str> = candidates
                    .iter()
                    .skip(worker)
                    .step_by(workers)
                    .map(String::as_str)
                    .collect();
                scope.spawn(move || check_generated(&sources))
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .fold(
                (0, 0, 0, std::collections::BTreeSet::new()),
                |(accepted, rejected, peak, mut admitted), (a, r, p, more)| {
                    admitted.extend(more);
                    (accepted + a, rejected + r, peak.max(p), admitted)
                },
            )
    });
    assert!(
        peak > 100_000,
        "the edge contexts should drive some admitted expression near the budget (peak {peak})"
    );
    // The #2520 constructs are exercised, not only generated.
    let missing = [
        "(self.name) + ('x')",
        "(actor.actorKind) + (self.status)",
        "(self.stock) in ([0, self.price])",
        "(self.status) in (['draft', 'active'])",
        "(self.name) not in (actor.roles)",
        "(self.is_taxable) in ([true])",
        "(self.price) in [(self.stock)..10]",
        "(len(self.name)) in (-1..(self.stock))",
        "[self.name, 'y'][1]",
        "[self.price, len(self.name)][1]",
        "self.is_taxable ? ([self.name, 'y'][1]) : (self.status)",
        "(one(self.order_lines, #.quantity > 1)) or (self.is_taxable)",
        "len(flatMap(self.order_lines, [#.quantity, #.amount]))",
        "sum(flatMap(self.order_lines, [#.quantity, #.amount]))",
    ]
    .into_iter()
    .filter(|sample| !admitted.contains(*sample))
    .collect::<Vec<_>>();
    assert!(missing.is_empty(), "not admitted: {missing:?}");
    assert!(accepted > 1_000, "only {accepted} accepted");
    assert!(rejected > 1_000, "only {rejected} rejected");
}

/// Checks and evaluates `sources` for [`generated_accepted_expressions_are_total_and_typed`]:
/// accepted and rejected counts, peak units used and the admitted sources.
fn check_generated(sources: &[&str]) -> (usize, usize, u64, std::collections::BTreeSet<String>) {
    let env = env_for(SKU);
    let contexts = contexts();
    let limits = ResourceLimitProfile::norn_limits_v1();
    let (mut accepted, mut rejected) = (0_usize, 0_usize);
    let mut peak = 0_u64;
    let mut admitted = std::collections::BTreeSet::new();
    for &source in sources {
        let expr = Expression::compile(source).unwrap_or_else(|err| panic!("{source}: {err}"));
        let Ok(checked) = check_expression_with(&expr, &env, &Type::Json, &limits) else {
            rejected += 1;
            continue;
        };
        accepted += 1;
        admitted.insert(source.to_owned());
        let bound = checked.cost().units();
        assert!(bound <= limits.decide.fuel_units);
        let inferred = checked.inferred();
        for context in &contexts {
            let evaluation = expr
                .evaluate(context, EvaluationBudget::new(bound))
                .unwrap_or_else(|err| {
                    panic!("accepted `{source}` (estimate {bound}) failed at runtime: {err}")
                });
            assert!(evaluation.units_used() <= bound);
            peak = peak.max(evaluation.units_used());
            let value = evaluation.into_value();
            assert!(
                conforms(&value, inferred),
                "accepted `{source}` returned {value:?}, checked as {inferred}"
            );
        }
    }
    (accepted, rejected, peak, admitted)
}

// ---------------------------------------------------------------------------
// Unary tests (T1-EXPR-3b, #2521)
// ---------------------------------------------------------------------------

fn check_test(self_type: &str, source: &str, subject: &Type) -> Result<(), Vec<&'static str>> {
    let test = UnaryTest::compile(source).map_err(|_compile_error| vec![codes::SYNTAX])?;
    check_unary_with(&test, &env_for(self_type), subject, &roomy_limits())
        .map(|_| ())
        .map_err(|diagnostics| diagnostics.iter().map(|d| d.code).collect())
}

fn status() -> Type {
    Type::enumeration(["active", "discontinued", "draft"])
}

#[test]
fn unary_tests_check_against_the_subject_type() {
    let accepted: &[(&str, Type)] = &[
        // The wildcard: an empty cell is skipped by the engine.
        ("", Type::Json),
        // Comparison operators compare `$`.
        ("> 10000", decimal(2)),
        ("<= self.price", Type::Number),
        ("!= 0", Type::Integer),
        ("== 'x'", Type::text()),
        ("> 5 and < 10", Type::Number),
        ("> 10, < -5", Type::Number),
        // A bare value is an equality test.
        ("5", Type::Number),
        ("-1.5", Type::Number),
        ("self.stock * 2", Type::Number),
        ("'gold'", Type::text()),
        ("'draft', 'active'", status()),
        ("self.status", status()),
        ("true", Type::Bool),
        ("self.is_taxable", Type::Bool),
        // A list or range is a membership test.
        ("[1..5]", Type::Integer),
        ("(0..self.stock]", Type::Number),
        ("[1, 2, 3]", Type::Number),
        ("['draft', 'active']", status()),
        ("not in ['draft']", status()),
        ("in actor.roles", Type::text()),
        ("map(self.order_lines, #.quantity)", Type::Integer),
        // `null` tests a nullable subject.
        ("null", decimal(3).optional()),
        ("!= null", Type::text().optional()),
        // A boolean-valued part is used as is.
        ("$ > 5 and $ < 10", Type::Number),
        ("($ ?? 0) > 5", Type::Number.optional()),
        ("$ == 0 or $ > 0", Type::Number),
        ("$ == null or self.stock > 0", Type::Number.optional()),
        ("len($) > 3", Type::text()),
        ("startsWith($, 'W'), endsWith($, 'x')", Type::text()),
        ("contains(lower($), 'id')", Type::text()),
        ("some($, # > 10 and # < 20)", Type::Number.list()),
        ("count($, # == 'admin') == 1", Type::text().list()),
        (
            "one($, #.quantity > 1)",
            Type::Record("commerce.OrderLine".into()).list(),
        ),
        ("$.quantity > 1", Type::Record("commerce.OrderLine".into())),
        ("$", Type::Bool),
        ("not $", Type::Bool),
        ("$ in [1..3] or self.stock > 0", Type::Number),
        ("self.stock > 0 and $ != 'x'", Type::text()),
    ];
    for (source, subject) in accepted {
        if let Err(codes) = check_test(SKU, source, subject) {
            panic!("`{source}` against `{subject}` should check, got {codes:?}");
        }
    }
}

/// Every excluded unary construct, with its stable code.
#[test]
fn unary_tests_outside_the_profile_are_rejected() {
    use codes::*;
    let cases: &[(&str, Type, &str)] = &[
        // Nullable subjects must be handled explicitly.
        ("> 5", Type::Number.optional(), NULLABLE),
        ("'x'", Type::text().optional(), NULLABLE),
        ("[1..5]", Type::Number.optional(), NULLABLE),
        ("len($) > 0", Type::text().optional(), NULLABLE),
        // Operand types.
        ("> 'a'", Type::text(), TYPE_ERROR),
        ("< 5", Type::text(), TYPE_ERROR),
        ("'5'", Type::Number, TYPE_ERROR),
        ("5", Type::Bool, TYPE_ERROR),
        ("[1..5]", Type::text(), TYPE_ERROR),
        ("[1, 2]", Type::text(), TYPE_ERROR),
        ("in 'abc'", Type::text(), TYPE_ERROR),
        ("self.name", Type::Number, TYPE_ERROR),
        ("$ + 'a'", Type::Number, TYPE_ERROR),
        ("len($)", Type::Number, TYPE_ERROR),
        ("some($, # > 1)", Type::Number, TYPE_ERROR),
        (
            "$.quantity",
            Type::Record("commerce.OrderLine".into()),
            TYPE_ERROR,
        ),
        // Always-constant tests.
        ("'retired'", status(), CONSTANT_COMPARISON),
        ("'retired', 'draft'", status(), CONSTANT_COMPARISON),
        ("['retired']", status(), CONSTANT_COMPARISON),
        ("null", Type::Number, CONSTANT_COMPARISON),
        ("[]", Type::Number, CONSTANT_COMPARISON),
        // Reads.
        (
            "$.nope > 1",
            Type::Record("commerce.OrderLine".into()),
            UNKNOWN_MEMBER,
        ),
        ("$ == 1", Type::Json, OPAQUE_READ),
        ("> 1", Type::Json, OPAQUE_READ),
        ("$[0] == 1", Type::Number.list(), INDEX),
        ("$['a'] == 1", Type::Number.list(), INDEX),
        ("> x", Type::Number, UNKNOWN_VARIABLE),
        // Constructs outside the allowlist.
        ("5 % 2", Type::Number, UNSUPPORTED_CONSTRUCT),
        ("2 ^ 3", Type::Number, UNSUPPORTED_CONSTRUCT),
        ("max([3, 5, 8])", Type::Number, UNSUPPORTED_CONSTRUCT),
        ("rand(100) >= 0", Type::Bool, UNSUPPORTED_CONSTRUCT),
        ("matches($, '[0-9]+')", Type::text(), UNSUPPORTED_CONSTRUCT),
        ("`a ${$}`", Type::text(), UNSUPPORTED_CONSTRUCT),
        ("$[0:5] == 'sample'", Type::text(), UNSUPPORTED_CONSTRUCT),
        ("string(true)", Type::text(), UNSUPPORTED_CONSTRUCT),
        ("number('1')", Type::Number, UNSUPPORTED_CONSTRUCT),
        ("bool('true')", Type::Bool, UNSUPPORTED_CONSTRUCT),
        ("d($).year() == 2024", Type::text(), SYNTAX),
        ("$root.self.stock > 1", Type::Number, UNSUPPORTED_CONSTRUCT),
        (
            "{quantity: 1, amount: 2}",
            Type::Record("commerce.OrderLine".into()),
            TYPE_ERROR,
        ),
        (
            "len(keys($)) > 0",
            Type::Record("commerce.OrderLine".into()),
            UNSUPPORTED_CONSTRUCT,
        ),
    ];
    for (source, subject, code) in cases {
        match check_test(SKU, source, subject) {
            Ok(()) => panic!("`{source}` against `{subject}` must be rejected with {code}"),
            Err(found) => assert!(
                found.contains(code),
                "`{source}` against `{subject}`: expected {code}, got {found:?}"
            ),
        }
    }
    // Only the empty cell is the wildcard; zen does not parse these.
    for source in [" ", "-", "> > 1", "[1..", "'open"] {
        assert!(UnaryTest::compile(source).is_err(), "`{source}` compiled");
    }
}

/// The implicit `$` has no source span; its findings take the part's span.
#[test]
fn unary_diagnostics_carry_the_part_span() {
    let test = UnaryTest::compile("'a', > 5").expect("compile");
    let diagnostics =
        check_unary(&test, &env_for(SKU), &Type::Number.optional()).expect_err("reject");
    let spans: Vec<(&str, (u32, u32))> = diagnostics.iter().map(|d| (d.code, d.span)).collect();
    assert!(spans.contains(&(codes::NULLABLE, (5, 8))), "{spans:?}");
    assert!(spans.iter().all(|(_, span)| *span != (0, 0)), "{spans:?}");
}

/// Subjects of every admitted kind, with conforming values.
fn subjects() -> Vec<(Type, Vec<Value>)> {
    let num = |text: &str| Value::Number(text.parse::<Number>().expect("number"));
    let strings = |values: &[&str]| values.iter().map(|v| Value::from(*v)).collect::<Vec<_>>();
    vec![
        (
            Type::Number,
            vec![
                num("0"),
                num("-12.5"),
                num("3"),
                num("79228162514264.337593543950335"),
            ],
        ),
        (
            Type::text(),
            vec![
                Value::from(""),
                Value::from("draft"),
                Value::from("  Widget "),
                Value::from("ümlaut"),
                // A max-size subject: `ΐ` upper-cases from 2 to 6 bytes.
                Value::from("\u{390}".repeat(100_000)),
            ],
        ),
        (status(), strings(&["draft", "active", "discontinued"])),
        (Type::Bool, vec![Value::Bool(true), Value::Bool(false)]),
        (
            Type::Number.optional(),
            vec![Value::Null, num("2"), num("-1")],
        ),
        (
            Type::text().optional(),
            vec![Value::Null, Value::from("draft"), Value::from("")],
        ),
        (
            Type::Number.list(),
            vec![
                Value::Array(Vec::new()),
                Value::Array(vec![num("1"), num("15")]),
                Value::Array(vec![num("7"); 100_000]),
            ],
        ),
        (
            Type::text().list(),
            vec![
                Value::Array(Vec::new()),
                Value::Array(strings(&["admin", "x"])),
                Value::Array(vec![Value::from(""); 60_000]),
            ],
        ),
    ]
}

/// Candidate unary tests generated from the grammar (and deliberately from
/// ill-typed combinations of it). Every test the checker accepts against a
/// subject type must evaluate, in zen's unary mode, to a boolean (never an
/// error) for every conforming subject and context.
#[test]
fn generated_accepted_unary_tests_are_total() {
    let values = [
        "5",
        "-1.5",
        "self.stock",
        "len(self.name)",
        "(self.weight ?? 0)",
        "'draft'",
        "'x'",
        "self.name",
        "self.status",
        "true",
        "self.is_taxable",
        "null",
        "[1, 2]",
        "['draft', 'active']",
        "actor.roles",
        "[0..10]",
        "(self.stock..5]",
        "[]",
        "map(self.order_lines, #.quantity)",
    ];
    let parts = [
        "$ > 1",
        "$ == 'draft'",
        "len($) > 0",
        "contains($, 'a')",
        "some($, # > 1)",
        "all($, # != '')",
        "one($, # > 1)",
        "count($, # == 'admin') > 0",
        "$ == null",
        "($ ?? 0) > 1",
        "($ ?? '') == 'x'",
        "not $",
        "$",
        "$ in [1..3]",
        "$ in actor.roles",
        "self.stock > 0",
        "$ + 1 > 2",
        "startsWith(lower($), 'd')",
    ];
    let prefixes = ["", "> ", "< ", ">= ", "<= ", "== ", "!= ", "in ", "not in "];
    let mut singles: Vec<String> = Vec::new();
    for value in values {
        for prefix in prefixes {
            singles.push(format!("{prefix}{value}"));
        }
    }
    singles.extend(parts.iter().map(|p| (*p).to_owned()));
    let mut candidates = singles.clone();
    let joinable = [
        "> 1",
        "< 10",
        "'draft'",
        "[1..5]",
        "null",
        "$ > 1",
        "len($) > 0",
        "some($, # > 1)",
        "true",
        "$ == null",
    ];
    for a in joinable {
        for b in joinable {
            for join in [", ", " and ", " or "] {
                candidates.push(format!("{a}{join}{b}"));
            }
        }
    }
    let env = env_for(SKU);
    let contexts = contexts();
    let limits = ResourceLimitProfile::norn_limits_v1();
    let (mut accepted, mut rejected) = (0_usize, 0_usize);
    let mut admitted = std::collections::BTreeSet::new();
    for (subject_type, subject_values) in subjects() {
        for subject in &subject_values {
            assert!(
                within_slice_limits(subject),
                "subject is not a conforming slice"
            );
        }
        for source in &candidates {
            let test = UnaryTest::compile(source).unwrap_or_else(|err| panic!("{source}: {err}"));
            let Ok(bound) = check_unary_with(&test, &env, &subject_type, &limits) else {
                rejected += 1;
                continue;
            };
            accepted += 1;
            admitted.insert(format!("{subject_type}: {source}"));
            // Every subject on the small contexts, and the smallest subject
            // on the max-cardinality edge contexts too. Evaluated under a
            // budget of exactly the static estimate (T1-EXPR-4).
            let small = contexts.len().min(3);
            for (index, context) in contexts.iter().enumerate() {
                let subjects = if index < small {
                    &subject_values[..]
                } else {
                    &subject_values[..1]
                };
                for subject in subjects {
                    if let Err(err) =
                        test.evaluate(context, subject, EvaluationBudget::new(bound.units()))
                    {
                        panic!(
                            "accepted `{source}` against `{subject_type}` (estimate {bound}) failed: {err}"
                        );
                    }
                }
            }
        }
    }
    let missing = [
        "number: > self.stock",
        "number: [0..10]",
        "number: > 1, < 10",
        "text: 'draft'",
        "text: in actor.roles",
        "bool: true",
        "number?: null",
        "number?: ($ ?? 0) > 1",
        "list<number>: len($) > 0",
        "list<text>: len($) > 0",
    ]
    .into_iter()
    .filter(|sample| !admitted.contains(*sample))
    .collect::<Vec<_>>();
    assert!(missing.is_empty(), "not admitted: {missing:?}");
    assert!(accepted > 200, "only {accepted} accepted");
    assert!(rejected > 1_000, "only {rejected} rejected");
}
