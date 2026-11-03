//! Receipt/source correspondence for retained definition documents.
use crate::{refusal::Refusal, scope::CellScope};
use norn_checker::{
    EntityArtifactSetContext, ReceiptSources, check_artifact_set_with_receipt,
    prepare_artifact_source,
};
use norn_ir::{ResourceLimitProfile, RuntimeValue};
use std::collections::BTreeSet;

pub(crate) struct Admitted {
    pub(crate) diagnostics: serde_json::Value,
    pub(crate) receipt_sha256: String,
}
impl Admitted {
    pub(crate) fn from_source(source: &str, scope: &CellScope) -> Result<Self, Refusal> {
        let RuntimeValue::Array(sources) =
            RuntimeValue::from_json_text(source).map_err(Refusal::host)?
        else {
            return Err(Refusal::host(
                "retained definitions must be an artifact array",
            ));
        };
        let mut exact = Vec::new();
        for source in sources {
            let prepared = prepare_artifact_source(source)
                .map_err(|_error| Refusal::host("invalid retained artifact"))?;
            if norn_checker::scope_conflict(&scope.as_scope_ref(), &prepared.artifact.scope)
                .is_some()
            {
                return Err(Refusal::host("retained artifact outside cell scope"));
            }
            exact.push(prepared.source);
        }
        let registry = BTreeSet::new();
        let context = EntityArtifactSetContext {
            registry: &registry,
            base: &[],
            tenant: None,
        };
        let sources = ReceiptSources::new(&exact, &[]).map_err(Refusal::host)?;
        let checked = check_artifact_set_with_receipt(
            &sources,
            &context,
            &ResourceLimitProfile::norn_limits_v1(),
        )
        .map_err(Refusal::host)?;
        if !checked.receipt.admitted {
            return Err(Refusal::host("retained definitions no longer admitted"));
        }
        Ok(Self {
            diagnostics: norn_checker::diagnostics_to_json(&checked.diagnostics),
            receipt_sha256: checked.receipt.content_sha256().map_err(Refusal::host)?,
        })
    }
}
