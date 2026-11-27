//! The ring a retained receipt is re-checked under (#1762 C2): a set that holds a widget is
//! re-checked by the Ring-1 check that admitted it, never by Ring-0 alone.

use norn_ir::{ArtifactEnvelope, ResourceLimitProfile};

use super::{
    CheckedArtifactSet, ReceiptCheckRequest, ReceiptError, ReceiptSources, ReceiptVersion,
    check_artifact_set_with_profile, invalid_receipt,
};
use crate::Diagnostic;
use crate::entity_artifacts::EntityArtifactSetContext;

/// The set check a receipt is issued under: Ring-0 [`crate::check_artifact_set`], or a ring
/// that runs it and checks more (Ring-1 `norn-ui-checker` adds the widget laws).
pub type SetCheck = fn(&[ArtifactEnvelope], &EntityArtifactSetContext<'_>) -> Vec<Diagnostic>;

/// [`super::check_artifact_set_with_verified_receipt`], the set checked by `check`: a retained
/// set that holds a widget is re-checked by the ring that admitted it, so its laws still hold,
/// and a set Ring-0 alone cannot judge is never judged by it (`widget.validation.ring1_required`).
///
/// # Errors
/// As [`super::check_artifact_set_with_verified_receipt`].
pub fn check_artifact_set_with_verified_receipt_by(
    artifacts: &ReceiptSources,
    context: &EntityArtifactSetContext<'_>,
    limits: &ResourceLimitProfile,
    version: &ReceiptVersion,
    check: SetCheck,
) -> Result<CheckedArtifactSet, ReceiptError> {
    if version.semantic_artifact_set != artifacts.identity {
        return Err(invalid_receipt(
            "verified receipt profile does not bind the supplied artifact set",
        ));
    }
    let context = artifacts.validated_context(context)?;
    check_artifact_set_with_profile(ReceiptCheckRequest {
        artifacts: &artifacts.artifacts,
        context: &context,
        limits,
        artifact_set: artifacts.identity.clone(),
        identifier_profile: &version.identifier_profile,
        flow_source_map: version.flow_source_map.as_ref(),
        retained: true,
        check,
    })
}
