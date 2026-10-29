//! Machine-checkable boundary between conservation checking and authority commit.
//!
//! The types in this module bind an exact pack, checker, certificate set, runtime
//! evaluation, transition identity, and required durable records. They do not
//! prove store atomicity: the authority host must persist all required records
//! in one admitted SQLite cell transaction and prove crash/reopen atomicity.

use std::collections::BTreeSet;

#[allow(
    clippy::wildcard_imports,
    reason = "moved verbatim from norn-ir, where every quantity-ledger IR type was in scope"
)]
use norn_ir::quantity_ledger::*;
use sha2::{Digest, Sha256};

use super::{
    CompiledLedgerPack, check_ledger_pack, diag, diagnostic_codes, evaluate_ledger_template,
    validate_ledger_pack,
};
use norn_ir::Diagnostic;

/// A checked pack plus the exact certificates the runtime may rely on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerPackSeal {
    pack_sha256: String,
    ontology_artifact: LedgerOntologyArtifactPin,
    unit_bindings_sha256: String,
    certificates: Vec<ConservationCertificate>,
    compiled: CompiledLedgerPack,
}

impl LedgerPackSeal {
    /// Canonical SHA-256 identity of the exact ordered pack IR.
    pub fn pack_sha256(&self) -> &str {
        self.pack_sha256.as_str()
    }

    /// Checker-generated certificates for all admitted template partitions.
    pub fn certificates(&self) -> &[ConservationCertificate] {
        self.certificates.as_slice()
    }

    /// Pack identifier bound into this seal.
    pub fn pack_id(&self) -> &str {
        self.compiled.pack_id.as_str()
    }

    /// Exact ontology artifact identity bound by this seal.
    pub fn ontology_artifact(&self) -> &LedgerOntologyArtifactPin {
        &self.ontology_artifact
    }

    /// Digest of the exact ordered unit-term and quantity-kind bindings.
    pub fn unit_bindings_sha256(&self) -> &str {
        self.unit_bindings_sha256.as_str()
    }
}

/// Computes the domain-separated SHA-256 identity of the exact ordered pack.
///
/// The preimage is a versioned sequence of fixed-width SHA-256 parts. Vector
/// order is significant; pack-local unit definitions and layer declarations
/// are therefore bound without depending on JSON object ordering.
#[must_use]
pub fn ledger_pack_sha256(pack: &LedgerPackIr) -> String {
    let mut hash = StableHash::new(b"norn.quantity.ledger-pack.v2");
    hash.text(b"ir-version", pack.ir_version.as_str());
    hash.text(b"pack-id", pack.pack_id.as_str());
    hash.text(
        b"ontology-artifact-id",
        pack.ontology_artifact.artifact_id.as_str(),
    );
    hash.text(
        b"ontology-artifact-version",
        pack.ontology_artifact.artifact_version.as_str(),
    );
    hash.text(
        b"ontology-artifact-sha256",
        pack.ontology_artifact.artifact_sha256.as_str(),
    );
    hash.marker(b"units-start");
    for unit in &pack.units {
        hash.marker(b"unit");
        hash.text(b"unit-id", unit.unit_id.as_str());
        hash.text(b"unit-term-id", unit.unit_term_id.as_str());
        hash.text(
            b"quantity-kind-term-id",
            unit.quantity_kind_term_id.as_str(),
        );
        hash.byte(b"unit-precision", unit.precision);
    }
    hash.marker(b"units-end");
    hash.marker(b"accounts-start");
    for account in &pack.accounts {
        hash.marker(b"account");
        hash.text(b"account-id", account.account_id.as_str());
        hash.marker(b"account-units-start");
        for unit_id in &account.allowed_unit_ids {
            hash.text(b"account-unit-id", unit_id.as_str());
        }
        hash.marker(b"account-units-end");
    }
    hash.marker(b"accounts-end");
    hash.marker(b"layers-start");
    for layer in &pack.layers {
        hash.marker(b"layer");
        hash.text(b"layer-id", layer.layer_id.as_str());
    }
    hash.marker(b"layers-end");
    hash.marker(b"templates-start");
    for template in &pack.templates {
        hash.marker(b"template");
        hash.text(b"template-id", template.template_id.as_str());
        hash.marker(b"parameters-start");
        for parameter in &template.parameters {
            hash.marker(b"parameter");
            hash.text(b"parameter-id", parameter.parameter_id.as_str());
            hash.byte(b"parameter-scale", parameter.scale);
            hash.text(b"parameter-min", parameter.min_unscaled.as_str());
            hash.text(b"parameter-max", parameter.max_unscaled.as_str());
        }
        hash.marker(b"parameters-end");
        hash.marker(b"legs-start");
        for leg in &template.legs {
            hash.marker(b"leg");
            hash.text(b"leg-id", leg.leg_id.as_str());
            hash.text(b"account-id", leg.account_id.as_str());
            hash.text(b"unit-id", leg.unit_id.as_str());
            hash.text(b"layer-id", leg.layer.as_str());
            hash.byte(b"result-scale", leg.amount.result_scale);
            hash.text(b"constant", leg.amount.constant_unscaled.as_str());
            hash.byte(b"constant-scale", leg.amount.constant_scale);
            hash.marker(b"terms-start");
            for term in &leg.amount.terms {
                hash.marker(b"term");
                hash.text(b"parameter-id", term.parameter_id.as_str());
                hash.text(b"coefficient", term.coefficient_unscaled.as_str());
                hash.byte(b"coefficient-scale", term.coefficient_scale);
            }
            hash.marker(b"terms-end");
        }
        hash.marker(b"legs-end");
    }
    hash.marker(b"templates-end");
    hash.finish()
}

/// Computes the domain-separated identity of the exact ontology pin and the
/// ordered pack-local unit-to-term bindings. It contains no conversion data.
#[must_use]
pub fn ledger_unit_bindings_sha256(pack: &LedgerPackIr) -> String {
    let mut hash = StableHash::new(b"norn.quantity.unit-bindings.v1");
    hash.text(
        b"ontology-artifact-id",
        pack.ontology_artifact.artifact_id.as_str(),
    );
    hash.text(
        b"ontology-artifact-version",
        pack.ontology_artifact.artifact_version.as_str(),
    );
    hash.text(
        b"ontology-artifact-sha256",
        pack.ontology_artifact.artifact_sha256.as_str(),
    );
    for unit in &pack.units {
        hash.marker(b"unit-binding");
        hash.text(b"unit-id", unit.unit_id.as_str());
        hash.text(b"unit-term-id", unit.unit_term_id.as_str());
        hash.text(
            b"quantity-kind-term-id",
            unit.quantity_kind_term_id.as_str(),
        );
    }
    hash.finish()
}

/// Computes the certificate identity from every claim except the identity itself.
#[must_use]
pub fn conservation_certificate_sha256(certificate: &ConservationCertificate) -> String {
    let mut hash = StableHash::new(b"norn.quantity.conservation-certificate.v2");
    hash.text(
        b"certificate-version",
        certificate.certificate_version.as_str(),
    );
    hash.text(b"ir-version", certificate.ir_version.as_str());
    hash.text(b"checker-version", certificate.checker_version.as_str());
    hash.text(b"pack-id", certificate.pack_id.as_str());
    hash.text(b"pack-sha256", certificate.pack_sha256.as_str());
    hash.text(
        b"ontology-artifact-id",
        certificate.ontology_artifact.artifact_id.as_str(),
    );
    hash.text(
        b"ontology-artifact-version",
        certificate.ontology_artifact.artifact_version.as_str(),
    );
    hash.text(
        b"ontology-artifact-sha256",
        certificate.ontology_artifact.artifact_sha256.as_str(),
    );
    hash.text(b"template-id", certificate.template_id.as_str());
    hash.text(b"unit-id", certificate.unit_id.as_str());
    hash.text(b"unit-term-id", certificate.unit_term_id.as_str());
    hash.text(
        b"quantity-kind-term-id",
        certificate.quantity_kind_term_id.as_str(),
    );
    hash.text(b"layer-id", certificate.layer.as_str());
    hash.byte(b"unit-precision", certificate.unit_precision);
    hash.text(
        b"constant-residual",
        certificate.constant_residual_unscaled.as_str(),
    );
    hash.marker(b"parameter-residuals-start");
    for (parameter_id, residual) in &certificate.parameter_residuals_unscaled {
        hash.marker(b"parameter-residual");
        hash.text(b"parameter-id", parameter_id.as_str());
        hash.text(b"residual", residual.as_str());
    }
    hash.marker(b"parameter-residuals-end");
    hash.finish()
}

/// Validates and compiles a pack into the only seal accepted by the posting boundary.
pub fn seal_ledger_pack(pack: &LedgerPackIr) -> Result<LedgerPackSeal, LedgerPackValidation> {
    let (validation, compiled) = check_ledger_pack(pack);
    match compiled {
        Some(compiled) => Ok(LedgerPackSeal {
            pack_sha256: ledger_pack_sha256(pack),
            ontology_artifact: pack.ontology_artifact.clone(),
            unit_bindings_sha256: ledger_unit_bindings_sha256(pack),
            certificates: validation.certificates,
            compiled,
        }),
        None => Err(validation),
    }
}

/// Replays the executable checker and verifies that one certificate belongs to
/// this exact pack, checker, IR, template, unit, layer, and precision.
pub fn verify_conservation_certificate(
    pack: &LedgerPackIr,
    certificate: &ConservationCertificate,
) -> Result<(), Vec<Diagnostic>> {
    let artifact_id = Some(pack.pack_id.clone());
    let mut diagnostics = Vec::new();
    let expected_pack_sha256 = ledger_pack_sha256(pack);

    if certificate.certificate_version != CONSERVATION_CERTIFICATE_VERSION
        || certificate.ir_version != pack.ir_version
        || certificate.checker_version != LEDGER_PACK_CHECKER_VERSION
    {
        diagnostics.push(diag(
            diagnostic_codes::CERTIFICATE_VERSION_MISMATCH,
            "certificate, IR, or checker version does not match the active checker",
            "certificate",
            artifact_id.clone(),
        ));
    }
    if certificate.pack_id != pack.pack_id || certificate.pack_sha256 != expected_pack_sha256 {
        diagnostics.push(diag(
            diagnostic_codes::CERTIFICATE_PACK_MISMATCH,
            "certificate is not bound to the exact supplied ledger pack",
            "certificate.packSha256",
            artifact_id.clone(),
        ));
    }
    if certificate.certificate_sha256 != conservation_certificate_sha256(certificate) {
        diagnostics.push(diag(
            diagnostic_codes::CERTIFICATE_HASH_MISMATCH,
            "certificate identity does not match its claims",
            "certificate.certificateSha256",
            artifact_id.clone(),
        ));
    }

    let validation = validate_ledger_pack(pack);
    if !validation.ok {
        diagnostics.extend(validation.diagnostics);
    } else if !validation
        .certificates
        .iter()
        .any(|expected| expected == certificate)
    {
        diagnostics.push(diag(
            diagnostic_codes::CERTIFICATE_CLAIM_MISMATCH,
            "certificate claims are not in the deterministic checker result",
            "certificate",
            artifact_id,
        ));
    }

    if diagnostics.is_empty() {
        Ok(())
    } else {
        Err(diagnostics)
    }
}

/// Checks proof references and exact runtime inputs, then emits the only
/// commitment a future CALA posting adapter should accept.
pub fn prepare_quantity_posting(
    seal: &LedgerPackSeal,
    intent: &QuantityPostingIntent,
) -> Result<QuantityPostingCommitment, Vec<Diagnostic>> {
    let artifact_id = Some(seal.pack_id().to_string());
    let mut diagnostics = Vec::new();
    if intent.contract_version != QUANTITY_POSTING_CONTRACT_VERSION {
        diagnostics.push(diag(
            diagnostic_codes::POSTING_CONTRACT_VERSION_MISMATCH,
            format!(
                "unsupported posting contract version `{}`; expected `{QUANTITY_POSTING_CONTRACT_VERSION}`",
                intent.contract_version
            ),
            "contractVersion",
            artifact_id.clone(),
        ));
    }
    for (path, value) in [
        ("scopeKey", intent.scope_key.as_str()),
        ("transitionId", intent.transition_id.as_str()),
        ("idempotencyKey", intent.idempotency_key.as_str()),
        ("templateId", intent.template_id.as_str()),
    ] {
        if value.is_empty() || value.trim() != value {
            diagnostics.push(diag(
                diagnostic_codes::POSTING_IDENTITY_EMPTY,
                format!("{path} must be non-empty and contain no surrounding whitespace"),
                path,
                artifact_id.clone(),
            ));
        }
    }
    if intent.pack_id != seal.pack_id()
        || intent.pack_sha256 != seal.pack_sha256()
        || intent.checker_version != LEDGER_PACK_CHECKER_VERSION
    {
        diagnostics.push(diag(
            diagnostic_codes::POSTING_REFERENCE_MISMATCH,
            "posting references do not match the installed pack seal and checker",
            "packSha256",
            artifact_id.clone(),
        ));
    }
    if !diagnostics.is_empty() {
        return Err(diagnostics);
    }

    let evaluation = evaluate_ledger_template(
        &seal.compiled,
        intent.template_id.as_str(),
        intent.parameters.as_slice(),
    )?;
    let mut certificates = seal
        .certificates
        .iter()
        .filter(|certificate| certificate.template_id == intent.template_id)
        .collect::<Vec<_>>();
    certificates.sort_by(|left, right| {
        (left.unit_id.as_str(), left.layer.as_str())
            .cmp(&(right.unit_id.as_str(), right.layer.as_str()))
    });
    let certificate_partitions = certificates
        .iter()
        .map(|certificate| {
            (
                certificate.unit_id.as_str(),
                certificate.layer.as_str(),
                certificate.unit_precision,
            )
        })
        .collect::<BTreeSet<_>>();
    let evaluation_partitions = evaluation
        .legs
        .iter()
        .map(|leg| (leg.unit_id.as_str(), leg.layer.as_str(), leg.unit_precision))
        .collect::<BTreeSet<_>>();
    if certificate_partitions != evaluation_partitions
        || certificates.iter().any(|certificate| {
            certificate.certificate_sha256 != conservation_certificate_sha256(certificate)
        })
    {
        return Err(vec![diag(
            diagnostic_codes::CERTIFICATE_COVERAGE_MISMATCH,
            "certificate set does not exactly cover the evaluated unit/layer partitions",
            "certificateSha256s",
            artifact_id,
        )]);
    }

    let certificate_sha256s = certificates
        .into_iter()
        .map(|certificate| certificate.certificate_sha256.clone())
        .collect::<Vec<_>>();
    let parameter_sha256 = parameter_sha256(intent.parameters.as_slice());
    let evaluation_sha256 = evaluation_sha256(&evaluation);
    let posting_input_sha256 = posting_input_sha256(
        intent,
        seal.ontology_artifact(),
        seal.unit_bindings_sha256(),
        certificate_sha256s.as_slice(),
        parameter_sha256.as_str(),
        evaluation_sha256.as_str(),
    );
    let outbox_message_id = outbox_message_id(intent, posting_input_sha256.as_str());

    Ok(QuantityPostingCommitment {
        contract_version: QUANTITY_POSTING_CONTRACT_VERSION.to_string(),
        scope_key: intent.scope_key.clone(),
        transition_id: intent.transition_id.clone(),
        idempotency_key: intent.idempotency_key.clone(),
        expected_owner_version: intent.expected_owner_version,
        pack_id: intent.pack_id.clone(),
        pack_sha256: intent.pack_sha256.clone(),
        checker_version: intent.checker_version.clone(),
        template_id: intent.template_id.clone(),
        certificate_sha256s,
        parameter_sha256,
        evaluation_sha256,
        posting_input_sha256,
        outbox_message_id,
        legs: evaluation.legs,
    })
}

/// Classifies a retry without performing any mutation.
pub fn classify_quantity_replay(
    commitment: &QuantityPostingCommitment,
    existing: Option<&QuantityIdempotencyRecord>,
) -> Result<QuantityReplayDecision, Vec<Diagnostic>> {
    let Some(existing) = existing else {
        return Ok(QuantityReplayDecision::New);
    };
    if existing.scope_key != commitment.scope_key
        || existing.idempotency_key != commitment.idempotency_key
    {
        return Err(vec![diag(
            diagnostic_codes::IDEMPOTENCY_RECORD_MISMATCH,
            "idempotency lookup returned a record from another scope or key",
            "idempotencyResult",
            Some(commitment.pack_id.clone()),
        )]);
    }
    if existing.posting_input_sha256 != commitment.posting_input_sha256 {
        return Ok(QuantityReplayDecision::Conflict);
    }
    if !is_sha256(existing.stable_result_sha256.as_str()) {
        return Err(vec![diag(
            diagnostic_codes::IDEMPOTENCY_RECORD_MISMATCH,
            "matching idempotency record has no canonical stable-result SHA-256 identity",
            "idempotencyResult.stableResultSha256",
            Some(commitment.pack_id.clone()),
        )]);
    }
    Ok(QuantityReplayDecision::Replay {
        stable_result_sha256: existing.stable_result_sha256.clone(),
    })
}

/// Accepts only a clean pre-state or a complete matching post-state.
///
/// This checker detects partial or cross-wired evidence after reopen. It does
/// not establish that the store is atomic; crash injection must demonstrate
/// that partial observations are unreachable in the authority host's SQLite adapter.
pub fn verify_quantity_commit_observation(
    commitment: &QuantityPostingCommitment,
    observation: &QuantityAuthorityCommitObservation,
) -> Result<QuantityCommitStatus, Vec<Diagnostic>> {
    if observation_is_empty(observation) {
        return Ok(QuantityCommitStatus::NotCommitted);
    }

    let artifact_id = Some(commitment.pack_id.clone());
    let mut diagnostics = Vec::new();
    for (path, observed) in [
        (
            "quantityTransactionInputSha256",
            observation.quantity_transaction_input_sha256.as_deref(),
        ),
        (
            "entriesInputSha256",
            observation.entries_input_sha256.as_deref(),
        ),
        (
            "balanceHeadsInputSha256",
            observation.balance_heads_input_sha256.as_deref(),
        ),
        (
            "balanceHistoryInputSha256",
            observation.balance_history_input_sha256.as_deref(),
        ),
        (
            "entityEventsInputSha256",
            observation.entity_events_input_sha256.as_deref(),
        ),
        (
            "businessEffectInputSha256",
            observation.business_effect_input_sha256.as_deref(),
        ),
        (
            "canonicalEventInputSha256",
            observation.canonical_event_input_sha256.as_deref(),
        ),
        (
            "checkpointInputSha256",
            observation.checkpoint_input_sha256.as_deref(),
        ),
    ] {
        require_matching_hash(
            path,
            observed,
            commitment.posting_input_sha256.as_str(),
            artifact_id.clone(),
            &mut diagnostics,
        );
    }

    match commitment.expected_owner_version.checked_add(1) {
        Some(expected) if observation.owner_version == Some(expected) => {}
        Some(expected) => diagnostics.push(diag(
            if observation.owner_version.is_some() {
                diagnostic_codes::COMMIT_EVIDENCE_MISMATCH
            } else {
                diagnostic_codes::PARTIAL_COMMIT_EVIDENCE
            },
            format!("owner version must be present and equal {expected}"),
            "ownerVersion",
            artifact_id.clone(),
        )),
        None => diagnostics.push(diag(
            diagnostic_codes::OWNER_VERSION_OVERFLOW,
            "expected owner version cannot advance without overflow",
            "expectedOwnerVersion",
            artifact_id.clone(),
        )),
    }

    let mut stable_result_sha256 = None;
    match &observation.idempotency_result {
        Some(record)
            if record.scope_key == commitment.scope_key
                && record.idempotency_key == commitment.idempotency_key
                && record.posting_input_sha256 == commitment.posting_input_sha256
                && is_sha256(record.stable_result_sha256.as_str()) =>
        {
            stable_result_sha256 = Some(record.stable_result_sha256.clone());
        }
        Some(_) => diagnostics.push(diag(
            diagnostic_codes::IDEMPOTENCY_RECORD_MISMATCH,
            "idempotency result does not match scope, key, input, and stable result obligations",
            "idempotencyResult",
            artifact_id.clone(),
        )),
        None => diagnostics.push(diag(
            diagnostic_codes::PARTIAL_COMMIT_EVIDENCE,
            "committed observation is missing its idempotency result",
            "idempotencyResult",
            artifact_id.clone(),
        )),
    }

    match &observation.outbox {
        Some(record)
            if record.message_id == commitment.outbox_message_id
                && record.transition_id == commitment.transition_id
                && record.posting_input_sha256 == commitment.posting_input_sha256 => {}
        Some(_) => diagnostics.push(diag(
            diagnostic_codes::COMMIT_EVIDENCE_MISMATCH,
            "outbox identity, transition, or input hash does not match the posting commitment",
            "outbox",
            artifact_id,
        )),
        None => diagnostics.push(diag(
            diagnostic_codes::PARTIAL_COMMIT_EVIDENCE,
            "committed observation is missing its outbox record",
            "outbox",
            artifact_id,
        )),
    }

    if !diagnostics.is_empty() {
        return Err(diagnostics);
    }
    match stable_result_sha256 {
        Some(stable_result_sha256) => Ok(QuantityCommitStatus::Committed {
            stable_result_sha256,
        }),
        None => Err(vec![diag(
            diagnostic_codes::PARTIAL_COMMIT_EVIDENCE,
            "committed observation has no stable result",
            "idempotencyResult",
            Some(commitment.pack_id.clone()),
        )]),
    }
}

fn parameter_sha256(parameters: &[LedgerParameterValue]) -> String {
    let mut sorted = parameters.iter().collect::<Vec<_>>();
    sorted.sort_by(|left, right| left.parameter_id.cmp(&right.parameter_id));
    let mut hash = StableHash::new(b"norn.quantity.parameters.v1");
    for parameter in sorted {
        hash.marker(b"parameter");
        hash.text(b"parameter-id", parameter.parameter_id.as_str());
        hash.byte(b"scale", parameter.scale);
        hash.text(b"unscaled", parameter.unscaled.as_str());
    }
    hash.finish()
}

fn evaluation_sha256(evaluation: &LedgerTemplateEvaluation) -> String {
    let mut hash = StableHash::new(b"norn.quantity.evaluation.v1");
    hash.text(b"pack-id", evaluation.pack_id.as_str());
    hash.text(b"template-id", evaluation.template_id.as_str());
    for leg in &evaluation.legs {
        hash.marker(b"leg");
        hash_evaluated_leg(&mut hash, leg);
    }
    hash.finish()
}

fn hash_evaluated_leg(hash: &mut StableHash, leg: &EvaluatedLedgerLeg) {
    hash.text(b"leg-id", leg.leg_id.as_str());
    hash.text(b"account-id", leg.account_id.as_str());
    hash.text(b"unit-id", leg.unit_id.as_str());
    hash.text(b"layer-id", leg.layer.as_str());
    hash.byte(b"unit-precision", leg.unit_precision);
    hash.text(b"amount-unscaled", leg.amount_unscaled.as_str());
}

fn posting_input_sha256(
    intent: &QuantityPostingIntent,
    ontology_artifact: &LedgerOntologyArtifactPin,
    unit_bindings_sha256: &str,
    certificate_sha256s: &[String],
    parameter_sha256: &str,
    evaluation_sha256: &str,
) -> String {
    let mut hash = StableHash::new(b"norn.quantity.posting-input.v2");
    hash.text(b"contract-version", intent.contract_version.as_str());
    hash.text(b"scope-key", intent.scope_key.as_str());
    hash.text(b"transition-id", intent.transition_id.as_str());
    hash.text(b"idempotency-key", intent.idempotency_key.as_str());
    hash.u64(b"expected-owner-version", intent.expected_owner_version);
    hash.text(b"pack-id", intent.pack_id.as_str());
    hash.text(b"pack-sha256", intent.pack_sha256.as_str());
    hash.text(
        b"ontology-artifact-id",
        ontology_artifact.artifact_id.as_str(),
    );
    hash.text(
        b"ontology-artifact-version",
        ontology_artifact.artifact_version.as_str(),
    );
    hash.text(
        b"ontology-artifact-sha256",
        ontology_artifact.artifact_sha256.as_str(),
    );
    hash.text(b"unit-bindings-sha256", unit_bindings_sha256);
    hash.text(b"checker-version", intent.checker_version.as_str());
    hash.text(b"template-id", intent.template_id.as_str());
    for certificate_sha256 in certificate_sha256s {
        hash.text(b"certificate-sha256", certificate_sha256.as_str());
    }
    hash.text(b"parameter-sha256", parameter_sha256);
    hash.text(b"evaluation-sha256", evaluation_sha256);
    hash.finish()
}

fn outbox_message_id(intent: &QuantityPostingIntent, posting_input_sha256: &str) -> String {
    let mut hash = StableHash::new(b"norn.quantity.outbox-message.v1");
    hash.text(b"scope-key", intent.scope_key.as_str());
    hash.text(b"transition-id", intent.transition_id.as_str());
    hash.text(b"posting-input-sha256", posting_input_sha256);
    hash.finish()
}

fn require_matching_hash(
    path: &str,
    observed: Option<&str>,
    expected: &str,
    artifact_id: Option<String>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match observed {
        Some(value) if value == expected => {}
        Some(_) => diagnostics.push(diag(
            diagnostic_codes::COMMIT_EVIDENCE_MISMATCH,
            format!("{path} does not match the posting input identity"),
            path,
            artifact_id,
        )),
        None => diagnostics.push(diag(
            diagnostic_codes::PARTIAL_COMMIT_EVIDENCE,
            format!("committed observation is missing {path}"),
            path,
            artifact_id,
        )),
    }
}

fn observation_is_empty(observation: &QuantityAuthorityCommitObservation) -> bool {
    observation.quantity_transaction_input_sha256.is_none()
        && observation.entries_input_sha256.is_none()
        && observation.balance_heads_input_sha256.is_none()
        && observation.balance_history_input_sha256.is_none()
        && observation.entity_events_input_sha256.is_none()
        && observation.owner_version.is_none()
        && observation.business_effect_input_sha256.is_none()
        && observation.canonical_event_input_sha256.is_none()
        && observation.checkpoint_input_sha256.is_none()
        && observation.idempotency_result.is_none()
        && observation.outbox.is_none()
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

struct StableHash(Sha256);

impl StableHash {
    fn new(domain: &[u8]) -> Self {
        let mut hash = Self(Sha256::new());
        hash.part(b"domain", domain);
        hash
    }

    fn marker(&mut self, marker: &[u8]) {
        self.part(b"marker", marker);
    }

    fn text(&mut self, label: &[u8], value: &str) {
        self.part(label, value.as_bytes());
    }

    fn byte(&mut self, label: &[u8], value: u8) {
        self.part(label, &[value]);
    }

    fn u64(&mut self, label: &[u8], value: u64) {
        self.part(label, &value.to_be_bytes());
    }

    fn part(&mut self, label: &[u8], value: &[u8]) {
        self.0.update(Sha256::digest(label));
        self.0.update(Sha256::digest(value));
    }

    fn finish(self) -> String {
        format!("{:x}", self.0.finalize())
    }
}
