//! LAW-WORDING-PIN-1: display slots bind to an exact law without changing its semantics.
use crate::{ArtifactRef, deontic::check_requirement};
use norn_ir::{ArtifactEnvelope, ArtifactKind, LawIR, deontic::StructuredRequirement, requirement_display::{RequirementLocale, RequirementView}};
use serde::{Deserialize, Serialize};

/// Compiler-produced display data, admitted separately from executable artifacts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LawWordingArtifact {
    /// Closed sidecar format.
    pub format: String,
    /// Stable law ID plus `.wording`.
    pub artifact_id: String,
    /// Exact referenced law artifact identity.
    pub law_artifact_id: String,
    /// Norn1 semantic-envelope digest issued by the checker.
    pub law_sha256: String,
    /// Existing structured requirement, including the complete law and EN/AR slots.
    pub requirement: StructuredRequirement,
}

/// Binding is structural evidence, independent of proof or translation approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WordingBinding {
    /// Exact semantic envelope and structured requirement agree.
    Bound,
    /// No binding to the current admitted law is established.
    Unverified,
}

/// A display projection from #322 and its independent binding status.
#[derive(Debug)]
pub struct AdmittedWording {
    /// Stale/mismatched law bindings are unverified.
    pub binding: WordingBinding,
    /// Read-only locale projection; consumers render slots as text.
    pub view: RequirementView,
}

/// Refusal to admit malformed display data; this never changes a law.
#[derive(Debug, thiserror::Error)]
pub enum WordingError {
    /// The sidecar format, identity or pin is malformed.
    #[error("law.wording.invalid_document")]
    Document,
    /// The shared display profile refuses the structured requirement.
    #[error("law.wording.render: {0}")]
    Render(&'static str),
    /// The shared structured-requirement checker refuses its slots.
    #[error("law.wording.requirement: {0:?}")]
    Requirement(crate::deontic::RequirementRefusal),
    /// Canonical hashing could not establish the referenced identity.
    #[error("law.wording.pin: {0:?}")]
    Pin(crate::ReceiptError),
    /// The admitted artifact payload cannot decode as a law.
    #[error("law.wording.encoding: {0}")]
    Encoding(#[from] serde_json::Error),
}

/// LAW-WORDING-PIN-1: stale wording never inherits a current law's assurance.
/// `law` must already have passed ordinary ontology admission in the caller's scope.
///
/// # Errors
/// Invalid format, malformed pins, unsupported display, or an ill-typed bound requirement.
pub fn admit_wording(wording: &LawWordingArtifact, law: &ArtifactEnvelope, env: &norn_expr::ZenTypeEnv, locale: RequirementLocale) -> Result<AdmittedWording, WordingError> {
    if wording.format != "norn.law_wording/1"
        || wording.artifact_id != format!("{}.wording", wording.requirement.norm.law.id)
        || wording.law_sha256.len() != 64
        || !wording.law_sha256.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return Err(WordingError::Document);
    }
    let view = RequirementView { requirement: wording.requirement.clone(), locale };
    view.slots().map_err(WordingError::Render)?;
    let pin = ArtifactRef::of(law).map_err(WordingError::Pin)?;
    let current: Option<LawIR> = if law.kind() == ArtifactKind::Law {
        Some(serde_json::from_str(&serde_json::to_string(&law.payload.to_json_value())?)?)
    } else { None };
    let bound = wording.law_artifact_id == pin.artifact_id && wording.law_sha256 == pin.sha256
        && current.as_ref() == Some(&wording.requirement.norm.law);
    if bound { check_requirement(&wording.requirement, env).map_err(WordingError::Requirement)?; }
    Ok(AdmittedWording { binding: if bound { WordingBinding::Bound } else { WordingBinding::Unverified }, view })
}
