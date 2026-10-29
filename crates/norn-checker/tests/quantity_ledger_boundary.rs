#![allow(
    clippy::expect_used,
    clippy::pedantic,
    clippy::unwrap_used,
    missing_docs,
    reason = "boundary contract tests intentionally fail hard on broken fixtures"
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

use norn_checker::quantity_ledger::{
    classify_quantity_replay, diagnostic_codes, ledger_pack_sha256, prepare_quantity_posting,
    seal_ledger_pack, verify_conservation_certificate, verify_quantity_commit_observation,
};
use norn_ir::quantity_ledger::{
    LEDGER_PACK_CHECKER_VERSION, LEDGER_PACK_IR_VERSION, LedgerAccountIr, LedgerLayerIr,
    LedgerLegIr, LedgerOntologyArtifactPin, LedgerPackIr, LedgerParameterIr, LedgerParameterValue,
    LedgerTemplateIr, LedgerUnitIr, LinearAmountIr, LinearTermIr,
    QUANTITY_POSTING_CONTRACT_VERSION, QuantityAuthorityCommitObservation, QuantityCommitStatus,
    QuantityIdempotencyRecord, QuantityOutboxRecord, QuantityPostingCommitment,
    QuantityPostingIntent, QuantityReplayDecision,
};

const STABLE_RESULT_SHA256: &str =
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn ontology_pin() -> LedgerOntologyArtifactPin {
    LedgerOntologyArtifactPin {
        artifact_id: "norn.ontology-pack.quantity-units".to_string(),
        artifact_version: "0.1.0".to_string(),
        artifact_sha256: "45be55780443fa2ece436ba8ab89107dd4ffcef5388beb3d332142c033ecf7d1"
            .to_string(),
    }
}

fn amount(parameter_id: &str, coefficient: &str) -> LinearAmountIr {
    LinearAmountIr {
        result_scale: 2,
        constant_unscaled: "0".to_string(),
        constant_scale: 0,
        terms: vec![LinearTermIr {
            parameter_id: parameter_id.to_string(),
            coefficient_unscaled: coefficient.to_string(),
            coefficient_scale: 0,
        }],
    }
}

fn pack() -> LedgerPackIr {
    LedgerPackIr {
        ir_version: LEDGER_PACK_IR_VERSION.to_string(),
        pack_id: "pack.boundary".to_string(),
        ontology_artifact: ontology_pin(),
        units: vec![LedgerUnitIr {
            unit_id: "kg".to_string(),
            unit_term_id: "norn.measure.unit.kilogram".to_string(),
            quantity_kind_term_id: "norn.measure.quantity-kind.mass".to_string(),
            precision: 2,
        }],
        accounts: ["warehouse-a", "warehouse-b"]
            .into_iter()
            .map(|account_id| LedgerAccountIr {
                funding: norn_ir::quantity_ledger::LedgerFundingPolicy::Gross,
                account_id: account_id.to_string(),
                allowed_unit_ids: vec!["kg".to_string()],
            })
            .collect(),
        layers: vec![LedgerLayerIr {
            layer_id: "posted".to_string(),
        }],
        templates: vec![LedgerTemplateIr {
            template_id: "transfer".to_string(),
            parameters: vec![LedgerParameterIr {
                parameter_id: "quantity".to_string(),
                scale: 2,
                min_unscaled: "0".to_string(),
                max_unscaled: "1000000".to_string(),
            }],
            legs: vec![
                LedgerLegIr {
                    leg_id: "source".to_string(),
                    account_id: "warehouse-a".to_string(),
                    unit_id: "kg".to_string(),
                    layer: "posted".to_string(),
                    amount: amount("quantity", "-1"),
                },
                LedgerLegIr {
                    leg_id: "destination".to_string(),
                    account_id: "warehouse-b".to_string(),
                    unit_id: "kg".to_string(),
                    layer: "posted".to_string(),
                    amount: amount("quantity", "1"),
                },
            ],
        }],
    }
}

fn intent(pack_sha256: &str) -> QuantityPostingIntent {
    QuantityPostingIntent {
        contract_version: QUANTITY_POSTING_CONTRACT_VERSION.to_string(),
        scope_key: "org:acme".to_string(),
        transition_id: "transition:42".to_string(),
        idempotency_key: "posting:42".to_string(),
        expected_owner_version: 8,
        pack_id: "pack.boundary".to_string(),
        pack_sha256: pack_sha256.to_string(),
        checker_version: LEDGER_PACK_CHECKER_VERSION.to_string(),
        template_id: "transfer".to_string(),
        parameters: vec![LedgerParameterValue {
            parameter_id: "quantity".to_string(),
            scale: 2,
            unscaled: "1250".to_string(),
        }],
    }
}

fn commitment() -> QuantityPostingCommitment {
    let pack = pack();
    let seal = seal_ledger_pack(&pack).expect("valid pack must seal");
    prepare_quantity_posting(&seal, &intent(seal.pack_sha256()))
        .expect("valid intent must produce a posting commitment")
}

fn complete_observation(
    commitment: &QuantityPostingCommitment,
) -> QuantityAuthorityCommitObservation {
    let input = Some(commitment.posting_input_sha256.clone());
    QuantityAuthorityCommitObservation {
        quantity_transaction_input_sha256: input.clone(),
        entries_input_sha256: input.clone(),
        balance_heads_input_sha256: input.clone(),
        balance_history_input_sha256: input.clone(),
        entity_events_input_sha256: input.clone(),
        owner_version: commitment.expected_owner_version.checked_add(1),
        business_effect_input_sha256: input.clone(),
        canonical_event_input_sha256: input.clone(),
        checkpoint_input_sha256: input,
        idempotency_result: Some(QuantityIdempotencyRecord {
            scope_key: commitment.scope_key.clone(),
            idempotency_key: commitment.idempotency_key.clone(),
            posting_input_sha256: commitment.posting_input_sha256.clone(),
            stable_result_sha256: STABLE_RESULT_SHA256.to_string(),
        }),
        outbox: Some(QuantityOutboxRecord {
            message_id: commitment.outbox_message_id.clone(),
            transition_id: commitment.transition_id.clone(),
            posting_input_sha256: commitment.posting_input_sha256.clone(),
        }),
    }
}

#[test]
fn certificate_identity_binds_exact_pack_checker_unit_and_layer() {
    let original = pack();
    let seal = seal_ledger_pack(&original).expect("valid pack must seal");
    let certificate = seal
        .certificates()
        .first()
        .expect("one partition must have one certificate");

    assert_eq!(certificate.pack_sha256, ledger_pack_sha256(&original));
    assert_eq!(certificate.pack_sha256.len(), 64);
    assert_eq!(certificate.certificate_sha256.len(), 64);
    verify_conservation_certificate(&original, certificate)
        .expect("checker-produced certificate must replay");

    let mut semantically_different = original.clone();
    semantically_different.units[0].unit_id = "lb".to_string();
    for leg in &mut semantically_different.templates[0].legs {
        leg.unit_id = "lb".to_string();
    }
    assert_ne!(
        ledger_pack_sha256(&original),
        ledger_pack_sha256(&semantically_different)
    );
    let errors = verify_conservation_certificate(&semantically_different, certificate)
        .expect_err("certificate reuse across pack identities must fail");
    assert!(
        errors
            .iter()
            .any(|error| error.code == diagnostic_codes::CERTIFICATE_PACK_MISMATCH)
    );
}

#[test]
fn recomputing_a_tampered_certificate_hash_does_not_forge_checker_agreement() {
    let pack = pack();
    let seal = seal_ledger_pack(&pack).expect("valid pack must seal");
    let original = seal.certificates()[0].clone();
    for attack in 0..2 {
        let mut forged = original.clone();
        if attack == 0 {
            forged.layer = "pending".to_string();
        } else {
            forged.quantity_kind_term_id = "norn.measure.quantity-kind.duration".to_string();
        }
        forged.certificate_sha256 =
            norn_checker::quantity_ledger::conservation_certificate_sha256(&forged);

        let errors = verify_conservation_certificate(&pack, &forged)
            .expect_err("self-consistent forged claims must not replace checker output");
        assert!(
            errors
                .iter()
                .any(|error| error.code == diagnostic_codes::CERTIFICATE_CLAIM_MISMATCH)
        );
    }
}

#[test]
fn posting_commitment_binds_runtime_evaluation_and_deterministic_outbox_identity() {
    let pack = pack();
    let seal = seal_ledger_pack(&pack).expect("valid pack must seal");
    let posting_intent = intent(seal.pack_sha256());
    let first = prepare_quantity_posting(&seal, &posting_intent).expect("intent must prepare");
    let second =
        prepare_quantity_posting(&seal, &posting_intent).expect("retry must prepare identically");

    assert_eq!(first, second);
    assert_eq!(first.certificate_sha256s.len(), 1);
    assert_eq!(first.legs.len(), 2);
    assert_eq!(first.legs[0].unit_id, "kg");
    assert_eq!(first.legs[0].layer, "posted");
    assert_eq!(first.legs[0].amount_unscaled, "-1250");
    assert_eq!(first.legs[1].amount_unscaled, "1250");
    assert_eq!(first.posting_input_sha256.len(), 64);
    assert_eq!(first.outbox_message_id.len(), 64);

    let mut changed = posting_intent;
    changed.parameters[0].unscaled = "1251".to_string();
    let changed = prepare_quantity_posting(&seal, &changed).expect("changed input remains legal");
    assert_ne!(first.parameter_sha256, changed.parameter_sha256);
    assert_ne!(first.evaluation_sha256, changed.evaluation_sha256);
    assert_ne!(first.posting_input_sha256, changed.posting_input_sha256);
    assert_ne!(first.outbox_message_id, changed.outbox_message_id);

    let mut changed_idempotency = intent(seal.pack_sha256());
    changed_idempotency.idempotency_key = "posting:43".to_string();
    let changed_idempotency = prepare_quantity_posting(&seal, &changed_idempotency)
        .expect("a distinct idempotency identity can prepare a distinct proposal");
    assert_ne!(
        first.posting_input_sha256,
        changed_idempotency.posting_input_sha256
    );
    assert_ne!(
        first.outbox_message_id,
        changed_idempotency.outbox_message_id
    );
}

#[test]
fn stale_pack_or_checker_reference_fails_before_runtime_posting() {
    let pack = pack();
    let seal = seal_ledger_pack(&pack).expect("valid pack must seal");
    for mut bad_intent in [intent("wrong-pack-hash"), intent(seal.pack_sha256())] {
        if bad_intent.pack_sha256 == seal.pack_sha256() {
            bad_intent.checker_version = "future-checker".to_string();
        }
        let errors = prepare_quantity_posting(&seal, &bad_intent)
            .expect_err("stale proof references must fail closed");
        assert!(
            errors
                .iter()
                .any(|error| error.code == diagnostic_codes::POSTING_REFERENCE_MISMATCH)
        );
    }
}

#[test]
fn idempotency_replay_returns_stable_result_and_changed_input_conflicts() {
    let commitment = commitment();
    assert_eq!(
        classify_quantity_replay(&commitment, None).unwrap(),
        QuantityReplayDecision::New
    );

    let record = QuantityIdempotencyRecord {
        scope_key: commitment.scope_key.clone(),
        idempotency_key: commitment.idempotency_key.clone(),
        posting_input_sha256: commitment.posting_input_sha256.clone(),
        stable_result_sha256: STABLE_RESULT_SHA256.to_string(),
    };
    assert_eq!(
        classify_quantity_replay(&commitment, Some(&record)).unwrap(),
        QuantityReplayDecision::Replay {
            stable_result_sha256: STABLE_RESULT_SHA256.to_string()
        }
    );

    let mut conflicting = record;
    conflicting.posting_input_sha256 = "different-input".to_string();
    assert_eq!(
        classify_quantity_replay(&commitment, Some(&conflicting)).unwrap(),
        QuantityReplayDecision::Conflict
    );
}

#[test]
fn authority_observation_accepts_only_all_absent_or_complete_matching_state() {
    let commitment = commitment();
    assert_eq!(
        verify_quantity_commit_observation(
            &commitment,
            &QuantityAuthorityCommitObservation::default()
        )
        .unwrap(),
        QuantityCommitStatus::NotCommitted
    );

    let complete = complete_observation(&commitment);
    assert_eq!(
        verify_quantity_commit_observation(&commitment, &complete).unwrap(),
        QuantityCommitStatus::Committed {
            stable_result_sha256: STABLE_RESULT_SHA256.to_string()
        }
    );

    const ALL_RECORDS: u16 = 0b111_1111_1111;
    for presence in 1_u16..ALL_RECORDS {
        let partial = QuantityAuthorityCommitObservation {
            quantity_transaction_input_sha256: (presence & 0b0_0000_0001 != 0)
                .then(|| commitment.posting_input_sha256.clone()),
            entries_input_sha256: (presence & 0b0_0000_0010 != 0)
                .then(|| commitment.posting_input_sha256.clone()),
            balance_heads_input_sha256: (presence & 0b0_0000_0100 != 0)
                .then(|| commitment.posting_input_sha256.clone()),
            balance_history_input_sha256: (presence & 0b0_0000_1000 != 0)
                .then(|| commitment.posting_input_sha256.clone()),
            entity_events_input_sha256: (presence & 0b0_0001_0000 != 0)
                .then(|| commitment.posting_input_sha256.clone()),
            owner_version: (presence & 0b0_0010_0000 != 0)
                .then(|| commitment.expected_owner_version.checked_add(1))
                .flatten(),
            business_effect_input_sha256: (presence & 0b0_0100_0000 != 0)
                .then(|| commitment.posting_input_sha256.clone()),
            canonical_event_input_sha256: (presence & 0b0_1000_0000 != 0)
                .then(|| commitment.posting_input_sha256.clone()),
            checkpoint_input_sha256: (presence & 0b1_0000_0000 != 0)
                .then(|| commitment.posting_input_sha256.clone()),
            idempotency_result: (presence & 0b10_0000_0000 != 0)
                .then(|| complete.idempotency_result.clone())
                .flatten(),
            outbox: (presence & 0b100_0000_0000 != 0)
                .then(|| complete.outbox.clone())
                .flatten(),
        };
        let errors = verify_quantity_commit_observation(&commitment, &partial)
            .expect_err("every nonempty partial durable state must fail closed");
        assert!(
            errors
                .iter()
                .any(|error| error.code == diagnostic_codes::PARTIAL_COMMIT_EVIDENCE)
        );
    }
}

#[test]
fn cross_wired_outbox_or_idempotency_evidence_is_rejected() {
    let commitment = commitment();
    let mut wrong_outbox = complete_observation(&commitment);
    wrong_outbox.outbox.as_mut().unwrap().message_id = "another-message".to_string();
    let errors = verify_quantity_commit_observation(&commitment, &wrong_outbox)
        .expect_err("cross-wired outbox must fail");
    assert!(
        errors
            .iter()
            .any(|error| error.code == diagnostic_codes::COMMIT_EVIDENCE_MISMATCH)
    );

    let mut wrong_idempotency = complete_observation(&commitment);
    wrong_idempotency
        .idempotency_result
        .as_mut()
        .unwrap()
        .scope_key = "org:other".to_string();
    let errors = verify_quantity_commit_observation(&commitment, &wrong_idempotency)
        .expect_err("cross-scope idempotency result must fail");
    assert!(
        errors
            .iter()
            .any(|error| error.code == diagnostic_codes::IDEMPOTENCY_RECORD_MISMATCH)
    );

    let mut malformed_result = complete_observation(&commitment);
    malformed_result
        .idempotency_result
        .as_mut()
        .unwrap()
        .stable_result_sha256 = "not-a-sha256".to_string();
    let errors = verify_quantity_commit_observation(&commitment, &malformed_result)
        .expect_err("a stable result must have one canonical SHA-256 identity");
    assert!(
        errors
            .iter()
            .any(|error| error.code == diagnostic_codes::IDEMPOTENCY_RECORD_MISMATCH)
    );
}

#[test]
fn boundary_hashes_are_versioned_golden_vectors() {
    let pack = pack();
    let seal = seal_ledger_pack(&pack).expect("valid pack must seal");
    let commitment = prepare_quantity_posting(&seal, &intent(seal.pack_sha256()))
        .expect("valid intent must prepare");
    assert_eq!(
        (
            seal.pack_sha256(),
            seal.unit_bindings_sha256(),
            seal.certificates()[0].certificate_sha256.as_str(),
            commitment.parameter_sha256.as_str(),
            commitment.evaluation_sha256.as_str(),
            commitment.posting_input_sha256.as_str(),
            commitment.outbox_message_id.as_str(),
        ),
        (
            "a74ca7f1eb0d7df566ce95d294079e0ff8bb54e805f9e22bd2c5811f00ebce75",
            "86f638d1d4378e8df2e7d45b8671a25dfa83302669f391f293523c0a92689c1e",
            "2cfdf3ba14ecd954871028bb3070ac063279d401b409a3da25a63955df90ab20",
            "61fae392a167a0d9e81fddab4e87ac576a43d91853c9114a39cc7589031478b2",
            "db1105f54612799979190bd12522dc62678aaaeb9eb41106752006541111f11f",
            "ea5ac2362eb48facce6341c9fea3df98f57fd22590a1abfe7c6e400c95ee8cd3",
            "546fac97af3bce80950dabab12e6f99a0845fb40879a4b22c1b2ad7d95d06288",
        )
    );
}
