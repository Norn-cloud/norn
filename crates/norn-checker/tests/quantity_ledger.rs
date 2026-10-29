#![allow(
    clippy::expect_used,
    clippy::pedantic,
    clippy::unwrap_used,
    missing_docs,
    reason = "fixture-oriented contract tests use explicit panic-on-broken-fixture assertions"
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

use std::collections::{BTreeMap, BTreeSet};

use norn_checker::quantity_ledger::{
    compile_ledger_pack, diagnostic_codes, evaluate_ledger_template, ledger_pack_sha256,
    ledger_unit_bindings_sha256, seal_ledger_pack, validate_ledger_pack,
    verify_conservation_certificate,
};
use norn_ir::quantity_ledger::{
    LEDGER_PACK_CHECKER_VERSION, LEDGER_PACK_IR_VERSION, LedgerAccountIr, LedgerLayerIr,
    LedgerLegIr, LedgerOntologyArtifactPin, LedgerPackIr, LedgerParameterIr, LedgerParameterValue,
    LedgerTemplateIr, LedgerUnitIr, LinearAmountIr, LinearTermIr,
};

fn ontology_pin() -> LedgerOntologyArtifactPin {
    LedgerOntologyArtifactPin {
        artifact_id: "norn.ontology-pack.quantity-units".to_string(),
        artifact_version: "0.1.0".to_string(),
        artifact_sha256: "45be55780443fa2ece436ba8ab89107dd4ffcef5388beb3d332142c033ecf7d1"
            .to_string(),
    }
}

fn ledger_unit(unit_id: String, precision: u8) -> LedgerUnitIr {
    LedgerUnitIr {
        unit_term_id: format!("norn.measure.unit.{unit_id}"),
        quantity_kind_term_id: if unit_id == "kg" || unit_id == "litre" {
            "norn.measure.quantity-kind.mass".to_string()
        } else {
            "norn.measure.quantity-kind.count".to_string()
        },
        unit_id,
        precision,
    }
}
use serde::Deserialize;

const SHARED_FIXTURES: &str =
    include_str!("../../../verification/stripmine-2026-08-22/fixtures/quantity-ledger.json");

#[derive(Debug, Deserialize)]
struct FixtureCorpus {
    cases: Vec<FixtureCase>,
}

#[derive(Debug, Deserialize)]
struct FixtureCase {
    id: String,
    expect: String,
    legs: Vec<FixtureLeg>,
}

#[derive(Debug, Deserialize)]
struct FixtureLeg {
    account: String,
    unit_id: String,
    layer: String,
    amount: i128,
}

fn constant_amount(value: i128) -> LinearAmountIr {
    LinearAmountIr {
        result_scale: 0,
        constant_unscaled: value.to_string(),
        constant_scale: 0,
        terms: Vec::new(),
    }
}

fn fixture_pack(case: &FixtureCase) -> LedgerPackIr {
    let units = case
        .legs
        .iter()
        .map(|leg| leg.unit_id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|unit_id| ledger_unit(unit_id, 0))
        .collect();
    let mut account_units = BTreeMap::<String, BTreeSet<String>>::new();
    for leg in &case.legs {
        account_units
            .entry(leg.account.clone())
            .or_default()
            .insert(leg.unit_id.clone());
    }
    let accounts = account_units
        .into_iter()
        .map(|(account_id, allowed_unit_ids)| LedgerAccountIr {
            funding: norn_ir::quantity_ledger::LedgerFundingPolicy::Gross,
            account_id,
            allowed_unit_ids: allowed_unit_ids.into_iter().collect(),
        })
        .collect();
    let layers = case
        .legs
        .iter()
        .map(|leg| leg.layer.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|layer_id| LedgerLayerIr { layer_id })
        .collect();
    let legs = case
        .legs
        .iter()
        .enumerate()
        .map(|(index, leg)| LedgerLegIr {
            leg_id: format!("leg-{index}"),
            account_id: leg.account.clone(),
            unit_id: leg.unit_id.clone(),
            layer: leg.layer.clone(),
            amount: constant_amount(leg.amount),
        })
        .collect();
    LedgerPackIr {
        ir_version: LEDGER_PACK_IR_VERSION.to_string(),
        pack_id: format!("fixture.{}", case.id),
        ontology_artifact: ontology_pin(),
        units,
        accounts,
        layers,
        templates: vec![LedgerTemplateIr {
            template_id: case.id.clone(),
            parameters: Vec::new(),
            legs,
        }],
    }
}

fn parameter() -> LedgerParameterIr {
    LedgerParameterIr {
        parameter_id: "quantity".to_string(),
        scale: 2,
        min_unscaled: "-1000000".to_string(),
        max_unscaled: "1000000".to_string(),
    }
}

fn parameter_amount(coefficient: &str) -> LinearAmountIr {
    LinearAmountIr {
        result_scale: 2,
        constant_unscaled: "0".to_string(),
        constant_scale: 0,
        terms: vec![LinearTermIr {
            parameter_id: "quantity".to_string(),
            coefficient_unscaled: coefficient.to_string(),
            coefficient_scale: 0,
        }],
    }
}

fn symbolic_pack(right_coefficient: &str) -> LedgerPackIr {
    LedgerPackIr {
        ir_version: LEDGER_PACK_IR_VERSION.to_string(),
        pack_id: "pack.symbolic".to_string(),
        ontology_artifact: ontology_pin(),
        units: vec![ledger_unit("kg".to_string(), 2)],
        accounts: vec![
            LedgerAccountIr {
                funding: norn_ir::quantity_ledger::LedgerFundingPolicy::Gross,
                account_id: "warehouse-a".to_string(),
                allowed_unit_ids: vec!["kg".to_string()],
            },
            LedgerAccountIr {
                funding: norn_ir::quantity_ledger::LedgerFundingPolicy::Gross,
                account_id: "warehouse-b".to_string(),
                allowed_unit_ids: vec!["kg".to_string()],
            },
        ],
        layers: vec![LedgerLayerIr {
            layer_id: "posted".to_string(),
        }],
        templates: vec![LedgerTemplateIr {
            template_id: "move".to_string(),
            parameters: vec![parameter()],
            legs: vec![
                LedgerLegIr {
                    leg_id: "source".to_string(),
                    account_id: "warehouse-a".to_string(),
                    unit_id: "kg".to_string(),
                    layer: "posted".to_string(),
                    amount: parameter_amount("-1"),
                },
                LedgerLegIr {
                    leg_id: "target".to_string(),
                    account_id: "warehouse-b".to_string(),
                    unit_id: "kg".to_string(),
                    layer: "posted".to_string(),
                    amount: parameter_amount(right_coefficient),
                },
            ],
        }],
    }
}

#[test]
fn shared_fixture_cases_enforce_unit_and_layer_partitioning() {
    let corpus: FixtureCorpus =
        serde_json::from_str(SHARED_FIXTURES).expect("shared fixture must remain valid JSON");
    for case in &corpus.cases {
        let validation = validate_ledger_pack(&fixture_pack(case));
        if case.expect == "accept" {
            assert!(
                validation.ok,
                "fixture {} should pass: {:#?}",
                case.id, validation.diagnostics
            );
            assert!(!validation.certificates.is_empty());
        } else {
            assert!(!validation.ok, "fixture {} should be rejected", case.id);
            assert!(validation.certificates.is_empty());
            assert!(
                validation
                    .diagnostics
                    .iter()
                    .any(|diagnostic| { diagnostic.code == diagnostic_codes::UNBALANCED_CONSTANT })
            );
        }
    }
}

#[test]
fn symbolic_coefficients_balance_for_every_declared_parameter_value() {
    let validation = validate_ledger_pack(&symbolic_pack("1"));
    assert!(validation.ok, "{:#?}", validation.diagnostics);
    assert_eq!(validation.certificates.len(), 1);
    assert_eq!(
        validation.certificates[0].checker_version,
        LEDGER_PACK_CHECKER_VERSION
    );
    assert_eq!(
        validation.certificates[0]
            .parameter_residuals_unscaled
            .get("quantity")
            .map(String::as_str),
        Some("0")
    );
}

#[test]
fn unsupported_ir_version_is_rejected_before_activation() {
    let mut pack = symbolic_pack("1");
    pack.ir_version = "99.0".to_string();
    let validation = validate_ledger_pack(&pack);
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == diagnostic_codes::UNSUPPORTED_IR_VERSION
            && diagnostic.path.as_deref() == Some("irVersion")
    }));
    assert!(compile_ledger_pack(&pack).is_err());
}

#[test]
fn ontology_artifact_and_unit_dimension_bindings_fail_closed() {
    let base = symbolic_pack("1");
    let attacks = [
        (
            {
                let mut candidate = base.clone();
                candidate.ontology_artifact.artifact_id.clear();
                candidate
            },
            diagnostic_codes::EMPTY_ONTOLOGY_ARTIFACT_ID,
        ),
        (
            {
                let mut candidate = base.clone();
                candidate.ontology_artifact.artifact_id = "*".to_string();
                candidate
            },
            diagnostic_codes::NON_EXACT_ONTOLOGY_ARTIFACT_ID,
        ),
        (
            {
                let mut candidate = base.clone();
                candidate.ontology_artifact.artifact_version.clear();
                candidate
            },
            diagnostic_codes::EMPTY_ONTOLOGY_ARTIFACT_VERSION,
        ),
        (
            {
                let mut candidate = base.clone();
                candidate.ontology_artifact.artifact_version = "^0.1".to_string();
                candidate
            },
            diagnostic_codes::NON_EXACT_ONTOLOGY_ARTIFACT_VERSION,
        ),
        (
            {
                let mut candidate = base.clone();
                candidate.ontology_artifact.artifact_sha256 = "ABC".to_string();
                candidate
            },
            diagnostic_codes::INVALID_ONTOLOGY_ARTIFACT_SHA256,
        ),
        (
            {
                let mut candidate = base.clone();
                candidate.units[0].unit_term_id.clear();
                candidate
            },
            diagnostic_codes::EMPTY_UNIT_TERM,
        ),
        (
            {
                let mut candidate = base.clone();
                candidate.units[0].unit_term_id = "unit wildcard *".to_string();
                candidate
            },
            diagnostic_codes::INVALID_UNIT_TERM,
        ),
        (
            {
                let mut candidate = base.clone();
                candidate.units[0].quantity_kind_term_id.clear();
                candidate
            },
            diagnostic_codes::EMPTY_QUANTITY_KIND_TERM,
        ),
        (
            {
                let mut candidate = base.clone();
                candidate.units[0].quantity_kind_term_id = "dimension *".to_string();
                candidate
            },
            diagnostic_codes::INVALID_QUANTITY_KIND_TERM,
        ),
        (
            {
                let mut candidate = base.clone();
                candidate.units[0].quantity_kind_term_id = candidate.units[0].unit_term_id.clone();
                candidate
            },
            diagnostic_codes::INCONSISTENT_DIMENSION_BINDING,
        ),
        (
            {
                let mut candidate = base.clone();
                let mut duplicate = ledger_unit("gram".to_string(), 2);
                duplicate.unit_term_id = candidate.units[0].unit_term_id.clone();
                candidate.units.push(duplicate);
                candidate
            },
            diagnostic_codes::DUPLICATE_UNIT_TERM,
        ),
    ];

    for (candidate, expected_code) in attacks {
        let validation = validate_ledger_pack(&candidate);
        assert!(!validation.ok, "attack {expected_code} must reject");
        assert!(validation.certificates.is_empty());
        assert!(
            validation
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == expected_code),
            "missing diagnostic {expected_code}: {:#?}",
            validation.diagnostics
        );
    }

    let mut encoded = serde_json::to_value(base).expect("closed pack must encode");
    encoded["units"][0]["conversionFactor"] = serde_json::json!(1000);
    serde_json::from_value::<LedgerPackIr>(encoded)
        .expect_err("a conversion field is outside the closed ledger serialization surface");
}

#[test]
fn different_dimensions_cannot_balance_or_substitute_after_sealing() {
    let original = symbolic_pack("1");
    let seal = seal_ledger_pack(&original).expect("baseline mass pack must seal");
    let certificate = seal.certificates()[0].clone();
    assert_eq!(certificate.unit_term_id, original.units[0].unit_term_id);
    assert_eq!(
        certificate.quantity_kind_term_id,
        original.units[0].quantity_kind_term_id
    );

    let mut cross_dimension = original.clone();
    cross_dimension.units.push(LedgerUnitIr {
        unit_id: "hour".to_string(),
        unit_term_id: "norn.measure.unit.hour".to_string(),
        quantity_kind_term_id: "norn.measure.quantity-kind.duration".to_string(),
        precision: 2,
    });
    cross_dimension.accounts[1].allowed_unit_ids = vec!["hour".to_string()];
    cross_dimension.templates[0].legs[1].unit_id = "hour".to_string();
    let cross_validation = validate_ledger_pack(&cross_dimension);
    assert!(!cross_validation.ok);
    assert!(
        cross_validation
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.code == diagnostic_codes::UNBALANCED_COEFFICIENT })
    );

    let mut substituted = original.clone();
    substituted.units[0].quantity_kind_term_id = "norn.measure.quantity-kind.duration".to_string();
    assert_ne!(
        ledger_pack_sha256(&original),
        ledger_pack_sha256(&substituted)
    );
    assert_ne!(
        ledger_unit_bindings_sha256(&original),
        ledger_unit_bindings_sha256(&substituted)
    );
    verify_conservation_certificate(&substituted, &certificate)
        .expect_err("a certificate cannot be reused after dimension substitution");
}

#[test]
fn account_registry_binds_every_leg_to_an_allowed_unit() {
    let mut unknown_account = symbolic_pack("1");
    unknown_account.templates[0].legs[0].account_id = "warehouse-missing".to_string();
    let validation = validate_ledger_pack(&unknown_account);
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == diagnostic_codes::UNKNOWN_ACCOUNT
            && diagnostic.path.as_deref() == Some("templates[0].legs[0].accountId")
    }));

    let mut wrong_unit = symbolic_pack("1");
    wrong_unit.units.push(ledger_unit("litre".to_string(), 2));
    wrong_unit.accounts[0].allowed_unit_ids = vec!["litre".to_string()];
    let validation = validate_ledger_pack(&wrong_unit);
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == diagnostic_codes::ACCOUNT_UNIT_MISMATCH
            && diagnostic.path.as_deref() == Some("templates[0].legs[0].unitId")
    }));

    let mut unknown_declared_unit = symbolic_pack("1");
    unknown_declared_unit.accounts[0]
        .allowed_unit_ids
        .push("unregistered-unit".to_string());
    let validation = validate_ledger_pack(&unknown_declared_unit);
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == diagnostic_codes::ACCOUNT_UNKNOWN_UNIT
            && diagnostic.path.as_deref() == Some("accounts[0].allowedUnitIds[1]")
    }));
}

#[test]
fn exact_runtime_evaluator_uses_sealed_bounds_scale_and_normalized_coefficients() {
    let compiled =
        compile_ledger_pack(&symbolic_pack("1")).expect("symbolically balanced pack must compile");
    let evaluation = evaluate_ledger_template(
        &compiled,
        "move",
        &[LedgerParameterValue {
            parameter_id: "quantity".to_string(),
            scale: 2,
            unscaled: "12345".to_string(),
        }],
    )
    .expect("in-bound exact input must evaluate");

    assert_eq!(evaluation.legs.len(), 2);
    assert_eq!(evaluation.legs[0].amount_unscaled, "-12345");
    assert_eq!(evaluation.legs[1].amount_unscaled, "12345");
    assert!(evaluation.legs.iter().all(|leg| leg.unit_precision == 2));
}

#[test]
fn runtime_evaluator_rejects_wrong_scale_and_out_of_bounds_values() {
    let compiled =
        compile_ledger_pack(&symbolic_pack("1")).expect("symbolically balanced pack must compile");
    let wrong_scale = evaluate_ledger_template(
        &compiled,
        "move",
        &[LedgerParameterValue {
            parameter_id: "quantity".to_string(),
            scale: 1,
            unscaled: "123".to_string(),
        }],
    )
    .expect_err("wrong input scale must fail");
    assert!(
        wrong_scale
            .iter()
            .any(|diagnostic| { diagnostic.code == diagnostic_codes::INPUT_SCALE_MISMATCH })
    );

    let out_of_bounds = evaluate_ledger_template(
        &compiled,
        "move",
        &[LedgerParameterValue {
            parameter_id: "quantity".to_string(),
            scale: 2,
            unscaled: "1000001".to_string(),
        }],
    )
    .expect_err("out-of-bound input must fail");
    assert!(
        out_of_bounds
            .iter()
            .any(|diagnostic| { diagnostic.code == diagnostic_codes::INPUT_OUT_OF_BOUNDS })
    );
}

#[test]
fn duplicate_legs_and_undeclared_layers_fail_closed() {
    let mut pack = symbolic_pack("1");
    pack.templates[0].legs[1].leg_id = "source".to_string();
    pack.templates[0].legs[1].layer = "undeclared".to_string();
    let validation = validate_ledger_pack(&pack);
    let codes = validation
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.as_str())
        .collect::<BTreeSet<_>>();
    assert!(codes.contains(diagnostic_codes::DUPLICATE_LEG));
    assert!(codes.contains(diagnostic_codes::UNKNOWN_LAYER));
}

#[test]
fn one_coefficient_perturbation_is_rejected_symbolically() {
    let validation = validate_ledger_pack(&symbolic_pack("2"));
    assert!(!validation.ok);
    assert!(validation.certificates.is_empty());
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == diagnostic_codes::UNBALANCED_COEFFICIENT
            && diagnostic.message.contains("parameter `quantity`")
            && diagnostic.path.as_deref() == Some("templates[0]")
    }));
}

#[test]
fn scale_that_requires_division_is_rejected_without_rounding() {
    let mut pack = symbolic_pack("1");
    pack.templates[0].legs[0].amount.terms[0].coefficient_scale = 1;
    let validation = validate_ledger_pack(&pack);
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == diagnostic_codes::TERM_SCALE_EXCEEDS_RESULT
            && diagnostic.path.as_deref()
                == Some("templates[0].legs[0].amount.terms[0].coefficientScale")
    }));
}

#[test]
fn declared_bounds_that_allow_i128_overflow_are_rejected() {
    let mut pack = symbolic_pack("1");
    pack.units[0].precision = 0;
    pack.templates[0].parameters[0].scale = 0;
    pack.templates[0].parameters[0].min_unscaled = i128::MAX.to_string();
    pack.templates[0].parameters[0].max_unscaled = i128::MAX.to_string();
    for leg in &mut pack.templates[0].legs {
        leg.amount.result_scale = 0;
    }
    pack.templates[0].legs[1].amount.terms[0].coefficient_unscaled = "2".to_string();

    let validation = validate_ledger_pack(&pack);
    assert!(
        validation
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.code == diagnostic_codes::BOUND_OVERFLOW })
    );
}

#[test]
fn seal_bound_analysis_uses_the_exact_sorted_runtime_parameter_order() {
    let fixed_parameter = |parameter_id: &str| LedgerParameterIr {
        parameter_id: parameter_id.to_string(),
        scale: 0,
        min_unscaled: "1".to_string(),
        max_unscaled: "1".to_string(),
    };
    let term = |parameter_id: &str, coefficient: String| LinearTermIr {
        parameter_id: parameter_id.to_string(),
        coefficient_unscaled: coefficient,
        coefficient_scale: 0,
    };
    let max = i128::MAX.to_string();
    let negative_max = (-i128::MAX).to_string();
    let pack = LedgerPackIr {
        ir_version: LEDGER_PACK_IR_VERSION.to_string(),
        pack_id: "pack.order-adversary".to_string(),
        ontology_artifact: ontology_pin(),
        units: vec![ledger_unit("each".to_string(), 0)],
        accounts: ["left", "right"]
            .into_iter()
            .map(|account_id| LedgerAccountIr {
                funding: norn_ir::quantity_ledger::LedgerFundingPolicy::Gross,
                account_id: account_id.to_string(),
                allowed_unit_ids: vec!["each".to_string()],
            })
            .collect(),
        layers: vec![LedgerLayerIr {
            layer_id: "posted".to_string(),
        }],
        templates: vec![LedgerTemplateIr {
            template_id: "ordered-overflow".to_string(),
            parameters: vec![
                fixed_parameter("a"),
                fixed_parameter("b"),
                fixed_parameter("c"),
            ],
            legs: vec![
                LedgerLegIr {
                    leg_id: "left".to_string(),
                    account_id: "left".to_string(),
                    unit_id: "each".to_string(),
                    layer: "posted".to_string(),
                    amount: LinearAmountIr {
                        result_scale: 0,
                        constant_unscaled: "0".to_string(),
                        constant_scale: 0,
                        // Declaration order stays in range: MAX, then 0, then 1.
                        // Compiled order a,b,c overflows at MAX + 1.
                        terms: vec![
                            term("a", max.clone()),
                            term("c", negative_max.clone()),
                            term("b", "1".to_string()),
                        ],
                    },
                },
                LedgerLegIr {
                    leg_id: "right".to_string(),
                    account_id: "right".to_string(),
                    unit_id: "each".to_string(),
                    layer: "posted".to_string(),
                    amount: LinearAmountIr {
                        result_scale: 0,
                        constant_unscaled: "0".to_string(),
                        constant_scale: 0,
                        terms: vec![
                            term("a", negative_max),
                            term("c", max),
                            term("b", "-1".to_string()),
                        ],
                    },
                },
            ],
        }],
    };

    let validation = validate_ledger_pack(&pack);
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == diagnostic_codes::BOUND_OVERFLOW
            && diagnostic
                .message
                .contains("compiled runtime evaluation order")
    }));
    assert!(compile_ledger_pack(&pack).is_err());
}

#[test]
fn seal_rejects_cross_leg_runtime_partition_prefix_overflow() {
    // INV-QL-NUMERIC-BOUNDS: every leg is individually evaluable and the
    // final symbolic residual is zero, but the concrete residual after the
    // second leg would attempt i128::MAX + 1 at runtime.
    let fixed_parameter = |parameter_id: &str| LedgerParameterIr {
        parameter_id: parameter_id.to_string(),
        scale: 0,
        min_unscaled: "1".to_string(),
        max_unscaled: "1".to_string(),
    };
    let parameter_amount = |parameter_id: &str, coefficient: i128| LinearAmountIr {
        result_scale: 0,
        constant_unscaled: "0".to_string(),
        constant_scale: 0,
        terms: vec![LinearTermIr {
            parameter_id: parameter_id.to_string(),
            coefficient_unscaled: coefficient.to_string(),
            coefficient_scale: 0,
        }],
    };
    let leg = |leg_id: &str, parameter_id: &str, coefficient: i128| LedgerLegIr {
        leg_id: leg_id.to_string(),
        account_id: format!("account-{leg_id}"),
        unit_id: "each".to_string(),
        layer: "posted".to_string(),
        amount: parameter_amount(parameter_id, coefficient),
    };
    let pack = LedgerPackIr {
        ir_version: LEDGER_PACK_IR_VERSION.to_string(),
        pack_id: "pack.cross-leg-prefix-overflow".to_string(),
        ontology_artifact: ontology_pin(),
        units: vec![ledger_unit("each".to_string(), 0)],
        accounts: ["first", "second", "third", "fourth"]
            .into_iter()
            .map(|leg_id| LedgerAccountIr {
                funding: norn_ir::quantity_ledger::LedgerFundingPolicy::Gross,
                account_id: format!("account-{leg_id}"),
                allowed_unit_ids: vec!["each".to_string()],
            })
            .collect(),
        layers: vec![LedgerLayerIr {
            layer_id: "posted".to_string(),
        }],
        templates: vec![LedgerTemplateIr {
            template_id: "unsafe-prefix".to_string(),
            parameters: vec![fixed_parameter("alpha"), fixed_parameter("zeta")],
            legs: vec![
                leg("first", "alpha", i128::MAX),
                leg("second", "zeta", 1),
                leg("third", "alpha", -i128::MAX),
                leg("fourth", "zeta", -1),
            ],
        }],
    };

    let validation = validate_ledger_pack(&pack);
    assert!(!validation.ok);
    assert!(validation.certificates.is_empty());
    assert!(validation.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == diagnostic_codes::BOUND_OVERFLOW
            && diagnostic
                .message
                .contains("runtime partition prefix through leg `second`")
    }));
    assert!(compile_ledger_pack(&pack).is_err());
}

#[test]
fn numeric_wire_encoding_is_canonical_and_diagnostics_are_stable() {
    let mut pack = symbolic_pack("1");
    pack.templates[0].parameters[0].min_unscaled = "-00".to_string();
    pack.templates[0].parameters[0].max_unscaled = "-1".to_string();

    let validation = validate_ledger_pack(&pack);
    let diagnostic = validation
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == diagnostic_codes::NON_CANONICAL_INTEGER)
        .expect("non-canonical integer must produce its stable diagnostic");
    assert_eq!(
        diagnostic.path.as_deref(),
        Some("templates[0].parameters[0].minUnscaled")
    );
}

#[test]
fn malformed_constant_reports_numeric_error_without_derived_bound_noise() {
    let mut pack = symbolic_pack("1");
    pack.templates[0].legs[0].amount.constant_unscaled = "not-an-integer".to_string();

    let validation = validate_ledger_pack(&pack);
    assert!(
        validation
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == diagnostic_codes::INVALID_INTEGER)
    );
    assert!(
        !validation
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == diagnostic_codes::BOUND_OVERFLOW)
    );
}

#[test]
fn checker_wire_types_roundtrip_losslessly() {
    let pack = symbolic_pack("1");
    let encoded = serde_json::to_string(&pack).expect("pack must serialize");
    let decoded: LedgerPackIr = serde_json::from_str(&encoded).expect("pack must deserialize");
    assert_eq!(decoded, pack);
}

#[test]
fn pack_diagnostics_keep_first_findings_and_refuse_at_count_or_json_byte_budget() {
    use norn_checker::quantity_ledger::{MAX_LEDGER_DIAGNOSTIC_BYTES, MAX_LEDGER_DIAGNOSTICS};
    // Small IDs exhaust count; escaped IDs exhaust wire bytes before count;
    // the reviewer's huge ID cannot fit even one retained finding.
    for pack_id in [
        "pack.bounded".to_string(),
        "e\u{301}".repeat(4_096),
        "\\".repeat(2_048),
        "x".repeat(131_072),
    ] {
        let mut candidate = symbolic_pack("1");
        candidate.pack_id = pack_id;
        let unit_id = candidate.units[0].unit_id.clone();
        candidate.accounts[0].allowed_unit_ids = vec![unit_id; 16_000];
        let refusal = compile_ledger_pack(&candidate).expect_err("duplicate units must refuse");
        assert!(!refusal.ok);
        assert!(refusal.certificates.is_empty());
        assert!(refusal.diagnostics.len() <= MAX_LEDGER_DIAGNOSTICS);
        let bytes = serde_json::to_vec(&refusal.diagnostics).expect("diagnostic JSON");
        assert!(bytes.len() <= MAX_LEDGER_DIAGNOSTIC_BYTES);
        assert_eq!(
            refusal.diagnostics.last().expect("typed marker").code,
            diagnostic_codes::TOO_MANY_DIAGNOSTICS
        );
        assert!(
            refusal
                .diagnostics
                .last()
                .expect("typed marker")
                .artifact_id
                .is_none()
        );
        if candidate.pack_id == "pack.bounded" {
            assert_eq!(refusal.diagnostics.len(), MAX_LEDGER_DIAGNOSTICS);
            for (index, diagnostic) in refusal.diagnostics[..MAX_LEDGER_DIAGNOSTICS - 1]
                .iter()
                .enumerate()
            {
                assert_eq!(diagnostic.code, diagnostic_codes::ACCOUNT_DUPLICATE_UNIT);
                assert_eq!(
                    diagnostic.path,
                    Some(format!("accounts[0].allowedUnitIds[{}]", index + 1))
                );
            }
        } else {
            assert!(refusal.diagnostics.len() < MAX_LEDGER_DIAGNOSTICS);
        }
        assert_eq!(validate_ledger_pack(&candidate), refusal);
    }
}
