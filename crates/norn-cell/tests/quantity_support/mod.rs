use norn_ir::quantity_ledger::{
    LEDGER_PACK_IR_VERSION, LedgerAccountIr, LedgerLayerIr, LedgerLegIr, LedgerOntologyArtifactPin,
    LedgerPackIr, LedgerParameterIr, LedgerParameterValue, LedgerTemplateIr, LedgerUnitIr,
    LinearAmountIr, LinearTermIr, QUANTITY_CELL_VERSION, QuantityOperation, QuantityRequest,
};

pub(crate) fn pack() -> LedgerPackIr {
    let accounts = ["issuance", "lot", "transit", "received", "loss"];
    let templates = [
        ("fund", "issuance", "lot"),
        ("ship", "lot", "transit"),
        ("receive", "transit", "received"),
        ("loss", "transit", "loss"),
    ]
    .into_iter()
    .map(|(id, from, to)| LedgerTemplateIr {
        template_id: id.into(),
        parameters: vec![LedgerParameterIr {
            parameter_id: "q".into(),
            scale: 0,
            min_unscaled: "0".into(),
            max_unscaled: i128::MAX.to_string(),
        }],
        legs: [("out", from, "-1"), ("in", to, "1")]
            .into_iter()
            .map(|(leg, account, coefficient)| LedgerLegIr {
                leg_id: leg.into(),
                account_id: account.into(),
                unit_id: "item".into(),
                layer: "physical".into(),
                amount: LinearAmountIr {
                    result_scale: 0,
                    constant_unscaled: "0".into(),
                    constant_scale: 0,
                    terms: vec![LinearTermIr {
                        parameter_id: "q".into(),
                        coefficient_unscaled: coefficient.into(),
                        coefficient_scale: 0,
                    }],
                },
            })
            .collect(),
    })
    .collect();
    LedgerPackIr {
        ir_version: LEDGER_PACK_IR_VERSION.into(),
        pack_id: "fixture.quantities".into(),
        ontology_artifact: LedgerOntologyArtifactPin {
            artifact_id: "units".into(),
            artifact_version: "1".into(),
            artifact_sha256: "a".repeat(64),
        },
        units: vec![LedgerUnitIr {
            unit_id: "item".into(),
            unit_term_id: "unit.item".into(),
            quantity_kind_term_id: "kind.count".into(),
            precision: 0,
        }],
        accounts: accounts
            .into_iter()
            .map(|a| LedgerAccountIr {
                funding: norn_ir::quantity_ledger::LedgerFundingPolicy::Gross,
                account_id: a.into(),
                allowed_unit_ids: vec!["item".into()],
            })
            .collect(),
        layers: vec![LedgerLayerIr {
            layer_id: "physical".into(),
        }],
        templates,
    }
}

pub(crate) fn request(_version: u64, operation: QuantityOperation) -> QuantityRequest {
    QuantityRequest {
        version: QUANTITY_CELL_VERSION.into(),
        ledger_id: "stock".into(),
        balance_versions: Vec::new(),
        operation,
    }
}

pub(crate) fn parameters(amount: i128) -> Vec<LedgerParameterValue> {
    vec![LedgerParameterValue {
        parameter_id: "q".into(),
        scale: 0,
        unscaled: amount.to_string(),
    }]
}

pub(crate) fn define() -> QuantityRequest {
    request(
        0,
        QuantityOperation::Define {
            account_limit: 128,
            pack: pack(),
            unrestricted_accounts: vec!["issuance".into()],
        },
    )
}

pub(crate) fn post(version: u64, id: &str, template: &str, amount: i128) -> QuantityRequest {
    request(
        version,
        QuantityOperation::Post {
            posting_id: id.into(),
            template_id: template.into(),
            parameters: parameters(amount),
        },
    )
}

pub(crate) fn reserve(version: u64, id: &str, amount: i128, expires_at: u64) -> QuantityRequest {
    request(
        version,
        QuantityOperation::Reserve {
            reservation_id: id.into(),
            template_id: "ship".into(),
            parameters: parameters(amount),
            expires_at,
        },
    )
}
