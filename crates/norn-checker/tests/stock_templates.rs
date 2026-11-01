//! Receipt/delivery template obligations; verification/posting-v1/templates.md.
#![allow(
    clippy::indexing_slicing,
    clippy::panic,
    reason = "typed synthetic fixtures and explicit assertions on checker output"
)]

use norn_checker::quantity_ledger::{
    compile_ledger_pack, evaluate_ledger_template, validate_ledger_pack,
    verify_conservation_certificate,
};
use norn_ir::quantity_ledger::{
    LEDGER_PACK_IR_VERSION, LedgerAccountIr, LedgerFundingPolicy, LedgerLayerIr, LedgerLegIr,
    LedgerOntologyArtifactPin, LedgerPackIr, LedgerParameterIr, LedgerParameterValue,
    LedgerTemplateIr, LedgerUnitIr, LinearAmountIr, LinearTermIr,
};

fn leg(account: &str, coefficient: i8, unit: &str, precision: u8) -> LedgerLegIr {
    LedgerLegIr {
        leg_id: account.into(),
        account_id: account.into(),
        unit_id: unit.into(),
        layer: "posted".into(),
        amount: LinearAmountIr {
            result_scale: precision,
            constant_unscaled: "0".into(),
            constant_scale: 0,
            terms: vec![LinearTermIr {
                parameter_id: "amount".into(),
                coefficient_unscaled: coefficient.to_string(),
                coefficient_scale: 0,
            }],
        },
    }
}

fn templates(gl: bool) -> LedgerPackIr {
    let (unit, precision, quantity_kind, accounts) = if gl {
        ("USD", 2, "currency", vec!["stock", "srbnb", "cogs"])
    } else {
        (
            "item",
            6,
            "count",
            vec!["stock", "external-source", "external-destination"],
        )
    };
    let template = |id: &str, positive: &str, negative: &str| LedgerTemplateIr {
        template_id: id.into(),
        parameters: vec![LedgerParameterIr {
            parameter_id: "amount".into(),
            scale: precision,
            min_unscaled: if gl { "0" } else { "1" }.into(),
            max_unscaled: "1000000000000".into(),
        }],
        legs: vec![
            leg(positive, 1, unit, precision),
            leg(negative, -1, unit, precision),
        ],
    };
    LedgerPackIr {
        ir_version: LEDGER_PACK_IR_VERSION.into(),
        pack_id: if gl {
            "demo.stock.gl"
        } else {
            "demo.stock.quantity"
        }
        .into(),
        // Synthetic pin: structural certificate evidence, not an admitted mapping.
        ontology_artifact: LedgerOntologyArtifactPin {
            artifact_id: "demo.stock.units".into(),
            artifact_version: "0.1.0".into(),
            artifact_sha256: "11".repeat(32),
        },
        units: vec![LedgerUnitIr {
            unit_id: unit.into(),
            unit_term_id: format!("demo.unit.{unit}"),
            quantity_kind_term_id: format!("demo.quantity.{quantity_kind}"),
            precision,
        }],
        accounts: accounts
            .into_iter()
            .map(|account| LedgerAccountIr {
                account_id: account.into(),
                allowed_unit_ids: vec![unit.into()],
                funding: LedgerFundingPolicy::Gross,
            })
            .collect(),
        layers: vec![LedgerLayerIr {
            layer_id: "posted".into(),
        }],
        templates: if gl {
            vec![
                template("receipt", "stock", "srbnb"),
                template("delivery", "cogs", "stock"),
            ]
        } else {
            vec![
                template("receipt", "stock", "external-source"),
                template("delivery", "external-destination", "stock"),
            ]
        },
    }
}

#[test]
fn receipt_delivery_templates_certify_every_parameter_coefficient() {
    for gl in [false, true] {
        let pack = templates(gl);
        let validation = validate_ledger_pack(&pack);
        assert!(validation.ok, "{:?}", validation.diagnostics);
        assert_eq!(validation.certificates.len(), 2);
        for certificate in validation.certificates {
            assert_eq!(certificate.constant_residual_unscaled, "0");
            assert_eq!(certificate.parameter_residuals_unscaled["amount"], "0");
        }
        let Ok(compiled) = compile_ledger_pack(&pack) else {
            panic!("valid templates compile")
        };
        for (template, amount, scale, expected) in [
            (
                "receipt",
                if gl { "8000" } else { "5000000" },
                if gl { 2 } else { 6 },
                if gl { "srbnb" } else { "external-source" },
            ),
            (
                "delivery",
                if gl { "6000" } else { "4000000" },
                if gl { 2 } else { 6 },
                "stock",
            ),
        ] {
            let result = evaluate_ledger_template(
                &compiled,
                template,
                &[LedgerParameterValue {
                    parameter_id: "amount".into(),
                    scale,
                    unscaled: amount.into(),
                }],
            );
            let Ok(evaluation) = result else {
                panic!("CP1 parameters evaluate")
            };
            assert_eq!(evaluation.legs.len(), 2);
            assert_eq!(evaluation.legs[0].amount_unscaled, amount);
            assert_eq!(evaluation.legs[1].amount_unscaled, format!("-{amount}"));
            assert_eq!(evaluation.legs[1].account_id, expected);
        }
    }
}

#[test]
fn specialization_cannot_drop_legs_or_change_coefficients() {
    for gl in [false, true] {
        for drop in [false, true] {
            let mut pack = templates(gl);
            if drop {
                pack.templates[0].legs.remove(1);
            } else {
                pack.templates[0].legs[1].amount.terms[0].coefficient_unscaled = "-2".into();
            }
            let result = validate_ledger_pack(&pack);
            assert!(!result.ok, "POST-VALUE: nonzero coefficient residual");
            assert!(result.certificates.is_empty());
        }
    }
}

#[test]
fn account_substitution_invalidates_the_pinned_certificate() {
    for gl in [false, true] {
        let mut pack = templates(gl);
        let certificate = validate_ledger_pack(&pack).certificates.remove(0);
        pack.templates[0].legs[1].account_id =
            if gl { "cogs" } else { "external-destination" }.into();
        assert!(
            validate_ledger_pack(&pack).ok,
            "substitution still conserves"
        );
        assert!(
            verify_conservation_certificate(&pack, &certificate).is_err(),
            "POST-AUTHORITY: the original seal must not admit a substituted account"
        );
    }
}

#[test]
fn exact_parameter_scale_and_bounds_are_admission_obligations() {
    for gl in [false, true] {
        let Ok(compiled) = compile_ledger_pack(&templates(gl)) else {
            panic!("valid templates compile")
        };
        for (scale, unscaled) in [
            (0, "1"),
            (if gl { 2 } else { 6 }, "-1"),
            (if gl { 2 } else { 6 }, "1000000000001"),
        ] {
            assert!(
                evaluate_ledger_template(
                    &compiled,
                    "receipt",
                    &[LedgerParameterValue {
                        parameter_id: "amount".into(),
                        scale,
                        unscaled: unscaled.into(),
                    }]
                )
                .is_err()
            );
        }
    }
}
