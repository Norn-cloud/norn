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
mod decimal;
mod expression;
pub use boundary::{
    LedgerPackSeal, classify_quantity_replay, conservation_certificate_sha256, ledger_pack_sha256,
    ledger_unit_bindings_sha256, prepare_quantity_posting, seal_ledger_pack,
    verify_conservation_certificate, verify_quantity_commit_observation,
};
use decimal::checked_pow10;
pub use expression::lower_ledger_amount_expression;

/// Stable diagnostic codes emitted by [`validate_ledger_pack`].
pub mod diagnostic_codes {
    /// Validation stopped at the diagnostic count or serialized-byte cap.
    pub const TOO_MANY_DIAGNOSTICS: &str = "ledger.validation.too_many_diagnostics";
    /// Pack declares an unsupported ledger-pack IR version.
    pub const UNSUPPORTED_IR_VERSION: &str = "ledger.pack.unsupported_ir_version";
    /// Pack identifier is empty or has surrounding whitespace.
    pub const EMPTY_PACK_ID: &str = "ledger.pack.empty_id";
    /// Ontology artifact pin identifier is empty or has surrounding whitespace.
    pub const EMPTY_ONTOLOGY_ARTIFACT_ID: &str = "ledger.ontology.empty_artifact_id";
    /// Ontology artifact pin identifier contains whitespace or wildcards.
    pub const NON_EXACT_ONTOLOGY_ARTIFACT_ID: &str = "ledger.ontology.non_exact_artifact_id";
    /// Ontology artifact pin version is empty or has surrounding whitespace.
    pub const EMPTY_ONTOLOGY_ARTIFACT_VERSION: &str = "ledger.ontology.empty_artifact_version";
    /// Ontology artifact pin version is a range or wildcard instead of one exact version.
    pub const NON_EXACT_ONTOLOGY_ARTIFACT_VERSION: &str =
        "ledger.ontology.non_exact_artifact_version";
    /// Ontology artifact pin SHA-256 is not 64 lowercase hexadecimal characters.
    pub const INVALID_ONTOLOGY_ARTIFACT_SHA256: &str = "ledger.ontology.invalid_artifact_sha256";
    /// Pack declares no conserved units.
    pub const NO_UNITS: &str = "ledger.pack.no_units";
    /// Pack declares no transaction templates.
    pub const NO_TEMPLATES: &str = "ledger.pack.no_templates";
    /// Required ledger identifier is empty or has surrounding whitespace.
    pub const EMPTY_ID: &str = "ledger.identifier.empty";
    /// Unit identifier appears more than once in a pack.
    pub const DUPLICATE_UNIT: &str = "ledger.unit.duplicate";
    /// Unit ontology term identifier is empty or has surrounding whitespace.
    pub const EMPTY_UNIT_TERM: &str = "ledger.unit.empty_unit_term";
    /// Quantity-kind ontology term identifier is empty or has surrounding whitespace.
    pub const EMPTY_QUANTITY_KIND_TERM: &str = "ledger.unit.empty_quantity_kind_term";
    /// Unit ontology term identifier contains whitespace or wildcards.
    pub const INVALID_UNIT_TERM: &str = "ledger.unit.invalid_unit_term";
    /// Quantity-kind ontology term identifier contains whitespace or wildcards.
    pub const INVALID_QUANTITY_KIND_TERM: &str = "ledger.unit.invalid_quantity_kind_term";
    /// Ontology unit term is bound by more than one pack-local unit.
    pub const DUPLICATE_UNIT_TERM: &str = "ledger.unit.duplicate_unit_term";
    /// Unit-term and quantity-kind/dimension term roles collide within a pack.
    pub const INCONSISTENT_DIMENSION_BINDING: &str = "ledger.unit.inconsistent_dimension_binding";
    /// Unit precision exceeds `MAX_LEDGER_SCALE`.
    pub const UNIT_SCALE_OUT_OF_RANGE: &str = "ledger.unit.scale_out_of_range";
    /// Pack declares no posting accounts.
    pub const NO_ACCOUNTS: &str = "ledger.pack.no_accounts";
    /// Account identifier appears more than once in a pack.
    pub const DUPLICATE_ACCOUNT: &str = "ledger.account.duplicate";
    /// Account admits no units.
    pub const ACCOUNT_NO_UNITS: &str = "ledger.account.no_units";
    /// Account lists the same allowed unit more than once.
    pub const ACCOUNT_DUPLICATE_UNIT: &str = "ledger.account.duplicate_unit";
    /// Account references an undeclared unit identifier.
    pub const ACCOUNT_UNKNOWN_UNIT: &str = "ledger.account.unknown_unit";
    /// Template leg references an undeclared account identifier.
    pub const UNKNOWN_ACCOUNT: &str = "ledger.leg.unknown_account";
    /// Template leg posts a unit that its target account does not admit.
    pub const ACCOUNT_UNIT_MISMATCH: &str = "ledger.leg.account_unit_mismatch";
    /// Pack declares no balance layers.
    pub const NO_LAYERS: &str = "ledger.pack.no_layers";
    /// Balance layer identifier appears more than once in a pack.
    pub const DUPLICATE_LAYER: &str = "ledger.layer.duplicate";
    /// Template identifier appears more than once in a pack.
    pub const DUPLICATE_TEMPLATE: &str = "ledger.template.duplicate";
    /// Transaction template declares fewer than two legs.
    pub const TOO_FEW_LEGS: &str = "ledger.template.too_few_legs";
    /// Leg identifier appears more than once in a template.
    pub const DUPLICATE_LEG: &str = "ledger.leg.duplicate";
    /// Parameter identifier appears more than once in a template.
    pub const DUPLICATE_PARAMETER: &str = "ledger.parameter.duplicate";
    /// Parameter scale exceeds `MAX_LEDGER_SCALE`.
    pub const PARAMETER_SCALE_OUT_OF_RANGE: &str = "ledger.parameter.scale_out_of_range";
    /// Decimal string is not a valid signed `i128` integer.
    pub const INVALID_INTEGER: &str = "ledger.numeric.invalid_integer";
    /// Signed `i128` integer string is not in canonical decimal encoding.
    pub const NON_CANONICAL_INTEGER: &str = "ledger.numeric.non_canonical_integer";
    /// Parameter `minUnscaled` exceeds `maxUnscaled`.
    pub const INVALID_BOUNDS: &str = "ledger.parameter.invalid_bounds";
    /// Template leg references an undeclared unit identifier.
    pub const UNKNOWN_UNIT: &str = "ledger.leg.unknown_unit";
    /// Template leg layer identifier is empty or has surrounding whitespace.
    pub const EMPTY_LAYER: &str = "ledger.leg.empty_layer";
    /// Template leg references an undeclared balance layer.
    pub const UNKNOWN_LAYER: &str = "ledger.leg.unknown_layer";
    /// Amount `resultScale` exceeds `MAX_LEDGER_SCALE`.
    pub const RESULT_SCALE_OUT_OF_RANGE: &str = "ledger.amount.result_scale_out_of_range";
    /// Amount `resultScale` exceeds its leg unit's precision.
    pub const RESULT_SCALE_EXCEEDS_UNIT: &str = "ledger.amount.result_scale_exceeds_unit";
    /// Amount `constantScale` exceeds `resultScale`.
    pub const CONSTANT_SCALE_EXCEEDS_RESULT: &str = "ledger.amount.constant_scale_exceeds_result";
    /// Linear term references an undeclared or invalid template parameter.
    pub const UNKNOWN_PARAMETER: &str = "ledger.amount.unknown_parameter";
    /// Parameter appears more than once in one leg's linear terms.
    pub const DUPLICATE_TERM: &str = "ledger.amount.duplicate_parameter_term";
    /// Combined parameter and coefficient scale exceeds `resultScale`.
    pub const TERM_SCALE_EXCEEDS_RESULT: &str = "ledger.amount.term_scale_exceeds_result";
    /// Scaling a constant or coefficient to unit precision overflows `i128`.
    pub const NORMALIZATION_OVERFLOW: &str = "ledger.amount.normalization_overflow";
    /// Evaluating a leg or running partition prefix over declared bounds can overflow `i128`.
    pub const BOUND_OVERFLOW: &str = "ledger.amount.bound_overflow";
    /// Combining leg residuals across a `(unit, layer)` partition overflows `i128`.
    pub const RESIDUAL_OVERFLOW: &str = "ledger.template.residual_overflow";
    /// Template has a non-zero constant residual for a `(unit, layer)` partition.
    pub const UNBALANCED_CONSTANT: &str = "ledger.template.unbalanced_constant";
    /// Template has a non-zero parameter coefficient residual for a `(unit, layer)` partition.
    pub const UNBALANCED_COEFFICIENT: &str = "ledger.template.unbalanced_coefficient";
    /// Runtime evaluation requested an unknown compiled template identifier.
    pub const UNKNOWN_TEMPLATE: &str = "ledger.runtime.unknown_template";
    /// Runtime input list supplies the same parameter more than once.
    pub const DUPLICATE_INPUT: &str = "ledger.runtime.duplicate_parameter";
    /// Runtime input list supplies an undeclared parameter.
    pub const UNKNOWN_INPUT: &str = "ledger.runtime.unknown_parameter";
    /// Runtime input list omits a required template parameter.
    pub const MISSING_INPUT: &str = "ledger.runtime.missing_parameter";
    /// Runtime parameter scale does not match the compiled template declaration.
    pub const INPUT_SCALE_MISMATCH: &str = "ledger.runtime.parameter_scale_mismatch";
    /// Runtime parameter value lies outside its sealed `[min, max]` bounds.
    pub const INPUT_OUT_OF_BOUNDS: &str = "ledger.runtime.parameter_out_of_bounds";
    /// Runtime leg or residual evaluation overflowed `i128`.
    pub const EVALUATION_OVERFLOW: &str = "ledger.runtime.evaluation_overflow";
    /// Evaluated runtime legs do not sum to zero per `(unit, layer)` partition.
    pub const RUNTIME_UNBALANCED: &str = "ledger.runtime.unbalanced";
    /// Conservation certificate does not match the supplied ledger pack identity or digest.
    pub const CERTIFICATE_PACK_MISMATCH: &str = "ledger.certificate.pack_mismatch";
    /// Conservation certificate, IR, or checker version does not match the active checker.
    pub const CERTIFICATE_VERSION_MISMATCH: &str = "ledger.certificate.version_mismatch";
    /// Conservation certificate SHA-256 digest does not match its claims.
    pub const CERTIFICATE_HASH_MISMATCH: &str = "ledger.certificate.hash_mismatch";
    /// Conservation certificate claims do not match the deterministic checker result.
    pub const CERTIFICATE_CLAIM_MISMATCH: &str = "ledger.certificate.claim_mismatch";
    /// Posting intent declares an unsupported posting contract version.
    pub const POSTING_CONTRACT_VERSION_MISMATCH: &str = "ledger.posting.contract_version_mismatch";
    /// Posting intent pack or checker references do not match the installed seal.
    pub const POSTING_REFERENCE_MISMATCH: &str = "ledger.posting.reference_mismatch";
    /// Posting intent identity field is empty or has surrounding whitespace.
    pub const POSTING_IDENTITY_EMPTY: &str = "ledger.posting.identity_empty";
    /// Installed conservation certificates do not cover all evaluated `(unit, layer)` partitions.
    pub const CERTIFICATE_COVERAGE_MISMATCH: &str = "ledger.posting.certificate_coverage_mismatch";
    /// Post-commit authority observation is missing one or more required durable records.
    pub const PARTIAL_COMMIT_EVIDENCE: &str = "ledger.commit.partial_evidence";
    /// Post-commit authority observation disagrees with the posting commitment.
    pub const COMMIT_EVIDENCE_MISMATCH: &str = "ledger.commit.evidence_mismatch";
    /// Idempotency record disagrees with scope, key, input, or stable-result obligations.
    pub const IDEMPOTENCY_RECORD_MISMATCH: &str = "ledger.commit.idempotency_mismatch";
    /// Advancing `expectedOwnerVersion` by one would overflow `u64`.
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
    /// Pack identifier bound into this compiled pack.
    pub pack_id: String,
    /// Checked transaction templates keyed by template identifier.
    pub templates: BTreeMap<String, CompiledLedgerTemplate>,
}

/// One checked template with exact parameter premises and fixed legs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledLedgerTemplate {
    /// Template identifier within the pack.
    pub template_id: String,
    parameters: BTreeMap<String, ParameterModel>,
    /// Normalized template legs in declaration order.
    pub legs: Vec<CompiledLedgerLeg>,
}

/// A leg normalized to its unit's canonical stored precision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledLedgerLeg {
    /// Leg identifier unique within its template.
    pub leg_id: String,
    /// Target posting account identifier.
    pub account_id: String,
    /// Conserved unit identifier.
    pub unit_id: String,
    /// Balance layer identifier.
    pub layer: String,
    /// Canonical stored decimal precision of `unit_id`.
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
///
/// # Errors
///
/// Returns the [`LedgerPackValidation`] report containing all collected
/// diagnostics when the pack fails validation.
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

    validate_pack_header(pack, artifact_id.as_deref(), &mut diagnostics);

    let units = validate_units(pack, artifact_id.as_deref(), &mut diagnostics);
    let accounts = validate_accounts(pack, &units, artifact_id.as_deref(), &mut diagnostics);
    let layers = validate_layers(pack, artifact_id.as_deref(), &mut diagnostics);
    let mut template_ids = BTreeSet::new();
    let mut certificates = Vec::new();
    let mut compiled_templates = BTreeMap::new();

    let ctx = PackCheckContext {
        pack,
        pack_sha256: pack_sha256.as_str(),
        artifact_id: artifact_id.as_deref(),
        units: &units,
        accounts: &accounts,
        layers: &layers,
    };
    for (template_index, template) in pack.templates.iter().enumerate() {
        if diagnostics.truncated {
            break;
        }
        let compiled_template = validate_template(
            &ctx,
            template_index,
            template,
            &mut template_ids,
            &mut certificates,
            &mut diagnostics,
        );
        compiled_templates.insert(template.template_id.clone(), compiled_template);
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

struct PackCheckContext<'a> {
    pack: &'a LedgerPackIr,
    pack_sha256: &'a str,
    artifact_id: Option<&'a str>,
    units: &'a BTreeMap<&'a str, &'a LedgerUnitIr>,
    accounts: &'a BTreeMap<&'a str, BTreeSet<&'a str>>,
    layers: &'a BTreeSet<&'a str>,
}

fn validate_pack_header(
    pack: &LedgerPackIr,
    artifact_id: Option<&str>,
    diagnostics: &mut LedgerDiagnostics,
) {
    if pack.ir_version != LEDGER_PACK_IR_VERSION {
        diagnostics.push(diag(
            diagnostic_codes::UNSUPPORTED_IR_VERSION,
            format!(
                "unsupported ledger pack IR version `{}`; expected `{LEDGER_PACK_IR_VERSION}`",
                pack.ir_version
            ),
            "irVersion",
            artifact_id.map(str::to_owned),
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
    validate_ontology_artifact_pin(pack, artifact_id.map(str::to_owned), diagnostics);
    if pack.units.is_empty() {
        diagnostics.push(diag(
            diagnostic_codes::NO_UNITS,
            "at least one conserved unit is required",
            "units",
            artifact_id.map(str::to_owned),
        ));
    }
    if pack.accounts.is_empty() {
        diagnostics.push(diag(
            diagnostic_codes::NO_ACCOUNTS,
            "at least one posting account is required",
            "accounts",
            artifact_id.map(str::to_owned),
        ));
    }
    if pack.layers.is_empty() {
        diagnostics.push(diag(
            diagnostic_codes::NO_LAYERS,
            "at least one declared balance layer is required",
            "layers",
            artifact_id.map(str::to_owned),
        ));
    }
    if pack.templates.is_empty() {
        diagnostics.push(diag(
            diagnostic_codes::NO_TEMPLATES,
            "at least one transaction template is required",
            "templates",
            artifact_id.map(str::to_owned),
        ));
    }
}

fn validate_template<'a>(
    ctx: &PackCheckContext<'a>,
    template_index: usize,
    template: &'a LedgerTemplateIr,
    template_ids: &mut BTreeSet<&'a str>,
    certificates: &mut Vec<ConservationCertificate>,
    diagnostics: &mut LedgerDiagnostics,
) -> CompiledLedgerTemplate {
    let template_path = format!("templates[{template_index}]");
    validate_identifier(
        template.template_id.as_str(),
        format!("{template_path}.templateId"),
        ctx.artifact_id.map(str::to_owned),
        diagnostics,
    );
    if !template_ids.insert(template.template_id.as_str()) {
        diagnostics.push(diag(
            diagnostic_codes::DUPLICATE_TEMPLATE,
            format!("duplicate template id `{}`", template.template_id),
            format!("{template_path}.templateId"),
            ctx.artifact_id.map(str::to_owned),
        ));
    }
    if template.legs.len() < 2 {
        diagnostics.push(diag(
            diagnostic_codes::TOO_FEW_LEGS,
            "a conserved transaction template requires at least two legs",
            format!("{template_path}.legs"),
            ctx.artifact_id.map(str::to_owned),
        ));
    }

    let parameters = validate_parameters(
        template,
        template_path.as_str(),
        ctx.artifact_id,
        diagnostics,
    );
    let mut acc = TemplateLegAccumulator::default();

    for (leg_index, leg) in template.legs.iter().enumerate() {
        if diagnostics.truncated {
            break;
        }
        let leg_path = format!("{template_path}.legs[{leg_index}]");
        validate_template_leg(
            ctx,
            leg,
            leg_path.as_str(),
            &parameters,
            &mut acc,
            diagnostics,
        );
    }

    finalize_template_residuals(
        ctx,
        template,
        template_path.as_str(),
        acc.residuals,
        certificates,
        diagnostics,
    );

    CompiledLedgerTemplate {
        template_id: template.template_id.clone(),
        parameters,
        legs: acc.compiled_legs,
    }
}

#[derive(Default)]
struct TemplateLegAccumulator<'a> {
    leg_ids: BTreeSet<&'a str>,
    compiled_legs: Vec<CompiledLedgerLeg>,
    residuals: BTreeMap<(String, String), NormalizedLinear>,
}

fn validate_template_leg<'a>(
    ctx: &PackCheckContext<'a>,
    leg: &'a LedgerLegIr,
    leg_path: &str,
    parameters: &BTreeMap<String, ParameterModel>,
    acc: &mut TemplateLegAccumulator<'a>,
    diagnostics: &mut LedgerDiagnostics,
) {
    validate_identifier(
        leg.leg_id.as_str(),
        format!("{leg_path}.legId"),
        ctx.artifact_id.map(str::to_owned),
        diagnostics,
    );
    if !acc.leg_ids.insert(leg.leg_id.as_str()) {
        diagnostics.push(diag(
            diagnostic_codes::DUPLICATE_LEG,
            format!("duplicate leg id `{}`", leg.leg_id),
            format!("{leg_path}.legId"),
            ctx.artifact_id.map(str::to_owned),
        ));
    }
    validate_identifier(
        leg.account_id.as_str(),
        format!("{leg_path}.accountId"),
        ctx.artifact_id.map(str::to_owned),
        diagnostics,
    );
    match ctx.accounts.get(leg.account_id.as_str()) {
        None => diagnostics.push(diag(
            diagnostic_codes::UNKNOWN_ACCOUNT,
            format!("unknown account `{}`", leg.account_id),
            format!("{leg_path}.accountId"),
            ctx.artifact_id.map(str::to_owned),
        )),
        Some(allowed_units) if !allowed_units.contains(leg.unit_id.as_str()) => {
            diagnostics.push(diag(
                diagnostic_codes::ACCOUNT_UNIT_MISMATCH,
                format!(
                    "account `{}` does not admit unit `{}`",
                    leg.account_id, leg.unit_id
                ),
                format!("{leg_path}.unitId"),
                ctx.artifact_id.map(str::to_owned),
            ));
        }
        Some(_) => {}
    }
    if !nonempty(leg.layer.as_str()) {
        diagnostics.push(diag(
            diagnostic_codes::EMPTY_LAYER,
            "layer must be non-empty",
            format!("{leg_path}.layer"),
            ctx.artifact_id.map(str::to_owned),
        ));
    } else if !ctx.layers.contains(leg.layer.as_str()) {
        diagnostics.push(diag(
            diagnostic_codes::UNKNOWN_LAYER,
            format!("unknown layer `{}`", leg.layer),
            format!("{leg_path}.layer"),
            ctx.artifact_id.map(str::to_owned),
        ));
    }

    let Some(unit) = ctx.units.get(leg.unit_id.as_str()) else {
        diagnostics.push(diag(
            diagnostic_codes::UNKNOWN_UNIT,
            format!("unknown unit `{}`", leg.unit_id),
            format!("{leg_path}.unitId"),
            ctx.artifact_id.map(str::to_owned),
        ));
        return;
    };

    let Some(normalized) = normalize_amount(
        &leg.amount,
        unit.precision,
        parameters,
        format!("{leg_path}.amount").as_str(),
        ctx.artifact_id,
        diagnostics,
    ) else {
        return;
    };

    acc.compiled_legs.push(CompiledLedgerLeg {
        leg_id: leg.leg_id.clone(),
        account_id: leg.account_id.clone(),
        unit_id: leg.unit_id.clone(),
        layer: leg.layer.clone(),
        unit_precision: unit.precision,
        amount: normalized.clone(),
    });

    accumulate_leg_residual(
        ctx.artifact_id,
        leg,
        leg_path,
        normalized,
        parameters,
        &mut acc.residuals,
        diagnostics,
    );
}

fn accumulate_leg_residual(
    artifact_id: Option<&str>,
    leg: &LedgerLegIr,
    leg_path: &str,
    normalized: NormalizedLinear,
    parameters: &BTreeMap<String, ParameterModel>,
    residuals: &mut BTreeMap<(String, String), NormalizedLinear>,
    diagnostics: &mut LedgerDiagnostics,
) {
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
            artifact_id.map(str::to_owned),
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
                artifact_id.map(str::to_owned),
            ));
        }
    }
    if prefix_is_representable {
        validate_runtime_prefix_bounds(
            residual,
            parameters,
            leg.leg_id.as_str(),
            format!("{leg_path}.amount").as_str(),
            artifact_id.map(str::to_owned),
            diagnostics,
        );
    }
}

fn finalize_template_residuals(
    ctx: &PackCheckContext<'_>,
    template: &LedgerTemplateIr,
    template_path: &str,
    residuals: BTreeMap<(String, String), NormalizedLinear>,
    certificates: &mut Vec<ConservationCertificate>,
    diagnostics: &mut LedgerDiagnostics,
) {
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
                template_path,
                ctx.artifact_id.map(str::to_owned),
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
                    template_path,
                    ctx.artifact_id.map(str::to_owned),
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
                ir_version: ctx.pack.ir_version.clone(),
                checker_version: LEDGER_PACK_CHECKER_VERSION.to_string(),
                pack_id: ctx.pack.pack_id.clone(),
                pack_sha256: ctx.pack_sha256.to_string(),
                ontology_artifact: ctx.pack.ontology_artifact.clone(),
                template_id: template.template_id.clone(),
                unit_id: unit_id.clone(),
                unit_term_id: ctx
                    .units
                    .get(unit_id.as_str())
                    .map_or_else(String::new, |definition| definition.unit_term_id.clone()),
                quantity_kind_term_id: ctx
                    .units
                    .get(unit_id.as_str())
                    .map_or_else(String::new, |definition| {
                        definition.quantity_kind_term_id.clone()
                    }),
                layer,
                unit_precision: ctx
                    .units
                    .get(unit_id.as_str())
                    .map_or(0, |definition| definition.precision),
                constant_residual_unscaled: "0".to_string(),
                parameter_residuals_unscaled,
            };
            certificate.certificate_sha256 = conservation_certificate_sha256(&certificate);
            certificates.push(certificate);
        }
    }
}

/// Evaluates one compiled template with the same unit-scale coefficients used
/// by the conservation checker. Inputs must use their declared scale and lie
/// within their sealed bounds. Every operation remains checked `i128`.
///
/// # Errors
///
/// Returns diagnostics when `template_id` is unknown, runtime parameters are
/// missing, duplicated, wrongly scaled, or out of bounds, evaluation overflows
/// `i128`, or the evaluated legs do not sum to zero per `(unit, layer)`
/// partition.
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
    let values = collect_runtime_inputs(template, inputs, artifact_id.as_deref(), &mut diagnostics);
    if !diagnostics.is_empty() {
        return Err(diagnostics.entries);
    }

    let evaluated_legs =
        evaluate_compiled_legs(template, &values, artifact_id.as_deref(), &mut diagnostics);
    if !diagnostics.is_empty() {
        return Err(diagnostics.entries);
    }

    Ok(LedgerTemplateEvaluation {
        pack_id: pack.pack_id.clone(),
        template_id: template.template_id.clone(),
        legs: evaluated_legs,
    })
}

fn collect_runtime_inputs(
    template: &CompiledLedgerTemplate,
    inputs: &[LedgerParameterValue],
    artifact_id: Option<&str>,
    diagnostics: &mut LedgerDiagnostics,
) -> BTreeMap<String, i128> {
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
                artifact_id.map(str::to_owned),
            ));
            continue;
        }
        let Some(parameter) = template.parameters.get(input.parameter_id.as_str()) else {
            diagnostics.push(diag(
                diagnostic_codes::UNKNOWN_INPUT,
                format!("unknown runtime parameter `{}`", input.parameter_id),
                format!("{path}.parameterId"),
                artifact_id.map(str::to_owned),
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
                artifact_id.map(str::to_owned),
            ));
            continue;
        }
        let Some(value) = parse_i128(
            input.unscaled.as_str(),
            format!("{path}.unscaled"),
            artifact_id.map(str::to_owned),
            diagnostics,
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
                artifact_id.map(str::to_owned),
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
                artifact_id.map(str::to_owned),
            ));
        }
    }
    values
}

fn evaluate_compiled_legs(
    template: &CompiledLedgerTemplate,
    values: &BTreeMap<String, i128>,
    artifact_id: Option<&str>,
    diagnostics: &mut LedgerDiagnostics,
) -> Vec<EvaluatedLedgerLeg> {
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
                    artifact_id.map(str::to_owned),
                ));
                continue;
            };
            let Some(term_value) = coefficient.checked_mul(input) else {
                diagnostics.push(diag(
                    diagnostic_codes::EVALUATION_OVERFLOW,
                    format!("coefficient multiplication overflowed for parameter `{parameter_id}`"),
                    format!("legs[{leg_index}].amount"),
                    artifact_id.map(str::to_owned),
                ));
                continue;
            };
            let Some(next_amount) = amount.checked_add(term_value) else {
                diagnostics.push(diag(
                    diagnostic_codes::EVALUATION_OVERFLOW,
                    format!("leg `{}` amount overflowed i128", leg.leg_id),
                    format!("legs[{leg_index}].amount"),
                    artifact_id.map(str::to_owned),
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
                artifact_id.map(str::to_owned),
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
                artifact_id.map(str::to_owned),
            ));
        }
    }
    evaluated_legs
}

fn validate_layers<'a>(
    pack: &'a LedgerPackIr,
    artifact_id: Option<&str>,
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
            artifact_id.map(str::to_owned),
            diagnostics,
        );
        if !layers.insert(layer.layer_id.as_str()) {
            diagnostics.push(diag(
                diagnostic_codes::DUPLICATE_LAYER,
                format!("duplicate layer id `{}`", layer.layer_id),
                path,
                artifact_id.map(str::to_owned),
            ));
        }
    }
    layers
}

fn validate_accounts<'a>(
    pack: &'a LedgerPackIr,
    units: &BTreeMap<&str, &LedgerUnitIr>,
    artifact_id: Option<&str>,
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
            artifact_id.map(str::to_owned),
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
                artifact_id.map(str::to_owned),
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
                artifact_id.map(str::to_owned),
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
                    artifact_id.map(str::to_owned),
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
                    artifact_id.map(str::to_owned),
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
                artifact_id.map(str::to_owned),
            ));
        }
    }
    accounts
}

fn validate_units<'a>(
    pack: &'a LedgerPackIr,
    artifact_id: Option<&str>,
    diagnostics: &mut LedgerDiagnostics,
) -> BTreeMap<&'a str, &'a LedgerUnitIr> {
    let mut units = BTreeMap::new();
    let mut unit_terms = BTreeSet::new();
    let mut quantity_kind_terms = BTreeSet::new();
    for (index, unit) in pack.units.iter().enumerate() {
        if diagnostics.truncated {
            break;
        }
        validate_unit_entry(
            index,
            unit,
            artifact_id,
            &mut units,
            &mut unit_terms,
            &mut quantity_kind_terms,
            diagnostics,
        );
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
                artifact_id.map(str::to_owned),
            ));
        }
    }
    units
}

fn validate_unit_entry<'a>(
    index: usize,
    unit: &'a LedgerUnitIr,
    artifact_id: Option<&str>,
    units: &mut BTreeMap<&'a str, &'a LedgerUnitIr>,
    unit_terms: &mut BTreeSet<&'a str>,
    quantity_kind_terms: &mut BTreeSet<&'a str>,
    diagnostics: &mut LedgerDiagnostics,
) {
    let path = format!("units[{index}]");
    validate_identifier(
        unit.unit_id.as_str(),
        format!("{path}.unitId"),
        artifact_id.map(str::to_owned),
        diagnostics,
    );
    if !nonempty(unit.unit_term_id.as_str()) {
        diagnostics.push(diag(
            diagnostic_codes::EMPTY_UNIT_TERM,
            "unitTermId must be non-empty and canonical",
            format!("{path}.unitTermId"),
            artifact_id.map(str::to_owned),
        ));
    } else if !is_exact_ontology_identity(unit.unit_term_id.as_str()) {
        diagnostics.push(diag(
            diagnostic_codes::INVALID_UNIT_TERM,
            "unitTermId must be one exact term identity without whitespace or wildcards",
            format!("{path}.unitTermId"),
            artifact_id.map(str::to_owned),
        ));
    } else if !unit_terms.insert(unit.unit_term_id.as_str()) {
        diagnostics.push(diag(
            diagnostic_codes::DUPLICATE_UNIT_TERM,
            format!(
                "ontology unit term `{}` is bound by more than one pack-local unit",
                unit.unit_term_id
            ),
            format!("{path}.unitTermId"),
            artifact_id.map(str::to_owned),
        ));
    }
    if !nonempty(unit.quantity_kind_term_id.as_str()) {
        diagnostics.push(diag(
            diagnostic_codes::EMPTY_QUANTITY_KIND_TERM,
            "quantityKindTermId must be non-empty and canonical",
            format!("{path}.quantityKindTermId"),
            artifact_id.map(str::to_owned),
        ));
    } else if !is_exact_ontology_identity(unit.quantity_kind_term_id.as_str()) {
        diagnostics.push(diag(
            diagnostic_codes::INVALID_QUANTITY_KIND_TERM,
            "quantityKindTermId must be one exact term identity without whitespace or wildcards",
            format!("{path}.quantityKindTermId"),
            artifact_id.map(str::to_owned),
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
            artifact_id.map(str::to_owned),
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
            artifact_id.map(str::to_owned),
        ));
    }
    if units.insert(unit.unit_id.as_str(), unit).is_some() {
        diagnostics.push(diag(
            diagnostic_codes::DUPLICATE_UNIT,
            format!("duplicate unit id `{}`", unit.unit_id),
            format!("{path}.unitId"),
            artifact_id.map(str::to_owned),
        ));
    }
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
    artifact_id: Option<&str>,
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
            artifact_id.map(str::to_owned),
            diagnostics,
        );
        if parameters.contains_key(parameter.parameter_id.as_str()) {
            diagnostics.push(diag(
                diagnostic_codes::DUPLICATE_PARAMETER,
                format!("duplicate parameter id `{}`", parameter.parameter_id),
                format!("{path}.parameterId"),
                artifact_id.map(str::to_owned),
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
                artifact_id.map(str::to_owned),
            ));
            continue;
        }
        let Some(min_unscaled) = parse_i128(
            parameter.min_unscaled.as_str(),
            format!("{path}.minUnscaled"),
            artifact_id.map(str::to_owned),
            diagnostics,
        ) else {
            continue;
        };
        let Some(max_unscaled) = parse_i128(
            parameter.max_unscaled.as_str(),
            format!("{path}.maxUnscaled"),
            artifact_id.map(str::to_owned),
            diagnostics,
        ) else {
            continue;
        };
        if min_unscaled > max_unscaled {
            diagnostics.push(diag(
                diagnostic_codes::INVALID_BOUNDS,
                "minUnscaled must be less than or equal to maxUnscaled",
                path,
                artifact_id.map(str::to_owned),
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
    artifact_id: Option<&str>,
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
            artifact_id.map(str::to_owned),
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
            artifact_id.map(str::to_owned),
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
            artifact_id.map(str::to_owned),
        ));
    }

    let constant = parse_i128(
        amount.constant_unscaled.as_str(),
        format!("{path}.constantUnscaled"),
        artifact_id.map(str::to_owned),
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
            artifact_id.map(str::to_owned),
        ));
    }

    let coefficients = normalize_amount_terms(
        amount,
        unit_multiplier,
        parameters,
        path,
        artifact_id,
        diagnostics,
    );
    let constant_for_bounds = constant_unit?;
    validate_normalized_bounds(
        constant_for_bounds,
        &coefficients,
        parameters,
        path,
        artifact_id,
        diagnostics,
    );

    if diagnostics.len() != initial_diagnostic_count {
        return None;
    }
    constant_unit.map(|constant| NormalizedLinear {
        constant,
        coefficients,
    })
}

fn normalize_amount_terms(
    amount: &LinearAmountIr,
    unit_multiplier: Option<i128>,
    parameters: &BTreeMap<String, ParameterModel>,
    path: &str,
    artifact_id: Option<&str>,
    diagnostics: &mut LedgerDiagnostics,
) -> BTreeMap<String, i128> {
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
            artifact_id.map(str::to_owned),
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
                artifact_id.map(str::to_owned),
            ));
            continue;
        }
        let Some(parameter) = parameters.get(term.parameter_id.as_str()) else {
            diagnostics.push(diag(
                diagnostic_codes::UNKNOWN_PARAMETER,
                format!("unknown or invalid parameter `{}`", term.parameter_id),
                format!("{term_path}.parameterId"),
                artifact_id.map(str::to_owned),
            ));
            continue;
        };
        let coefficient = parse_i128(
            term.coefficient_unscaled.as_str(),
            format!("{term_path}.coefficientUnscaled"),
            artifact_id.map(str::to_owned),
            diagnostics,
        );
        let Some(source_scale) = parameter.scale.checked_add(term.coefficient_scale) else {
            diagnostics.push(diag(
                diagnostic_codes::TERM_SCALE_EXCEEDS_RESULT,
                "parameter and coefficient scales overflow their representation",
                format!("{term_path}.coefficientScale"),
                artifact_id.map(str::to_owned),
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
                artifact_id.map(str::to_owned),
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
                artifact_id.map(str::to_owned),
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
                artifact_id.map(str::to_owned),
            ));
            continue;
        };

        coefficients.insert(term.parameter_id.clone(), factor_unit);
    }

    coefficients
}

fn validate_normalized_bounds(
    constant_for_bounds: i128,
    coefficients: &BTreeMap<String, i128>,
    parameters: &BTreeMap<String, ParameterModel>,
    path: &str,
    artifact_id: Option<&str>,
    diagnostics: &mut LedgerDiagnostics,
) {
    // Runtime iterates this BTreeMap in parameter-id order. Prove bounds in
    // exactly that order; declaration-order analysis would be unsound because
    // checked addition can overflow in one order even when the final sum and a
    // different order fit i128.
    let mut runtime_range_min = Some(constant_for_bounds);
    let mut runtime_range_max = Some(constant_for_bounds);
    for (parameter_id, factor_unit) in coefficients {
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
                artifact_id.map(str::to_owned),
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
                artifact_id.map(str::to_owned),
            ));
        }
    }
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
