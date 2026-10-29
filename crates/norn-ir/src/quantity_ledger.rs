//! Conserved-quantity ledger pack IR types.
//!
//! The pure seal-time checker, evaluator, and posting boundary that consume
//! these types live in `norn_checker::quantity_ledger` (T1-CHK-1).
//!
//! This module is intentionally independent of CALA, Turso, CEL, and the
//! runtime engine. The closed authoring-expression module lowers a deliberately
//! small affine language into [`LinearAmountIr`]. The seal checker then
//! validates that exact scaled-integer representation and checks that every
//! template has a zero constant and zero coefficient for every `(unit, layer)`
//! partition. It is executable code and testable evidence, but it is not a
//! Lean or other proof-assistant correspondence claim.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::Diagnostic;

mod boundary;
mod cell;
pub use cell::{
    QUANTITY_CELL_VERSION, QuantityBalanceVersion, QuantityOperation, QuantityRequest,
    QuantityResolution, QuantitySlice,
};
mod expression;
pub use boundary::{
    QuantityAuthorityCommitObservation, QuantityCommitStatus, QuantityIdempotencyRecord,
    QuantityOutboxRecord, QuantityPostingCommitment, QuantityPostingIntent, QuantityReplayDecision,
};
pub use expression::{
    LEDGER_AMOUNT_EXPRESSION_VERSION, LedgerAmountExpressionIr, LedgerAmountExpressionNode,
    LedgerAmountLoweringError, LedgerAmountLoweringErrorCode,
};

/// Wire version of the narrow v1 quantity-ledger checker input.
pub const LEDGER_PACK_IR_VERSION: &str = "1.2";

/// Version of the executable normalization and conservation algorithm.
pub const LEDGER_PACK_CHECKER_VERSION: &str = "0.3.0";

/// Wire version of a conservation certificate and its hash preimage.
pub const CONSERVATION_CERTIFICATE_VERSION: &str = "1.1";

/// Wire version of the proof-to-authority posting boundary.
pub const QUANTITY_POSTING_CONTRACT_VERSION: &str = "1.1";

/// Highest scale admitted by the downstream decimal boundary.
pub const MAX_LEDGER_SCALE: u8 = 28;

/// Minimal declarative input required to check quantity conservation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LedgerPackIr {
    pub ir_version: String,
    pub pack_id: String,
    /// Exact promoted ontology artifact whose term identities this pack uses.
    pub ontology_artifact: LedgerOntologyArtifactPin,
    pub units: Vec<LedgerUnitIr>,
    pub accounts: Vec<LedgerAccountIr>,
    pub layers: Vec<LedgerLayerIr>,
    pub templates: Vec<LedgerTemplateIr>,
}

/// Exact immutable ontology artifact identity admitted by this ledger pack.
///
/// The ledger checker does not interpret OWL, follow mappings, or infer a
/// conversion. An outer promotion boundary must establish that this pin names
/// an admitted ontology artifact; this checker only makes the pin mandatory
/// and binds it into every downstream digest and certificate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LedgerOntologyArtifactPin {
    pub artifact_id: String,
    pub artifact_version: String,
    pub artifact_sha256: String,
}

/// One pack-local conserved base unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LedgerUnitIr {
    pub unit_id: String,
    /// Exact ontology term identifying the representation unit.
    pub unit_term_id: String,
    /// Exact ontology term identifying the conserved quantity kind/dimension.
    pub quantity_kind_term_id: String,
    /// Number of base-10 fractional digits in the canonical stored amount.
    pub precision: u8,
}

/// One pack-local posting account and the open unit identifiers it admits.
///
/// Accounts do not encode monetary debit/credit assumptions. The same closed
/// machinery can conserve currency, mass, energy, inventory, carbon, or any
/// other declared unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LedgerAccountIr {
    pub account_id: String,
    pub allowed_unit_ids: Vec<String>,
    /// Explicit funding semantics; omitted authoring values mean gross.
    #[serde(default)]
    pub funding: LedgerFundingPolicy,
}

/// Closed, checker-admitted funding semantics within one conserved partition.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LedgerFundingPolicy {
    /// Every outgoing leg must be independently funded before this vector.
    #[default]
    Gross,
    /// Only this vector's incoming legs in the same partition may fund outgoing.
    Net,
}

/// One closed pack-local balance layer, such as posted or pending.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LedgerLayerIr {
    pub layer_id: String,
}

/// A fixed-arity transaction template and its typed parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LedgerTemplateIr {
    pub template_id: String,
    pub parameters: Vec<LedgerParameterIr>,
    pub legs: Vec<LedgerLegIr>,
}

/// Declared exact input range. Bounds are canonical signed decimal strings
/// containing the unscaled integer at `scale`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LedgerParameterIr {
    pub parameter_id: String,
    pub scale: u8,
    pub min_unscaled: String,
    pub max_unscaled: String,
}

/// One signed leg. Positive and negative coefficients encode the two sides of
/// a conservation equation; no implicit debit/credit sign is introduced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LedgerLegIr {
    pub leg_id: String,
    pub account_id: String,
    pub unit_id: String,
    pub layer: String,
    pub amount: LinearAmountIr,
}

/// A normalized-source linear expression before scale alignment.
///
/// Its real value is `constant + sum(coefficient_i * parameter_i)`. Each
/// integer field is encoded as a canonical decimal string and interpreted at
/// its adjacent scale. Only exact power-of-ten upscaling is admitted in v1;
/// division and rounding never occur.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinearAmountIr {
    pub result_scale: u8,
    pub constant_unscaled: String,
    pub constant_scale: u8,
    pub terms: Vec<LinearTermIr>,
}

/// One exact linear coefficient applied to a declared template parameter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinearTermIr {
    pub parameter_id: String,
    pub coefficient_unscaled: String,
    pub coefficient_scale: u8,
}

/// Auditable zero-residual evidence produced for one template partition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConservationCertificate {
    pub certificate_version: String,
    pub certificate_sha256: String,
    pub ir_version: String,
    pub checker_version: String,
    pub pack_id: String,
    pub pack_sha256: String,
    pub ontology_artifact: LedgerOntologyArtifactPin,
    pub template_id: String,
    pub unit_id: String,
    pub unit_term_id: String,
    pub quantity_kind_term_id: String,
    pub layer: String,
    pub unit_precision: u8,
    pub constant_residual_unscaled: String,
    pub parameter_residuals_unscaled: BTreeMap<String, String>,
}

/// Complete deterministic checker result. Certificates are emitted only when
/// the entire pack validates; partial certificates cannot activate a pack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LedgerPackValidation {
    pub ok: bool,
    pub diagnostics: Vec<Diagnostic>,
    pub certificates: Vec<ConservationCertificate>,
}

/// A runtime parameter at an explicit exact scale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LedgerParameterValue {
    pub parameter_id: String,
    pub scale: u8,
    pub unscaled: String,
}

/// Exact evaluated leg amount at the unit's canonical precision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvaluatedLedgerLeg {
    pub leg_id: String,
    pub account_id: String,
    pub unit_id: String,
    pub layer: String,
    pub unit_precision: u8,
    pub amount_unscaled: String,
}

/// Runtime evaluation output after bounds, scale, arithmetic, and conservation
/// checks have all succeeded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LedgerTemplateEvaluation {
    pub pack_id: String,
    pub template_id: String,
    pub legs: Vec<EvaluatedLedgerLeg>,
}
