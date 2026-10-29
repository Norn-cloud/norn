//! Pure seal-time validation for conserved-quantity ledger packs, moved
//! verbatim from `norn-ir` (T1-CHK-1, #2524). The wire types stay in
//! `norn_ir::quantity_ledger`.
//!
//! This module is intentionally independent of CALA, Turso, CEL, and the
//! runtime engine. The closed authoring-expression module lowers a deliberately
//! small affine language into [`LinearAmountIr`]. The seal checker then
//! validates that exact scaled-integer representation and checks that every
//! template has a zero constant and zero coefficient for every `(unit, layer)`
//! partition. It is executable code and testable evidence, but it is not a
//! Lean or other proof-assistant correspondence claim.

use std::collections::{BTreeMap, BTreeSet};

#[allow(
    clippy::wildcard_imports,
    reason = "moved verbatim from norn-ir, where every quantity-ledger IR type was in scope"
)]
use norn_ir::quantity_ledger::*;
use norn_ir::{Diagnostic, DiagnosticRemediation, DiagnosticSeverity};

mod boundary;
mod expression;
pub use boundary::{
    LedgerPackSeal, classify_quantity_replay, conservation_certificate_sha256, ledger_pack_sha256,
    ledger_unit_bindings_sha256, prepare_quantity_posting, seal_ledger_pack,
    verify_conservation_certificate, verify_quantity_commit_observation,
};
pub use expression::lower_ledger_amount_expression;

/// Stable diagnostic codes emitted by [`validate_ledger_pack`].
pub mod diagnostic_codes {
    pub const TOO_MANY_DIAGNOSTICS: &str = "ledger.validation.too_many_diagnostics";
    pub const UNSUPPORTED_IR_VERSION: &str = "ledger.pack.unsupported_ir_version";
    pub const EMPTY_PACK_ID: &str = "ledger.pack.empty_id";
    pub const EMPTY_ONTOLOGY_ARTIFACT_ID: &str = "ledger.ontology.empty_artifact_id";
    pub const NON_EXACT_ONTOLOGY_ARTIFACT_ID: &str = "ledger.ontology.non_exact_artifact_id";
    pub const EMPTY_ONTOLOGY_ARTIFACT_VERSION: &str = "ledger.ontology.empty_artifact_version";
    pub const NON_EXACT_ONTOLOGY_ARTIFACT_VERSION: &str =
        "ledger.ontology.non_exact_artifact_version";
    pub const INVALID_ONTOLOGY_ARTIFACT_SHA256: &str = "ledger.ontology.invalid_artifact_sha256";
    pub const NO_UNITS: &str = "ledger.pack.no_units";
    pub const NO_TEMPLATES: &str = "ledger.pack.no_templates";
    pub const EMPTY_ID: &str = "ledger.identifier.empty";
    pub const DUPLICATE_UNIT: &str = "ledger.unit.duplicate";
    pub const EMPTY_UNIT_TERM: &str = "ledger.unit.empty_unit_term";
    pub const EMPTY_QUANTITY_KIND_TERM: &str = "ledger.unit.empty_quantity_kind_term";
    pub const INVALID_UNIT_TERM: &str = "ledger.unit.invalid_unit_term";
    pub const INVALID_QUANTITY_KIND_TERM: &str = "ledger.unit.invalid_quantity_kind_term";
    pub const DUPLICATE_UNIT_TERM: &str = "ledger.unit.duplicate_unit_term";
    pub const INCONSISTENT_DIMENSION_BINDING: &str = "ledger.unit.inconsistent_dimension_binding";
    pub const UNIT_SCALE_OUT_OF_RANGE: &str = "ledger.unit.scale_out_of_range";
    pub const NO_ACCOUNTS: &str = "ledger.pack.no_accounts";
    pub const DUPLICATE_ACCOUNT: &str = "ledger.account.duplicate";
    pub const ACCOUNT_NO_UNITS: &str = "ledger.account.no_units";
    pub const ACCOUNT_DUPLICATE_UNIT: &str = "ledger.account.duplicate_unit";
    pub const ACCOUNT_UNKNOWN_UNIT: &str = "ledger.account.unknown_unit";
    pub const UNKNOWN_ACCOUNT: &str = "ledger.leg.unknown_account";
    pub const ACCOUNT_UNIT_MISMATCH: &str = "ledger.leg.account_unit_mismatch";
    pub const NO_LAYERS: &str = "ledger.pack.no_layers";
    pub const DUPLICATE_LAYER: &str = "ledger.layer.duplicate";
    pub const DUPLICATE_TEMPLATE: &str = "ledger.template.duplicate";
    pub const TOO_FEW_LEGS: &str = "ledger.template.too_few_legs";
    pub const DUPLICATE_LEG: &str = "ledger.leg.duplicate";
    pub const DUPLICATE_PARAMETER: &str = "ledger.parameter.duplicate";
    pub const PARAMETER_SCALE_OUT_OF_RANGE: &str = "ledger.parameter.scale_out_of_range";
    pub const INVALID_INTEGER: &str = "ledger.numeric.invalid_integer";
    pub const NON_CANONICAL_INTEGER: &str = "ledger.numeric.non_canonical_integer";
    pub const INVALID_BOUNDS: &str = "ledger.parameter.invalid_bounds";
    pub const UNKNOWN_UNIT: &str = "ledger.leg.unknown_unit";
    pub const EMPTY_LAYER: &str = "ledger.leg.empty_layer";
    pub const UNKNOWN_LAYER: &str = "ledger.leg.unknown_layer";
    pub const RESULT_SCALE_OUT_OF_RANGE: &str = "ledger.amount.result_scale_out_of_range";
    pub const RESULT_SCALE_EXCEEDS_UNIT: &str = "ledger.amount.result_scale_exceeds_unit";
    pub const CONSTANT_SCALE_EXCEEDS_RESULT: &str = "ledger.amount.constant_scale_exceeds_result";
    pub const UNKNOWN_PARAMETER: &str = "ledger.amount.unknown_parameter";
    pub const DUPLICATE_TERM: &str = "ledger.amount.duplicate_parameter_term";
    pub const TERM_SCALE_EXCEEDS_RESULT: &str = "ledger.amount.term_scale_exceeds_result";
    pub const NORMALIZATION_OVERFLOW: &str = "ledger.amount.normalization_overflow";
    pub const BOUND_OVERFLOW: &str = "ledger.amount.bound_overflow";
    pub const RESIDUAL_OVERFLOW: &str = "ledger.template.residual_overflow";
    pub const UNBALANCED_CONSTANT: &str = "ledger.template.unbalanced_constant";
    pub const UNBALANCED_COEFFICIENT: &str = "ledger.template.unbalanced_coefficient";
    pub const UNKNOWN_TEMPLATE: &str = "ledger.runtime.unknown_template";
    pub const DUPLICATE_INPUT: &str = "ledger.runtime.duplicate_parameter";
    pub const UNKNOWN_INPUT: &str = "ledger.runtime.unknown_parameter";
    pub const MISSING_INPUT: &str = "ledger.runtime.missing_parameter";
    pub const INPUT_SCALE_MISMATCH: &str = "ledger.runtime.parameter_scale_mismatch";
    pub const INPUT_OUT_OF_BOUNDS: &str = "ledger.runtime.parameter_out_of_bounds";
    pub const EVALUATION_OVERFLOW: &str = "ledger.runtime.evaluation_overflow";
    pub const RUNTIME_UNBALANCED: &str = "ledger.runtime.unbalanced";
    pub const CERTIFICATE_PACK_MISMATCH: &str = "ledger.certificate.pack_mismatch";
    pub const CERTIFICATE_VERSION_MISMATCH: &str = "ledger.certificate.version_mismatch";
    pub const CERTIFICATE_HASH_MISMATCH: &str = "ledger.certificate.hash_mismatch";
    pub const CERTIFICATE_CLAIM_MISMATCH: &str = "ledger.certificate.claim_mismatch";
    pub const POSTING_CONTRACT_VERSION_MISMATCH: &str = "ledger.posting.contract_version_mismatch";
    pub const POSTING_REFERENCE_MISMATCH: &str = "ledger.posting.reference_mismatch";
    pub const POSTING_IDENTITY_EMPTY: &str = "ledger.posting.identity_empty";
    pub const CERTIFICATE_COVERAGE_MISMATCH: &str = "ledger.posting.certificate_coverage_mismatch";
    pub const PARTIAL_COMMIT_EVIDENCE: &str = "ledger.commit.partial_evidence";
    pub const COMMIT_EVIDENCE_MISMATCH: &str = "ledger.commit.evidence_mismatch";
    pub const IDEMPOTENCY_RECORD_MISMATCH: &str = "ledger.commit.idempotency_mismatch";
    pub const OWNER_VERSION_OVERFLOW: &str = "ledger.commit.owner_version_overflow";
}

/// Maximum returned diagnostics, including the final truncation refusal.
pub const MAX_LEDGER_DIAGNOSTICS: usize = 64;
/// Maximum JSON bytes of the returned diagnostic array, including escaping.
pub const MAX_LEDGER_DIAGNOSTIC_BYTES: usize = 64 * 1024;

// Keep the deterministic first N findings and reserve room for a small typed
// refusal. The marker never copies caller-controlled identifiers or messages.
#[derive(Default)]
struct LedgerDiagnostics {
    entries: Vec<Diagnostic>,
    json_bytes: usize,
    truncated: bool,
}

impl LedgerDiagnostics {
    fn push(&mut self, diagnostic: Diagnostic) {
        const MARKER_RESERVE: usize = 512;
        if self.truncated {
            return;
        }
        let remaining = MAX_LEDGER_DIAGNOSTIC_BYTES - MARKER_RESERVE;
        // Avoid serializing a single oversized diagnostic just to reject it.
        let string_bytes = diagnostic
            .code
            .len()
            .saturating_add(diagnostic.message.len())
            .saturating_add(diagnostic.path.as_ref().map_or(0, String::len))
            .saturating_add(diagnostic.artifact_id.as_ref().map_or(0, String::len));
        let bytes = if string_bytes > remaining {
            usize::MAX
        } else {
            serde_json::to_string(&diagnostic).map_or(usize::MAX, |json| json.len())
        };
        // Include the enclosing array and a comma for every entry.
        let next_bytes = self
            .json_bytes
            .max(2)
            .saturating_add(bytes)
            .saturating_add(1);
        if self.entries.len() < MAX_LEDGER_DIAGNOSTICS - 1 && next_bytes <= remaining {
            self.entries.push(diagnostic);
            self.json_bytes = next_bytes;
        } else {
            self.truncated = true;
            self.entries.push(diag(
                diagnostic_codes::TOO_MANY_DIAGNOSTICS,
                "ledger validation stopped at its diagnostic count or byte limit",
                "",
                None,
            ));
        }
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Successfully normalized pack consumed by the exact runtime evaluator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledLedgerPack {
    pub pack_id: String,
    pub templates: BTreeMap<String, CompiledLedgerTemplate>,
}

/// One checked template with exact parameter premises and fixed legs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledLedgerTemplate {
    pub template_id: String,
    parameters: BTreeMap<String, ParameterModel>,
    pub legs: Vec<CompiledLedgerLeg>,
}

/// A leg normalized to its unit's canonical stored precision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledLedgerLeg {
    pub leg_id: String,
    pub account_id: String,
    pub unit_id: String,
    pub layer: String,
    pub unit_precision: u8,
    amount: NormalizedLinear,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParameterModel {
    scale: u8,
    min_unscaled: i128,
    max_unscaled: i128,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct NormalizedLinear {
    constant: i128,
    coefficients: BTreeMap<String, i128>,
}

/// Validates identifiers, exact numeric premises, bounded evaluation, and
/// symbolic conservation for a ledger pack.
#[must_use]
pub fn validate_ledger_pack(pack: &LedgerPackIr) -> LedgerPackValidation {
    check_ledger_pack(pack).0
}

/// Compiles a valid pack into the one normalized representation consumed at
/// runtime. Invalid packs return deterministic first-N diagnostics and a typed refusal
/// when the count or serialized-byte budget is exhausted.
pub fn compile_ledger_pack(
    pack: &LedgerPackIr,
) -> Result<CompiledLedgerPack, LedgerPackValidation> {
    let (validation, compiled) = check_ledger_pack(pack);
    compiled.ok_or(validation)
}

fn check_ledger_pack(pack: &LedgerPackIr) -> (LedgerPackValidation, Option<CompiledLedgerPack>) {
    let mut diagnostics = LedgerDiagnostics::default();
    let artifact_id = nonempty(pack.pack_id.as_str()).then(|| pack.pack_id.clone());
    let pack_sha256 = ledger_pack_sha256(pack);

    if pack.ir_version != LEDGER_PACK_IR_VERSION {
        diagnostics.push(diag(
            diagnostic_codes::UNSUPPORTED_IR_VERSION,
            format!(
                "unsupported ledger pack IR version `{}`; expected `{LEDGER_PACK_IR_VERSION}`",
                pack.ir_version
            ),
            "irVersion",
            artifact_id.clone(),
        ));
    }
    if artifact_id.is_none() {
        diagnostics.push(diag(
            diagnostic_codes::EMPTY_PACK_ID,
            "packId must be non-empty",
            "packId",
            None,
        ));
    }
    validate_ontology_artifact_pin(pack, artifact_id.clone(), &mut diagnostics);
    if pack.units.is_empty() {
        diagnostics.push(diag(
            diagnostic_codes::NO_UNITS,
            "at least one conserved unit is required",
            "units",
            artifact_id.clone(),
        ));
    }
    if pack.accounts.is_empty() {
        diagnostics.push(diag(
            diagnostic_codes::NO_ACCOUNTS,
            "at least one posting account is required",
            "accounts",
            artifact_id.clone(),
        ));
    }
    if pack.layers.is_empty() {
        diagnostics.push(diag(
            diagnostic_codes::NO_LAYERS,
            "at least one declared balance layer is required",
            "layers",
            artifact_id.clone(),
        ));
    }
    if pack.templates.is_empty() {
        diagnostics.push(diag(
            diagnostic_codes::NO_TEMPLATES,
            "at least one transaction template is required",
            "templates",
            artifact_id.clone(),
        ));
    }

    let units = validate_units(pack, &artifact_id, &mut diagnostics);
    let accounts = validate_accounts(pack, &units, &artifact_id, &mut diagnostics);
    let layers = validate_layers(pack, &artifact_id, &mut diagnostics);
    let mut template_ids = BTreeSet::new();
    let mut certificates = Vec::new();
    let mut compiled_templates = BTreeMap::new();

    for (template_index, template) in pack.templates.iter().enumerate() {
        if diagnostics.truncated {
            break;
        }
        let template_path = format!("templates[{template_index}]");
        validate_identifier(
            template.template_id.as_str(),
            format!("{template_path}.templateId"),
            artifact_id.clone(),
            &mut diagnostics,
        );
        if !template_ids.insert(template.template_id.as_str()) {
            diagnostics.push(diag(
                diagnostic_codes::DUPLICATE_TEMPLATE,
                format!("duplicate template id `{}`", template.template_id),
                format!("{template_path}.templateId"),
                artifact_id.clone(),
            ));
        }
        if template.legs.len() < 2 {
            diagnostics.push(diag(
                diagnostic_codes::TOO_FEW_LEGS,
                "a conserved transaction template requires at least two legs",
                format!("{template_path}.legs"),
                artifact_id.clone(),
            ));
        }

        let parameters = validate_parameters(
            template,
            template_path.as_str(),
            &artifact_id,
            &mut diagnostics,
        );
        let mut residuals: BTreeMap<(String, String), NormalizedLinear> = BTreeMap::new();
        let mut compiled_legs = Vec::new();
        let mut leg_ids = BTreeSet::new();

        for (leg_index, leg) in template.legs.iter().enumerate() {
            if diagnostics.truncated {
                break;
            }
            let leg_path = format!("{template_path}.legs[{leg_index}]");
            validate_identifier(
                leg.leg_id.as_str(),
                format!("{leg_path}.legId"),
                artifact_id.clone(),
                &mut diagnostics,
            );
            if !leg_ids.insert(leg.leg_id.as_str()) {
                diagnostics.push(diag(
                    diagnostic_codes::DUPLICATE_LEG,
                    format!("duplicate leg id `{}`", leg.leg_id),
                    format!("{leg_path}.legId"),
                    artifact_id.clone(),
                ));
            }
            validate_identifier(
                leg.account_id.as_str(),
                format!("{leg_path}.accountId"),
                artifact_id.clone(),
                &mut diagnostics,
            );
            match accounts.get(leg.account_id.as_str()) {
                None => diagnostics.push(diag(
                    diagnostic_codes::UNKNOWN_ACCOUNT,
                    format!("unknown account `{}`", leg.account_id),
                    format!("{leg_path}.accountId"),
                    artifact_id.clone(),
                )),
                Some(allowed_units) if !allowed_units.contains(leg.unit_id.as_str()) => {
                    diagnostics.push(diag(
                        diagnostic_codes::ACCOUNT_UNIT_MISMATCH,
                        format!(
                            "account `{}` does not admit unit `{}`",
                            leg.account_id, leg.unit_id
                        ),
                        format!("{leg_path}.unitId"),
                        artifact_id.clone(),
                    ));
                }
                Some(_) => {}
            }
            if !nonempty(leg.layer.as_str()) {
                diagnostics.push(diag(
                    diagnostic_codes::EMPTY_LAYER,
                    "layer must be non-empty",
                    format!("{leg_path}.layer"),
                    artifact_id.clone(),
                ));
            } else if !layers.contains(leg.layer.as_str()) {
                diagnostics.push(diag(
                    diagnostic_codes::UNKNOWN_LAYER,
                    format!("unknown layer `{}`", leg.layer),
                    format!("{leg_path}.layer"),
                    artifact_id.clone(),
                ));
            }

            let Some(unit) = units.get(leg.unit_id.as_str()) else {
                diagnostics.push(diag(
                    diagnostic_codes::UNKNOWN_UNIT,
                    format!("unknown unit `{}`", leg.unit_id),
                    format!("{leg_path}.unitId"),
                    artifact_id.clone(),
                ));
                continue;
            };

            let Some(normalized) = normalize_amount(
                &leg.amount,
                unit.precision,
                &parameters,
                format!("{leg_path}.amount").as_str(),
                &artifact_id,
                &mut diagnostics,
            ) else {
                continue;
            };

            compiled_legs.push(CompiledLedgerLeg {
                leg_id: leg.leg_id.clone(),
                account_id: leg.account_id.clone(),
                unit_id: leg.unit_id.clone(),
                layer: leg.layer.clone(),
                unit_precision: unit.precision,
                amount: normalized.clone(),
            });

            let key = (leg.unit_id.clone(), leg.layer.clone());
            let residual = residuals.entry(key).or_default();
            let mut prefix_is_representable = true;
            if let Some(sum) = residual.constant.checked_add(normalized.constant) {
                residual.constant = sum;
            } else {
                prefix_is_representable = false;
                diagnostics.push(diag(
                    diagnostic_codes::RESIDUAL_OVERFLOW,
                    "constant residual overflowed i128 while combining legs",
                    format!("{leg_path}.amount.constantUnscaled"),
                    artifact_id.clone(),
                ));
            }
            for (parameter_id, coefficient) in normalized.coefficients {
                if diagnostics.truncated {
                    break;
                }
                let previous = residual
                    .coefficients
                    .get(parameter_id.as_str())
                    .copied()
                    .unwrap_or(0);
                if let Some(sum) = previous.checked_add(coefficient) {
                    residual.coefficients.insert(parameter_id, sum);
                } else {
                    prefix_is_representable = false;
                    diagnostics.push(diag(
                        diagnostic_codes::RESIDUAL_OVERFLOW,
                        "parameter coefficient residual overflowed i128 while combining legs",
                        format!("{leg_path}.amount.terms"),
                        artifact_id.clone(),
                    ));
                }
            }
            if prefix_is_representable {
                validate_runtime_prefix_bounds(
                    residual,
                    &parameters,
                    leg.leg_id.as_str(),
                    format!("{leg_path}.amount").as_str(),
                    artifact_id.clone(),
                    &mut diagnostics,
                );
            }
        }

        for ((unit_id, layer), residual) in residuals {
            if diagnostics.truncated {
                break;
            }
            if residual.constant != 0 {
                diagnostics.push(diag(
                    diagnostic_codes::UNBALANCED_CONSTANT,
                    format!(
                        "template `{}` has constant residual {} for unit `{unit_id}` layer `{layer}`",
                        template.template_id, residual.constant
                    ),
                    template_path.clone(),
                    artifact_id.clone(),
                ));
            }
            for (parameter_id, coefficient) in &residual.coefficients {
                if diagnostics.truncated {
                    break;
                }
                if *coefficient != 0 {
                    diagnostics.push(diag(
                        diagnostic_codes::UNBALANCED_COEFFICIENT,
                        format!(
                            "template `{}` has coefficient residual {coefficient} for parameter `{parameter_id}`, unit `{unit_id}`, layer `{layer}`",
                            template.template_id
                        ),
                        template_path.clone(),
                        artifact_id.clone(),
                    ));
                }
            }

            if residual.constant == 0
                && residual
                    .coefficients
                    .values()
                    .all(|coefficient| *coefficient == 0)
            {
                let parameter_residuals_unscaled = residual
                    .coefficients
                    .into_iter()
                    .map(|(parameter_id, coefficient)| (parameter_id, coefficient.to_string()))
                    .collect();
                let mut certificate = ConservationCertificate {
                    certificate_version: CONSERVATION_CERTIFICATE_VERSION.to_string(),
                    certificate_sha256: String::new(),
                    ir_version: pack.ir_version.clone(),
                    checker_version: LEDGER_PACK_CHECKER_VERSION.to_string(),
                    pack_id: pack.pack_id.clone(),
                    pack_sha256: pack_sha256.clone(),
                    ontology_artifact: pack.ontology_artifact.clone(),
                    template_id: template.template_id.clone(),
                    unit_id: unit_id.clone(),
                    unit_term_id: units
                        .get(unit_id.as_str())
                        .map_or_else(String::new, |definition| definition.unit_term_id.clone()),
                    quantity_kind_term_id: units
                        .get(unit_id.as_str())
                        .map_or_else(String::new, |definition| {
                            definition.quantity_kind_term_id.clone()
                        }),
                    layer,
                    unit_precision: units
                        .get(unit_id.as_str())
                        .map_or(0, |definition| definition.precision),
                    constant_residual_unscaled: "0".to_string(),
                    parameter_residuals_unscaled,
                };
                certificate.certificate_sha256 = conservation_certificate_sha256(&certificate);
                certificates.push(certificate);
            }
        }

        compiled_templates.insert(
            template.template_id.clone(),
            CompiledLedgerTemplate {
                template_id: template.template_id.clone(),
                parameters,
                legs: compiled_legs,
            },
        );
    }

    let ok = diagnostics.is_empty();
    if !ok {
        certificates.clear();
    }
    let validation = LedgerPackValidation {
        ok,
        diagnostics: diagnostics.entries,
        certificates,
    };
    let compiled = validation.ok.then(|| CompiledLedgerPack {
        pack_id: pack.pack_id.clone(),
        templates: compiled_templates,
    });
    (validation, compiled)
}

/// Evaluates one compiled template with the same unit-scale coefficients used
/// by the conservation checker. Inputs must use their declared scale and lie
/// within their sealed bounds. Every operation remains checked `i128`.
pub fn evaluate_ledger_template(
    pack: &CompiledLedgerPack,
    template_id: &str,
    inputs: &[LedgerParameterValue],
) -> Result<LedgerTemplateEvaluation, Vec<Diagnostic>> {
    let artifact_id = Some(pack.pack_id.clone());
    let Some(template) = pack.templates.get(template_id) else {
        return Err(vec![diag(
            diagnostic_codes::UNKNOWN_TEMPLATE,
            format!("unknown compiled template `{template_id}`"),
            "templateId",
            artifact_id,
        )]);
    };

    let mut diagnostics = LedgerDiagnostics::default();
    let mut seen_inputs = BTreeSet::new();
    let mut values = BTreeMap::new();
    for (input_index, input) in inputs.iter().enumerate() {
        if diagnostics.truncated {
            break;
        }
        let path = format!("parameters[{input_index}]");
        if !seen_inputs.insert(input.parameter_id.as_str()) {
            diagnostics.push(diag(
                diagnostic_codes::DUPLICATE_INPUT,
                format!("duplicate runtime parameter `{}`", input.parameter_id),
                format!("{path}.parameterId"),
                artifact_id.clone(),
            ));
            continue;
        }
        let Some(parameter) = template.parameters.get(input.parameter_id.as_str()) else {
            diagnostics.push(diag(
                diagnostic_codes::UNKNOWN_INPUT,
                format!("unknown runtime parameter `{}`", input.parameter_id),
                format!("{path}.parameterId"),
                artifact_id.clone(),
            ));
            continue;
        };
        if input.scale != parameter.scale {
            diagnostics.push(diag(
                diagnostic_codes::INPUT_SCALE_MISMATCH,
                format!(
                    "runtime parameter `{}` has scale {}; expected {}",
                    input.parameter_id, input.scale, parameter.scale
                ),
                format!("{path}.scale"),
                artifact_id.clone(),
            ));
            continue;
        }
        let Some(value) = parse_i128(
            input.unscaled.as_str(),
            format!("{path}.unscaled"),
            artifact_id.clone(),
            &mut diagnostics,
        ) else {
            continue;
        };
        if value < parameter.min_unscaled || value > parameter.max_unscaled {
            diagnostics.push(diag(
                diagnostic_codes::INPUT_OUT_OF_BOUNDS,
                format!(
                    "runtime parameter `{}` value {value} is outside sealed bounds [{}, {}]",
                    input.parameter_id, parameter.min_unscaled, parameter.max_unscaled
                ),
                format!("{path}.unscaled"),
                artifact_id.clone(),
            ));
            continue;
        }
        values.insert(input.parameter_id.clone(), value);
    }
    for parameter_id in template.parameters.keys() {
        if diagnostics.truncated {
            break;
        }
        if !seen_inputs.contains(parameter_id.as_str()) {
            diagnostics.push(diag(
                diagnostic_codes::MISSING_INPUT,
                format!("missing runtime parameter `{parameter_id}`"),
                "parameters",
                artifact_id.clone(),
            ));
        }
    }
    if !diagnostics.is_empty() {
        return Err(diagnostics.entries);
    }

    let mut evaluated_legs = Vec::with_capacity(template.legs.len());
    let mut runtime_residuals: BTreeMap<(String, String), i128> = BTreeMap::new();
    for (leg_index, leg) in template.legs.iter().enumerate() {
        if diagnostics.truncated {
            break;
        }
        let mut amount = leg.amount.constant;
        for (parameter_id, coefficient) in &leg.amount.coefficients {
            if diagnostics.truncated {
                break;
            }
            let Some(input) = values.get(parameter_id.as_str()).copied() else {
                diagnostics.push(diag(
                    diagnostic_codes::MISSING_INPUT,
                    format!("compiled leg requires missing parameter `{parameter_id}`"),
                    format!("legs[{leg_index}].amount"),
                    artifact_id.clone(),
                ));
                continue;
            };
            let Some(term_value) = coefficient.checked_mul(input) else {
                diagnostics.push(diag(
                    diagnostic_codes::EVALUATION_OVERFLOW,
                    format!("coefficient multiplication overflowed for parameter `{parameter_id}`"),
                    format!("legs[{leg_index}].amount"),
                    artifact_id.clone(),
                ));
                continue;
            };
            let Some(next_amount) = amount.checked_add(term_value) else {
                diagnostics.push(diag(
                    diagnostic_codes::EVALUATION_OVERFLOW,
                    format!("leg `{}` amount overflowed i128", leg.leg_id),
                    format!("legs[{leg_index}].amount"),
                    artifact_id.clone(),
                ));
                continue;
            };
            amount = next_amount;
        }

        let key = (leg.unit_id.clone(), leg.layer.clone());
        let previous = runtime_residuals.get(&key).copied().unwrap_or(0);
        if let Some(residual) = previous.checked_add(amount) {
            runtime_residuals.insert(key, residual);
        } else {
            diagnostics.push(diag(
                diagnostic_codes::EVALUATION_OVERFLOW,
                "runtime balance assertion overflowed i128",
                format!("legs[{leg_index}].amount"),
                artifact_id.clone(),
            ));
        }
        evaluated_legs.push(EvaluatedLedgerLeg {
            leg_id: leg.leg_id.clone(),
            account_id: leg.account_id.clone(),
            unit_id: leg.unit_id.clone(),
            layer: leg.layer.clone(),
            unit_precision: leg.unit_precision,
            amount_unscaled: amount.to_string(),
        });
    }
    for ((unit_id, layer), residual) in runtime_residuals {
        if diagnostics.truncated {
            break;
        }
        if residual != 0 {
            diagnostics.push(diag(
                diagnostic_codes::RUNTIME_UNBALANCED,
                format!("runtime residual {residual} for unit `{unit_id}` layer `{layer}`"),
                "legs",
                artifact_id.clone(),
            ));
        }
    }
    if !diagnostics.is_empty() {
        return Err(diagnostics.entries);
    }

    Ok(LedgerTemplateEvaluation {
        pack_id: pack.pack_id.clone(),
        template_id: template.template_id.clone(),
        legs: evaluated_legs,
    })
}

fn validate_layers<'a>(
    pack: &'a LedgerPackIr,
    artifact_id: &Option<String>,
    diagnostics: &mut LedgerDiagnostics,
) -> BTreeSet<&'a str> {
    let mut layers = BTreeSet::new();
    for (index, layer) in pack.layers.iter().enumerate() {
        if diagnostics.truncated {
            break;
        }
        let path = format!("layers[{index}].layerId");
        validate_identifier(
            layer.layer_id.as_str(),
            path.clone(),
            artifact_id.clone(),
            diagnostics,
        );
        if !layers.insert(layer.layer_id.as_str()) {
            diagnostics.push(diag(
                diagnostic_codes::DUPLICATE_LAYER,
                format!("duplicate layer id `{}`", layer.layer_id),
                path,
                artifact_id.clone(),
            ));
        }
    }
    layers
}

fn validate_accounts<'a>(
    pack: &'a LedgerPackIr,
    units: &BTreeMap<&str, &LedgerUnitIr>,
    artifact_id: &Option<String>,
    diagnostics: &mut LedgerDiagnostics,
) -> BTreeMap<&'a str, BTreeSet<&'a str>> {
    let mut accounts = BTreeMap::new();
    for (account_index, account) in pack.accounts.iter().enumerate() {
        if diagnostics.truncated {
            break;
        }
        let account_path = format!("accounts[{account_index}]");
        validate_identifier(
            account.account_id.as_str(),
            format!("{account_path}.accountId"),
            artifact_id.clone(),
            diagnostics,
        );
        if account.allowed_unit_ids.is_empty() {
            diagnostics.push(diag(
                diagnostic_codes::ACCOUNT_NO_UNITS,
                format!(
                    "account `{}` must admit at least one unit",
                    account.account_id
                ),
                format!("{account_path}.allowedUnitIds"),
                artifact_id.clone(),
            ));
        }
        let mut allowed_units = BTreeSet::new();
        for (unit_index, unit_id) in account.allowed_unit_ids.iter().enumerate() {
            if diagnostics.truncated {
                break;
            }
            let unit_path = format!("{account_path}.allowedUnitIds[{unit_index}]");
            validate_identifier(
                unit_id.as_str(),
                unit_path.clone(),
                artifact_id.clone(),
                diagnostics,
            );
            if !allowed_units.insert(unit_id.as_str()) {
                diagnostics.push(diag(
                    diagnostic_codes::ACCOUNT_DUPLICATE_UNIT,
                    format!(
                        "account `{}` declares unit `{unit_id}` more than once",
                        account.account_id
                    ),
                    unit_path.clone(),
                    artifact_id.clone(),
                ));
            }
            if !units.contains_key(unit_id.as_str()) {
                diagnostics.push(diag(
                    diagnostic_codes::ACCOUNT_UNKNOWN_UNIT,
                    format!(
                        "account `{}` references unknown unit `{unit_id}`",
                        account.account_id
                    ),
                    unit_path,
                    artifact_id.clone(),
                ));
            }
        }
        if accounts
            .insert(account.account_id.as_str(), allowed_units)
            .is_some()
        {
            diagnostics.push(diag(
                diagnostic_codes::DUPLICATE_ACCOUNT,
                format!("duplicate account id `{}`", account.account_id),
                format!("{account_path}.accountId"),
                artifact_id.clone(),
            ));
        }
    }
    accounts
}

fn validate_units<'a>(
    pack: &'a LedgerPackIr,
    artifact_id: &Option<String>,
    diagnostics: &mut LedgerDiagnostics,
) -> BTreeMap<&'a str, &'a LedgerUnitIr> {
    let mut units = BTreeMap::new();
    let mut unit_terms = BTreeSet::new();
    let mut quantity_kind_terms = BTreeSet::new();
    for (index, unit) in pack.units.iter().enumerate() {
        if diagnostics.truncated {
            break;
        }
        let path = format!("units[{index}]");
        validate_identifier(
            unit.unit_id.as_str(),
            format!("{path}.unitId"),
            artifact_id.clone(),
            diagnostics,
        );
        if !nonempty(unit.unit_term_id.as_str()) {
            diagnostics.push(diag(
                diagnostic_codes::EMPTY_UNIT_TERM,
                "unitTermId must be non-empty and canonical",
                format!("{path}.unitTermId"),
                artifact_id.clone(),
            ));
        } else if !is_exact_ontology_identity(unit.unit_term_id.as_str()) {
            diagnostics.push(diag(
                diagnostic_codes::INVALID_UNIT_TERM,
                "unitTermId must be one exact term identity without whitespace or wildcards",
                format!("{path}.unitTermId"),
                artifact_id.clone(),
            ));
        } else if !unit_terms.insert(unit.unit_term_id.as_str()) {
            diagnostics.push(diag(
                diagnostic_codes::DUPLICATE_UNIT_TERM,
                format!(
                    "ontology unit term `{}` is bound by more than one pack-local unit",
                    unit.unit_term_id
                ),
                format!("{path}.unitTermId"),
                artifact_id.clone(),
            ));
        }
        if !nonempty(unit.quantity_kind_term_id.as_str()) {
            diagnostics.push(diag(
                diagnostic_codes::EMPTY_QUANTITY_KIND_TERM,
                "quantityKindTermId must be non-empty and canonical",
                format!("{path}.quantityKindTermId"),
                artifact_id.clone(),
            ));
        } else if !is_exact_ontology_identity(unit.quantity_kind_term_id.as_str()) {
            diagnostics.push(diag(
                diagnostic_codes::INVALID_QUANTITY_KIND_TERM,
                "quantityKindTermId must be one exact term identity without whitespace or wildcards",
                format!("{path}.quantityKindTermId"),
                artifact_id.clone(),
            ));
        } else {
            quantity_kind_terms.insert(unit.quantity_kind_term_id.as_str());
        }
        if nonempty(unit.unit_term_id.as_str())
            && nonempty(unit.quantity_kind_term_id.as_str())
            && unit.unit_term_id == unit.quantity_kind_term_id
        {
            diagnostics.push(diag(
                diagnostic_codes::INCONSISTENT_DIMENSION_BINDING,
                "a unit term cannot also be its quantity-kind/dimension term",
                format!("{path}.quantityKindTermId"),
                artifact_id.clone(),
            ));
        }
        if unit.precision > MAX_LEDGER_SCALE {
            diagnostics.push(diag(
                diagnostic_codes::UNIT_SCALE_OUT_OF_RANGE,
                format!(
                    "unit precision {} exceeds maximum {MAX_LEDGER_SCALE}",
                    unit.precision
                ),
                format!("{path}.precision"),
                artifact_id.clone(),
            ));
        }
        if units.insert(unit.unit_id.as_str(), unit).is_some() {
            diagnostics.push(diag(
                diagnostic_codes::DUPLICATE_UNIT,
                format!("duplicate unit id `{}`", unit.unit_id),
                format!("{path}.unitId"),
                artifact_id.clone(),
            ));
        }
    }
    for (index, unit) in pack.units.iter().enumerate() {
        if diagnostics.truncated {
            break;
        }
        if unit_terms.contains(unit.quantity_kind_term_id.as_str())
            || quantity_kind_terms.contains(unit.unit_term_id.as_str())
        {
            diagnostics.push(diag(
                diagnostic_codes::INCONSISTENT_DIMENSION_BINDING,
                "unit-term and quantity-kind/dimension term roles must be disjoint within a pack",
                format!("units[{index}].unitTermId"),
                artifact_id.clone(),
            ));
        }
    }
    units
}

fn validate_ontology_artifact_pin(
    pack: &LedgerPackIr,
    artifact_id: Option<String>,
    diagnostics: &mut LedgerDiagnostics,
) {
    if !nonempty(pack.ontology_artifact.artifact_id.as_str()) {
        diagnostics.push(diag(
            diagnostic_codes::EMPTY_ONTOLOGY_ARTIFACT_ID,
            "ontology artifactId must be non-empty and canonical",
            "ontologyArtifact.artifactId",
            artifact_id.clone(),
        ));
    } else if !is_exact_ontology_identity(pack.ontology_artifact.artifact_id.as_str()) {
        diagnostics.push(diag(
            diagnostic_codes::NON_EXACT_ONTOLOGY_ARTIFACT_ID,
            "ontology artifactId must be one exact identity without whitespace or wildcards",
            "ontologyArtifact.artifactId",
            artifact_id.clone(),
        ));
    }
    if !nonempty(pack.ontology_artifact.artifact_version.as_str()) {
        diagnostics.push(diag(
            diagnostic_codes::EMPTY_ONTOLOGY_ARTIFACT_VERSION,
            "ontology artifactVersion must be non-empty and canonical",
            "ontologyArtifact.artifactVersion",
            artifact_id.clone(),
        ));
    } else if !is_exact_artifact_version(pack.ontology_artifact.artifact_version.as_str()) {
        diagnostics.push(diag(
            diagnostic_codes::NON_EXACT_ONTOLOGY_ARTIFACT_VERSION,
            "ontology artifactVersion must be one exact version, not a range or wildcard",
            "ontologyArtifact.artifactVersion",
            artifact_id.clone(),
        ));
    }
    if !is_canonical_sha256(pack.ontology_artifact.artifact_sha256.as_str()) {
        diagnostics.push(diag(
            diagnostic_codes::INVALID_ONTOLOGY_ARTIFACT_SHA256,
            "ontology artifactSha256 must be exactly 64 lowercase hexadecimal characters",
            "ontologyArtifact.artifactSha256",
            artifact_id,
        ));
    }
}

fn is_exact_ontology_identity(value: &str) -> bool {
    nonempty(value)
        && !value
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte == b'*')
}

fn is_exact_artifact_version(value: &str) -> bool {
    nonempty(value)
        && !value
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || b"*^~<>=,".contains(&byte))
}

fn is_canonical_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_parameters(
    template: &LedgerTemplateIr,
    template_path: &str,
    artifact_id: &Option<String>,
    diagnostics: &mut LedgerDiagnostics,
) -> BTreeMap<String, ParameterModel> {
    let mut parameters = BTreeMap::new();
    for (index, parameter) in template.parameters.iter().enumerate() {
        if diagnostics.truncated {
            break;
        }
        let path = format!("{template_path}.parameters[{index}]");
        validate_identifier(
            parameter.parameter_id.as_str(),
            format!("{path}.parameterId"),
            artifact_id.clone(),
            diagnostics,
        );
        if parameters.contains_key(parameter.parameter_id.as_str()) {
            diagnostics.push(diag(
                diagnostic_codes::DUPLICATE_PARAMETER,
                format!("duplicate parameter id `{}`", parameter.parameter_id),
                format!("{path}.parameterId"),
                artifact_id.clone(),
            ));
            continue;
        }
        if parameter.scale > MAX_LEDGER_SCALE {
            diagnostics.push(diag(
                diagnostic_codes::PARAMETER_SCALE_OUT_OF_RANGE,
                format!(
                    "parameter scale {} exceeds maximum {MAX_LEDGER_SCALE}",
                    parameter.scale
                ),
                format!("{path}.scale"),
                artifact_id.clone(),
            ));
            continue;
        }
        let Some(min_unscaled) = parse_i128(
            parameter.min_unscaled.as_str(),
            format!("{path}.minUnscaled"),
            artifact_id.clone(),
            diagnostics,
        ) else {
            continue;
        };
        let Some(max_unscaled) = parse_i128(
            parameter.max_unscaled.as_str(),
            format!("{path}.maxUnscaled"),
            artifact_id.clone(),
            diagnostics,
        ) else {
            continue;
        };
        if min_unscaled > max_unscaled {
            diagnostics.push(diag(
                diagnostic_codes::INVALID_BOUNDS,
                "minUnscaled must be less than or equal to maxUnscaled",
                path,
                artifact_id.clone(),
            ));
            continue;
        }
        parameters.insert(
            parameter.parameter_id.clone(),
            ParameterModel {
                scale: parameter.scale,
                min_unscaled,
                max_unscaled,
            },
        );
    }
    parameters
}

fn normalize_amount(
    amount: &LinearAmountIr,
    unit_precision: u8,
    parameters: &BTreeMap<String, ParameterModel>,
    path: &str,
    artifact_id: &Option<String>,
    diagnostics: &mut LedgerDiagnostics,
) -> Option<NormalizedLinear> {
    let initial_diagnostic_count = diagnostics.len();
    if amount.result_scale > MAX_LEDGER_SCALE {
        diagnostics.push(diag(
            diagnostic_codes::RESULT_SCALE_OUT_OF_RANGE,
            format!(
                "result scale {} exceeds maximum {MAX_LEDGER_SCALE}",
                amount.result_scale
            ),
            format!("{path}.resultScale"),
            artifact_id.clone(),
        ));
    }
    if amount.result_scale > unit_precision {
        diagnostics.push(diag(
            diagnostic_codes::RESULT_SCALE_EXCEEDS_UNIT,
            format!(
                "result scale {} exceeds unit precision {unit_precision}",
                amount.result_scale
            ),
            format!("{path}.resultScale"),
            artifact_id.clone(),
        ));
    }
    if amount.constant_scale > amount.result_scale {
        diagnostics.push(diag(
            diagnostic_codes::CONSTANT_SCALE_EXCEEDS_RESULT,
            format!(
                "constant scale {} exceeds result scale {}",
                amount.constant_scale, amount.result_scale
            ),
            format!("{path}.constantScale"),
            artifact_id.clone(),
        ));
    }

    let constant = parse_i128(
        amount.constant_unscaled.as_str(),
        format!("{path}.constantUnscaled"),
        artifact_id.clone(),
        diagnostics,
    );
    let result_unit_gap = unit_precision.checked_sub(amount.result_scale);
    let constant_result_gap = amount.result_scale.checked_sub(amount.constant_scale);
    let unit_multiplier = result_unit_gap.and_then(checked_pow10);
    let constant_result = constant
        .zip(constant_result_gap.and_then(checked_pow10))
        .and_then(|(value, multiplier)| value.checked_mul(multiplier));
    let constant_unit = constant_result
        .zip(unit_multiplier)
        .and_then(|(value, multiplier)| value.checked_mul(multiplier));
    if constant.is_some() && constant_unit.is_none() {
        diagnostics.push(diag(
            diagnostic_codes::NORMALIZATION_OVERFLOW,
            "constant cannot be normalized to unit precision without i128 overflow",
            format!("{path}.constantUnscaled"),
            artifact_id.clone(),
        ));
    }

    let mut coefficients = BTreeMap::new();
    let mut seen_parameters = BTreeSet::new();

    for (term_index, term) in amount.terms.iter().enumerate() {
        if diagnostics.truncated {
            break;
        }
        let term_path = format!("{path}.terms[{term_index}]");
        validate_identifier(
            term.parameter_id.as_str(),
            format!("{term_path}.parameterId"),
            artifact_id.clone(),
            diagnostics,
        );
        if !seen_parameters.insert(term.parameter_id.as_str()) {
            diagnostics.push(diag(
                diagnostic_codes::DUPLICATE_TERM,
                format!(
                    "parameter `{}` appears more than once; normalize it to one coefficient",
                    term.parameter_id
                ),
                format!("{term_path}.parameterId"),
                artifact_id.clone(),
            ));
            continue;
        }
        let Some(parameter) = parameters.get(term.parameter_id.as_str()) else {
            diagnostics.push(diag(
                diagnostic_codes::UNKNOWN_PARAMETER,
                format!("unknown or invalid parameter `{}`", term.parameter_id),
                format!("{term_path}.parameterId"),
                artifact_id.clone(),
            ));
            continue;
        };
        let coefficient = parse_i128(
            term.coefficient_unscaled.as_str(),
            format!("{term_path}.coefficientUnscaled"),
            artifact_id.clone(),
            diagnostics,
        );
        let Some(source_scale) = parameter.scale.checked_add(term.coefficient_scale) else {
            diagnostics.push(diag(
                diagnostic_codes::TERM_SCALE_EXCEEDS_RESULT,
                "parameter and coefficient scales overflow their representation",
                format!("{term_path}.coefficientScale"),
                artifact_id.clone(),
            ));
            continue;
        };
        let Some(result_gap) = amount.result_scale.checked_sub(source_scale) else {
            diagnostics.push(diag(
                diagnostic_codes::TERM_SCALE_EXCEEDS_RESULT,
                format!(
                    "parameter scale {} plus coefficient scale {} exceeds result scale {}",
                    parameter.scale, term.coefficient_scale, amount.result_scale
                ),
                format!("{term_path}.coefficientScale"),
                artifact_id.clone(),
            ));
            continue;
        };
        let factor_result = coefficient
            .zip(checked_pow10(result_gap))
            .and_then(|(value, multiplier)| value.checked_mul(multiplier));
        let Some(factor_result) = factor_result else {
            diagnostics.push(diag(
                diagnostic_codes::NORMALIZATION_OVERFLOW,
                "coefficient cannot be normalized to result scale without i128 overflow",
                format!("{term_path}.coefficientUnscaled"),
                artifact_id.clone(),
            ));
            continue;
        };
        let Some(factor_unit) =
            unit_multiplier.and_then(|multiplier| factor_result.checked_mul(multiplier))
        else {
            diagnostics.push(diag(
                diagnostic_codes::NORMALIZATION_OVERFLOW,
                "coefficient cannot be normalized to unit precision without i128 overflow",
                format!("{term_path}.coefficientUnscaled"),
                artifact_id.clone(),
            ));
            continue;
        };

        coefficients.insert(term.parameter_id.clone(), factor_unit);
    }

    // Runtime iterates this BTreeMap in parameter-id order. Prove bounds in
    // exactly that order; declaration-order analysis would be unsound because
    // checked addition can overflow in one order even when the final sum and a
    // different order fit i128.
    let constant_for_bounds = constant_unit?;
    let mut runtime_range_min = Some(constant_for_bounds);
    let mut runtime_range_max = Some(constant_for_bounds);
    for (parameter_id, factor_unit) in &coefficients {
        let Some(parameter) = parameters.get(parameter_id.as_str()) else {
            continue;
        };
        let endpoint_a = factor_unit.checked_mul(parameter.min_unscaled);
        let endpoint_b = factor_unit.checked_mul(parameter.max_unscaled);
        let (Some(endpoint_a), Some(endpoint_b)) = (endpoint_a, endpoint_b) else {
            diagnostics.push(diag(
                diagnostic_codes::BOUND_OVERFLOW,
                format!(
                    "runtime coefficient for parameter `{parameter_id}` overflows at a declared bound"
                ),
                format!("{path}.terms"),
                artifact_id.clone(),
            ));
            continue;
        };
        let term_min = endpoint_a.min(endpoint_b);
        let term_max = endpoint_a.max(endpoint_b);
        runtime_range_min = runtime_range_min.and_then(|value| value.checked_add(term_min));
        runtime_range_max = runtime_range_max.and_then(|value| value.checked_add(term_max));
        if runtime_range_min.is_none() || runtime_range_max.is_none() {
            diagnostics.push(diag(
                diagnostic_codes::BOUND_OVERFLOW,
                format!(
                    "parameter `{parameter_id}` can overflow the compiled runtime evaluation order"
                ),
                format!("{path}.terms"),
                artifact_id.clone(),
            ));
        }
    }

    if diagnostics.len() != initial_diagnostic_count {
        return None;
    }
    constant_unit.map(|constant| NormalizedLinear {
        constant,
        coefficients,
    })
}

/// Proves that the concrete runtime residual after one more leg fits `i128`
/// for every declared input. Per-leg bounds alone are insufficient: two safe
/// leg values can overflow when the evaluator adds the second value to the
/// partition's running residual. Combining coefficients first preserves
/// correlations when the same parameter appears in multiple legs and checks
/// prefixes in the exact filtered template-leg order used at runtime.
fn validate_runtime_prefix_bounds(
    prefix: &NormalizedLinear,
    parameters: &BTreeMap<String, ParameterModel>,
    leg_id: &str,
    path: &str,
    artifact_id: Option<String>,
    diagnostics: &mut LedgerDiagnostics,
) {
    let mut prefix_min = prefix.constant;
    let mut prefix_max = prefix.constant;
    for (parameter_id, coefficient) in &prefix.coefficients {
        if diagnostics.truncated {
            break;
        }
        let Some(parameter) = parameters.get(parameter_id.as_str()) else {
            return;
        };
        let Some(endpoint_a) = coefficient.checked_mul(parameter.min_unscaled) else {
            diagnostics.push(diag(
                diagnostic_codes::BOUND_OVERFLOW,
                format!(
                    "runtime partition prefix through leg `{leg_id}` can overflow for parameter `{parameter_id}`"
                ),
                path,
                artifact_id,
            ));
            return;
        };
        let Some(endpoint_b) = coefficient.checked_mul(parameter.max_unscaled) else {
            diagnostics.push(diag(
                diagnostic_codes::BOUND_OVERFLOW,
                format!(
                    "runtime partition prefix through leg `{leg_id}` can overflow for parameter `{parameter_id}`"
                ),
                path,
                artifact_id,
            ));
            return;
        };
        let Some(next_min) = prefix_min.checked_add(endpoint_a.min(endpoint_b)) else {
            diagnostics.push(diag(
                diagnostic_codes::BOUND_OVERFLOW,
                format!("runtime partition prefix through leg `{leg_id}` can overflow i128"),
                path,
                artifact_id,
            ));
            return;
        };
        let Some(next_max) = prefix_max.checked_add(endpoint_a.max(endpoint_b)) else {
            diagnostics.push(diag(
                diagnostic_codes::BOUND_OVERFLOW,
                format!("runtime partition prefix through leg `{leg_id}` can overflow i128"),
                path,
                artifact_id,
            ));
            return;
        };
        prefix_min = next_min;
        prefix_max = next_max;
    }
}

fn parse_i128(
    value: &str,
    path: String,
    artifact_id: Option<String>,
    diagnostics: &mut LedgerDiagnostics,
) -> Option<i128> {
    let Ok(parsed) = value.parse::<i128>() else {
        diagnostics.push(diag(
            diagnostic_codes::INVALID_INTEGER,
            format!("`{value}` is not a signed i128 decimal integer"),
            path,
            artifact_id,
        ));
        return None;
    };
    if parsed.to_string() != value {
        diagnostics.push(diag(
            diagnostic_codes::NON_CANONICAL_INTEGER,
            format!("`{value}` is not the canonical decimal encoding `{parsed}`"),
            path,
            artifact_id,
        ));
        return None;
    }
    Some(parsed)
}

fn checked_pow10(exponent: u8) -> Option<i128> {
    let mut value = 1_i128;
    for _ in 0..exponent {
        value = value.checked_mul(10)?;
    }
    Some(value)
}

fn validate_identifier(
    value: &str,
    path: String,
    artifact_id: Option<String>,
    diagnostics: &mut LedgerDiagnostics,
) {
    if !nonempty(value) {
        diagnostics.push(diag(
            diagnostic_codes::EMPTY_ID,
            "identifier must be non-empty and contain no surrounding whitespace",
            path,
            artifact_id,
        ));
    }
}

fn nonempty(value: &str) -> bool {
    !value.is_empty() && value.trim() == value
}

fn diag(
    code: &str,
    message: impl Into<String>,
    path: impl Into<String>,
    artifact_id: Option<String>,
) -> Diagnostic {
    Diagnostic {
        code: code.to_string(),
        severity: DiagnosticSeverity::Error,
        message: message.into(),
        artifact_id,
        path: Some(path.into()),
        remediation: Some(DiagnosticRemediation::FixDefinition),
    }
}
