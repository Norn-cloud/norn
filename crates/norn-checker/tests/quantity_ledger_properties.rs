#![allow(
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::pedantic,
    clippy::unwrap_used,
    missing_docs,
    reason = "bounded property tests use compact generated vectors and assertion-oriented test failures"
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

use std::collections::BTreeMap;

use norn_checker::quantity_ledger::{
    compile_ledger_pack, diagnostic_codes, evaluate_ledger_template, ledger_pack_sha256,
    ledger_unit_bindings_sha256, seal_ledger_pack, validate_ledger_pack,
    verify_conservation_certificate,
};
use norn_ir::quantity_ledger::{
    LEDGER_PACK_IR_VERSION, LedgerAccountIr, LedgerLayerIr, LedgerLegIr, LedgerOntologyArtifactPin,
    LedgerPackIr, LedgerParameterIr, LedgerParameterValue, LedgerTemplateIr, LedgerUnitIr,
    LinearAmountIr, LinearTermIr,
};

fn ontology_pin() -> LedgerOntologyArtifactPin {
    LedgerOntologyArtifactPin {
        artifact_id: "norn.ontology-pack.quantity-units".to_string(),
        artifact_version: "0.1.0".to_string(),
        artifact_sha256: "45be55780443fa2ece436ba8ab89107dd4ffcef5388beb3d332142c033ecf7d1"
            .to_string(),
    }
}
use proptest::prelude::*;

#[derive(Clone, Debug)]
struct BalancedSeed {
    parameter_scales: [u8; 2],
    coefficient_scales: [u8; 2],
    precision_gap: u8,
    // The first value is generated non-zero so moving the first leg across a
    // unit or layer boundary is guaranteed to break symbolic conservation.
    values: Vec<i16>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OracleLeg {
    leg_id: String,
    account_id: String,
    unit_id: String,
    layer: String,
    unit_precision: u8,
    amount_unscaled: i128,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OracleError {
    Invalid,
    Overflow,
}

#[derive(Clone, Debug)]
struct OracleParameter {
    scale: u8,
    min: i128,
    max: i128,
}

#[derive(Clone, Debug)]
struct OracleAmount {
    constant: i128,
    coefficients: BTreeMap<String, i128>,
}

fn nonzero_small_i16() -> impl Strategy<Value = i16> {
    (-4_i16..=4).prop_filter("the first coefficient must be non-zero", |value| {
        *value != 0
    })
}

fn balanced_seed_strategy() -> impl Strategy<Value = BalancedSeed> {
    (
        (0_u8..=2, 0_u8..=2, 0_u8..=1, 0_u8..=1, 0_u8..=2),
        (nonzero_small_i16(), prop::collection::vec(-4_i16..=4, 8)),
    )
        .prop_map(
            |(
                (parameter_a, parameter_b, coefficient_a, coefficient_b, precision_gap),
                (first, mut rest),
            )| {
                let mut values = vec![first];
                values.append(&mut rest);
                BalancedSeed {
                    parameter_scales: [parameter_a, parameter_b],
                    coefficient_scales: [coefficient_a, coefficient_b],
                    precision_gap,
                    values,
                }
            },
        )
}

fn parameter(parameter_id: &str, scale: u8) -> LedgerParameterIr {
    LedgerParameterIr {
        parameter_id: parameter_id.to_string(),
        scale,
        min_unscaled: "-2".to_string(),
        max_unscaled: "2".to_string(),
    }
}

fn linear_amount(
    constant: i128,
    coefficients: [i128; 2],
    seed: &BalancedSeed,
    result_scale: u8,
) -> LinearAmountIr {
    LinearAmountIr {
        result_scale,
        constant_unscaled: constant.to_string(),
        constant_scale: 0,
        terms: vec![
            LinearTermIr {
                parameter_id: "alpha".to_string(),
                coefficient_unscaled: coefficients[0].to_string(),
                coefficient_scale: seed.coefficient_scales[0],
            },
            LinearTermIr {
                parameter_id: "zeta".to_string(),
                coefficient_unscaled: coefficients[1].to_string(),
                coefficient_scale: seed.coefficient_scales[1],
            },
        ],
    }
}

fn leg(
    leg_id: &str,
    unit_id: &str,
    layer: &str,
    constant: i128,
    coefficients: [i128; 2],
    seed: &BalancedSeed,
    result_scale: u8,
) -> LedgerLegIr {
    LedgerLegIr {
        leg_id: leg_id.to_string(),
        account_id: format!("account-{leg_id}"),
        unit_id: unit_id.to_string(),
        layer: layer.to_string(),
        amount: linear_amount(constant, coefficients, seed, result_scale),
    }
}

fn generated_balanced_pack(seed: &BalancedSeed) -> LedgerPackIr {
    let result_scale = seed
        .parameter_scales
        .iter()
        .zip(seed.coefficient_scales)
        .map(|(parameter_scale, coefficient_scale)| parameter_scale + coefficient_scale)
        .max()
        .unwrap_or(0);
    let precision = result_scale + seed.precision_gap;
    let value = |index: usize| i128::from(seed.values[index]);

    let first_coefficients = [value(0), value(1)];
    let second_coefficients = [value(2), value(3)];
    let first_constant = value(4);
    let second_constant = value(5);
    let two_leg_coefficients = [value(6), value(7)];
    let two_leg_constant = value(8);

    let three_leg_partition = vec![
        leg(
            "a-one",
            "unit-a",
            "posted",
            first_constant,
            first_coefficients,
            seed,
            result_scale,
        ),
        leg(
            "a-two",
            "unit-a",
            "posted",
            second_constant,
            second_coefficients,
            seed,
            result_scale,
        ),
        leg(
            "a-balance",
            "unit-a",
            "posted",
            -(first_constant + second_constant),
            [
                -(first_coefficients[0] + second_coefficients[0]),
                -(first_coefficients[1] + second_coefficients[1]),
            ],
            seed,
            result_scale,
        ),
    ];
    let two_leg_partition = vec![
        leg(
            "b-one",
            "unit-b",
            "pending",
            two_leg_constant,
            two_leg_coefficients,
            seed,
            result_scale,
        ),
        leg(
            "b-balance",
            "unit-b",
            "pending",
            -two_leg_constant,
            [-two_leg_coefficients[0], -two_leg_coefficients[1]],
            seed,
            result_scale,
        ),
    ];

    LedgerPackIr {
        ir_version: LEDGER_PACK_IR_VERSION.to_string(),
        pack_id: "property.quantity-ledger".to_string(),
        ontology_artifact: ontology_pin(),
        units: vec![
            LedgerUnitIr {
                unit_id: "unit-a".to_string(),
                unit_term_id: "norn.measure.unit.property-a".to_string(),
                quantity_kind_term_id: "norn.measure.quantity-kind.mass".to_string(),
                precision,
            },
            LedgerUnitIr {
                unit_id: "unit-b".to_string(),
                unit_term_id: "norn.measure.unit.property-b".to_string(),
                quantity_kind_term_id: "norn.measure.quantity-kind.duration".to_string(),
                precision,
            },
        ],
        accounts: [
            ("account-a-one", "unit-a"),
            ("account-a-two", "unit-a"),
            ("account-a-balance", "unit-a"),
            ("account-b-one", "unit-b"),
            ("account-b-balance", "unit-b"),
        ]
        .into_iter()
        .map(|(account_id, unit_id)| LedgerAccountIr {
            funding: norn_ir::quantity_ledger::LedgerFundingPolicy::Gross,
            account_id: account_id.to_string(),
            allowed_unit_ids: vec![unit_id.to_string()],
        })
        .collect(),
        layers: vec![
            LedgerLayerIr {
                layer_id: "posted".to_string(),
            },
            LedgerLayerIr {
                layer_id: "pending".to_string(),
            },
        ],
        templates: vec![LedgerTemplateIr {
            template_id: "generated-move".to_string(),
            parameters: vec![
                parameter("zeta", seed.parameter_scales[1]),
                // Deliberately declared after zeta; runtime canonicalizes by ID.
                parameter("alpha", seed.parameter_scales[0]),
            ],
            legs: three_leg_partition
                .into_iter()
                .chain(two_leg_partition)
                .collect(),
        }],
    }
}

fn checked_pow10(exponent: u8) -> Result<i128, OracleError> {
    let mut value = 1_i128;
    for _ in 0..exponent {
        value = value.checked_mul(10).ok_or(OracleError::Overflow)?;
    }
    Ok(value)
}

fn oracle_parameters(
    template: &LedgerTemplateIr,
) -> Result<BTreeMap<String, OracleParameter>, OracleError> {
    template
        .parameters
        .iter()
        .map(|parameter| {
            let min = parameter
                .min_unscaled
                .parse::<i128>()
                .map_err(|_parse_error| OracleError::Invalid)?;
            let max = parameter
                .max_unscaled
                .parse::<i128>()
                .map_err(|_parse_error| OracleError::Invalid)?;
            if min > max {
                return Err(OracleError::Invalid);
            }
            Ok((
                parameter.parameter_id.clone(),
                OracleParameter {
                    scale: parameter.scale,
                    min,
                    max,
                },
            ))
        })
        .collect()
}

/// Independent direct-to-unit normalization. Unlike the production checker,
/// this does not normalize through `result_scale`; it derives the equivalent
/// decimal exponent from the source scales and unit precision in one step.
fn oracle_normalize(
    amount: &LinearAmountIr,
    unit_precision: u8,
    parameters: &BTreeMap<String, OracleParameter>,
) -> Result<OracleAmount, OracleError> {
    if amount.result_scale > unit_precision || amount.constant_scale > amount.result_scale {
        return Err(OracleError::Invalid);
    }
    let constant_source = amount
        .constant_unscaled
        .parse::<i128>()
        .map_err(|_parse_error| OracleError::Invalid)?;
    let constant_multiplier = checked_pow10(
        unit_precision
            .checked_sub(amount.constant_scale)
            .ok_or(OracleError::Invalid)?,
    )?;
    let constant = constant_source
        .checked_mul(constant_multiplier)
        .ok_or(OracleError::Overflow)?;

    let mut coefficients = BTreeMap::new();
    for term in &amount.terms {
        let parameter = parameters
            .get(term.parameter_id.as_str())
            .ok_or(OracleError::Invalid)?;
        let source_scale = parameter
            .scale
            .checked_add(term.coefficient_scale)
            .ok_or(OracleError::Invalid)?;
        if source_scale > amount.result_scale {
            return Err(OracleError::Invalid);
        }
        let coefficient = term
            .coefficient_unscaled
            .parse::<i128>()
            .map_err(|_parse_error| OracleError::Invalid)?;
        let multiplier = checked_pow10(
            unit_precision
                .checked_sub(source_scale)
                .ok_or(OracleError::Invalid)?,
        )?;
        let normalized = coefficient
            .checked_mul(multiplier)
            .ok_or(OracleError::Overflow)?;
        if coefficients
            .insert(term.parameter_id.clone(), normalized)
            .is_some()
        {
            return Err(OracleError::Invalid);
        }
    }

    let normalized = OracleAmount {
        constant,
        coefficients,
    };
    oracle_assert_bounds(&normalized, parameters)?;
    Ok(normalized)
}

fn oracle_assert_bounds(
    amount: &OracleAmount,
    parameters: &BTreeMap<String, OracleParameter>,
) -> Result<(), OracleError> {
    // Verify that every endpoint is evaluable. Small generated cases are then
    // exhaustively enumerated, independently of this interval calculation.
    let mut range_min = amount.constant;
    let mut range_max = amount.constant;
    for (parameter_id, coefficient) in &amount.coefficients {
        let parameter = parameters
            .get(parameter_id.as_str())
            .ok_or(OracleError::Invalid)?;
        let endpoint_a = coefficient
            .checked_mul(parameter.min)
            .ok_or(OracleError::Overflow)?;
        let endpoint_b = coefficient
            .checked_mul(parameter.max)
            .ok_or(OracleError::Overflow)?;
        range_min = range_min
            .checked_add(endpoint_a.min(endpoint_b))
            .ok_or(OracleError::Overflow)?;
        range_max = range_max
            .checked_add(endpoint_a.max(endpoint_b))
            .ok_or(OracleError::Overflow)?;
    }
    Ok(())
}

fn oracle_symbolically_balanced(pack: &LedgerPackIr) -> Result<bool, OracleError> {
    let units = pack
        .units
        .iter()
        .map(|unit| (unit.unit_id.as_str(), unit.precision))
        .collect::<BTreeMap<_, _>>();
    let template = pack.templates.first().ok_or(OracleError::Invalid)?;
    let parameters = oracle_parameters(template)?;
    let mut residuals: BTreeMap<(String, String), OracleAmount> = BTreeMap::new();

    for leg in &template.legs {
        let precision = units
            .get(leg.unit_id.as_str())
            .copied()
            .ok_or(OracleError::Invalid)?;
        let amount = oracle_normalize(&leg.amount, precision, &parameters)?;
        let residual = residuals
            .entry((leg.unit_id.clone(), leg.layer.clone()))
            .or_insert_with(|| OracleAmount {
                constant: 0,
                coefficients: BTreeMap::new(),
            });
        residual.constant = residual
            .constant
            .checked_add(amount.constant)
            .ok_or(OracleError::Overflow)?;
        for (parameter_id, coefficient) in amount.coefficients {
            let previous = residual
                .coefficients
                .get(parameter_id.as_str())
                .copied()
                .unwrap_or(0);
            residual.coefficients.insert(
                parameter_id,
                previous
                    .checked_add(coefficient)
                    .ok_or(OracleError::Overflow)?,
            );
        }
        // A zero final expression is not sufficient: the actual runtime adds
        // legs in order, so every concrete partition prefix must fit i128.
        oracle_assert_bounds(residual, &parameters)?;
    }

    Ok(residuals.values().all(|residual| {
        residual.constant == 0
            && residual
                .coefficients
                .values()
                .all(|coefficient| *coefficient == 0)
    }))
}

fn oracle_evaluate(
    pack: &LedgerPackIr,
    inputs: &BTreeMap<String, i128>,
) -> Result<Vec<OracleLeg>, OracleError> {
    let units = pack
        .units
        .iter()
        .map(|unit| (unit.unit_id.as_str(), unit.precision))
        .collect::<BTreeMap<_, _>>();
    let template = pack.templates.first().ok_or(OracleError::Invalid)?;
    let parameters = oracle_parameters(template)?;
    let mut result = Vec::with_capacity(template.legs.len());

    for leg in &template.legs {
        let precision = units
            .get(leg.unit_id.as_str())
            .copied()
            .ok_or(OracleError::Invalid)?;
        let amount = oracle_normalize(&leg.amount, precision, &parameters)?;
        let mut evaluated = amount.constant;
        for (parameter_id, coefficient) in &amount.coefficients {
            let value = inputs
                .get(parameter_id.as_str())
                .copied()
                .ok_or(OracleError::Invalid)?;
            let parameter = parameters
                .get(parameter_id.as_str())
                .ok_or(OracleError::Invalid)?;
            if value < parameter.min || value > parameter.max {
                return Err(OracleError::Invalid);
            }
            evaluated = evaluated
                .checked_add(
                    coefficient
                        .checked_mul(value)
                        .ok_or(OracleError::Overflow)?,
                )
                .ok_or(OracleError::Overflow)?;
        }
        result.push(OracleLeg {
            leg_id: leg.leg_id.clone(),
            account_id: leg.account_id.clone(),
            unit_id: leg.unit_id.clone(),
            layer: leg.layer.clone(),
            unit_precision: precision,
            amount_unscaled: evaluated,
        });
    }
    Ok(result)
}

fn runtime_inputs(pack: &LedgerPackIr, alpha: i128, zeta: i128) -> Vec<LedgerParameterValue> {
    let template = &pack.templates[0];
    template
        .parameters
        .iter()
        .map(|parameter| LedgerParameterValue {
            parameter_id: parameter.parameter_id.clone(),
            scale: parameter.scale,
            unscaled: if parameter.parameter_id == "alpha" {
                alpha.to_string()
            } else {
                zeta.to_string()
            },
        })
        .collect()
}

fn assert_exhaustive_runtime_matches_oracle(pack: &LedgerPackIr) -> Result<(), String> {
    let compiled = compile_ledger_pack(pack)
        .map_err(|validation| format!("production seal rejected: {:#?}", validation.diagnostics))?;

    for alpha in -2_i128..=2 {
        for zeta in -2_i128..=2 {
            let inputs = BTreeMap::from([("alpha".to_string(), alpha), ("zeta".to_string(), zeta)]);
            let oracle = oracle_evaluate(pack, &inputs)
                .map_err(|error| format!("oracle evaluation failed: {error:?}"))?;
            let actual = evaluate_ledger_template(
                &compiled,
                "generated-move",
                &runtime_inputs(pack, alpha, zeta),
            )
            .map_err(|diagnostics| format!("runtime evaluation failed: {diagnostics:#?}"))?;

            let actual_legs = actual
                .legs
                .iter()
                .map(|leg| {
                    Ok(OracleLeg {
                        leg_id: leg.leg_id.clone(),
                        account_id: leg.account_id.clone(),
                        unit_id: leg.unit_id.clone(),
                        layer: leg.layer.clone(),
                        unit_precision: leg.unit_precision,
                        amount_unscaled: leg
                            .amount_unscaled
                            .parse::<i128>()
                            .map_err(|error| format!("invalid runtime integer: {error}"))?,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
            if actual_legs != oracle {
                return Err(format!(
                    "runtime/oracle mismatch for alpha={alpha}, zeta={zeta}: actual={actual_legs:?}, oracle={oracle:?}"
                ));
            }

            let mut residuals = BTreeMap::<(String, String), i128>::new();
            for leg in oracle {
                let key = (leg.unit_id, leg.layer);
                let previous = residuals.get(&key).copied().unwrap_or(0);
                residuals.insert(
                    key,
                    previous
                        .checked_add(leg.amount_unscaled)
                        .ok_or_else(|| "oracle residual overflow".to_string())?,
                );
            }
            if residuals.values().any(|residual| *residual != 0) {
                return Err(format!(
                    "oracle found non-zero runtime residuals for alpha={alpha}, zeta={zeta}: {residuals:?}"
                ));
            }
        }
    }
    Ok(())
}

fn has_conservation_diagnostic(pack: &LedgerPackIr) -> bool {
    validate_ledger_pack(pack)
        .diagnostics
        .iter()
        .any(|diagnostic| {
            diagnostic.code == diagnostic_codes::UNBALANCED_COEFFICIENT
                || diagnostic.code == diagnostic_codes::UNBALANCED_CONSTANT
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn ontology_dimension_substitution_never_preserves_pack_or_certificate_identity(
        replacement in prop::sample::select(vec![
            "norn.measure.quantity-kind.count",
            "norn.measure.quantity-kind.duration",
            "norn.measure.quantity-kind.ratio",
        ]),
    ) {
        let seed = BalancedSeed {
            parameter_scales: [0, 0],
            coefficient_scales: [0, 0],
            precision_gap: 0,
            values: vec![1, 0, 0, 0, 0, 0, 0, 0, 0],
        };
        let original = generated_balanced_pack(&seed);
        let seal = seal_ledger_pack(&original).expect("generated baseline must seal");
        let certificate = seal.certificates()[0].clone();
        let mut substituted = original.clone();
        substituted.units[0].quantity_kind_term_id = replacement.to_string();

        prop_assert_ne!(ledger_pack_sha256(&original), ledger_pack_sha256(&substituted));
        prop_assert_ne!(
            ledger_unit_bindings_sha256(&original),
            ledger_unit_bindings_sha256(&substituted)
        );
        prop_assert!(verify_conservation_certificate(&substituted, &certificate).is_err());
    }

    // INV-QL-CONSERVATION and CASE-JOURNAL-BALANCED-PER-UNIT-LAYER:
    // generated exact-linear entries conserve independently in each partition.
    #[test]
    fn generated_balanced_packs_match_independent_oracle(seed in balanced_seed_strategy()) {
        let pack = generated_balanced_pack(&seed);
        let oracle_balanced = oracle_symbolically_balanced(&pack)
            .map_err(|error| TestCaseError::fail(format!("oracle seal failed: {error:?}")))?;
        prop_assert!(oracle_balanced);

        let validation = validate_ledger_pack(&pack);
        prop_assert!(validation.ok, "{:#?}", validation.diagnostics);
        prop_assert_eq!(validation.certificates.len(), 2);
        assert_exhaustive_runtime_matches_oracle(&pack)
            .map_err(TestCaseError::fail)?;
    }

    // CASE-JOURNAL-UNBALANCED and partition-isolation cases: a single source
    // mutation must not be hidden by another leg, unit, or lifecycle layer.
    #[test]
    fn coefficient_constant_unit_and_layer_mutations_fail_closed(seed in balanced_seed_strategy()) {
        let pack = generated_balanced_pack(&seed);

        let mut coefficient = pack.clone();
        let value = coefficient.templates[0].legs[0].amount.terms[0]
            .coefficient_unscaled.parse::<i128>().expect("generated coefficient");
        coefficient.templates[0].legs[0].amount.terms[0].coefficient_unscaled =
            (value + 1).to_string();

        let mut constant = pack.clone();
        let value = constant.templates[0].legs[0].amount.constant_unscaled
            .parse::<i128>().expect("generated constant");
        constant.templates[0].legs[0].amount.constant_unscaled = (value + 1).to_string();

        let mut unit = pack.clone();
        unit.templates[0].legs[0].unit_id = "unit-b".to_string();

        let mut layer = pack;
        layer.templates[0].legs[0].layer = "pending".to_string();

        for (name, mutation) in [
            ("coefficient", coefficient),
            ("constant", constant),
            ("unit", unit),
            ("layer", layer),
        ] {
            let oracle_balanced = oracle_symbolically_balanced(&mutation)
                .map_err(|error| TestCaseError::fail(format!("{name} oracle failed: {error:?}")))?;
            prop_assert!(!oracle_balanced, "{name} mutation remained oracle-balanced");
            prop_assert!(!validate_ledger_pack(&mutation).ok, "{name} mutation sealed");
            prop_assert!(has_conservation_diagnostic(&mutation), "{name} mutation lacked a conservation diagnostic");
        }
    }
}

#[test]
fn scaled_negative_multi_parameter_case_matches_brute_force_oracle() {
    // INV-QL-EXACT-SCALED-I128: alpha uses scale 2 and a scale-1 coefficient;
    // zeta uses scale 1, negative coefficients, and declaration order differs
    // from runtime's sorted parameter order.
    let seed = BalancedSeed {
        parameter_scales: [2, 1],
        coefficient_scales: [1, 0],
        precision_gap: 2,
        values: vec![-3, 4, 2, -2, 1, -3, -4, 3, 2],
    };
    let pack = generated_balanced_pack(&seed);
    assert_eq!(oracle_symbolically_balanced(&pack), Ok(true));
    assert_exhaustive_runtime_matches_oracle(&pack).expect("all bounded assignments must agree");
}

#[test]
fn balanced_symbolic_expression_with_unsafe_bounds_is_rejected_by_both_checkers() {
    // INV-QL-NUMERIC-BOUNDS: zero symbolic residual is insufficient when a
    // legal runtime input can overflow a coefficient multiplication.
    let seed = BalancedSeed {
        parameter_scales: [0, 0],
        coefficient_scales: [0, 0],
        precision_gap: 0,
        values: vec![2, 0, 0, 0, 0, 0, 0, 0, 0],
    };
    let mut pack = generated_balanced_pack(&seed);
    for parameter in &mut pack.templates[0].parameters {
        if parameter.parameter_id == "alpha" {
            parameter.min_unscaled = i128::MAX.to_string();
            parameter.max_unscaled = i128::MAX.to_string();
        }
    }

    assert_eq!(
        oracle_symbolically_balanced(&pack),
        Err(OracleError::Overflow)
    );
    let validation = validate_ledger_pack(&pack);
    assert!(!validation.ok);
    assert!(
        validation
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.code == diagnostic_codes::BOUND_OVERFLOW })
    );
}

#[test]
fn cross_leg_prefix_overflow_is_rejected_by_production_and_independent_oracle() {
    // INV-QL-NUMERIC-BOUNDS: each leg and the final zero residual fit, while
    // the concrete prefix after leg two is i128::MAX + 1.
    let seed = BalancedSeed {
        parameter_scales: [0, 0],
        coefficient_scales: [0, 0],
        precision_gap: 0,
        values: vec![1, 0, 0, 0, 0, 0, 0, 0, 0],
    };
    let mut pack = generated_balanced_pack(&seed);
    for parameter in &mut pack.templates[0].parameters {
        parameter.min_unscaled = "1".to_string();
        parameter.max_unscaled = "1".to_string();
    }
    pack.templates[0].legs = vec![
        leg("first", "unit-a", "posted", 0, [i128::MAX, 0], &seed, 0),
        leg("second", "unit-a", "posted", 0, [0, 1], &seed, 0),
        leg("third", "unit-a", "posted", 0, [-i128::MAX, 0], &seed, 0),
        leg("fourth", "unit-a", "posted", 0, [0, -1], &seed, 0),
    ];

    assert_eq!(
        oracle_symbolically_balanced(&pack),
        Err(OracleError::Overflow)
    );
    let validation = validate_ledger_pack(&pack);
    assert!(!validation.ok);
    assert!(validation.certificates.is_empty());
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == diagnostic_codes::BOUND_OVERFLOW
            && diagnostic
                .message
                .contains("runtime partition prefix through leg `second`")
    }));
}
