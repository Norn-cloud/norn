//! Read-bound findings an observation authorizes: an unchanged object's
//! data-quality findings, bound to the compare-and-swap read that guards it.

use norn_ir::provenance::InstanceProvenance;
use norn_ir::{DataQualityFinding, DataQualityRuleRef};

use super::{ObservationAuthorization, ObservationRefusal, findings_for};
use crate::cell_host::{FactExpectation, FactKey, FactVersion, JsonObject};
use crate::commit_check::InvariantFailure;
use crate::decision_commit::TypeCatalog;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AuthorizedObservedReadFinding {
    /// The object, by the concrete type its laws and evidence use.
    pub(crate) key: FactKey,
    /// The spelling of `key` the commit's compare-and-swap read uses: a
    /// hierarchy alias of it when another read already pins the object
    /// (one canonical object, one read; #1701).
    pub(crate) read_key: FactKey,
    pub(crate) expected: FactVersion,
    pub(crate) values: JsonObject,
    pub(crate) provenance: InstanceProvenance,
    pub(crate) observation_id: String,
    pub(crate) findings: Vec<DataQualityFinding>,
    /// LAW-LINK-1: a linked subject re-checked because its target changed
    /// files findings only for the link laws the change broke.
    pub(crate) laws: Option<Vec<DataQualityRuleRef>>,
}

impl ObservationAuthorization {
    pub(super) fn authorize_read_findings(
        &mut self,
        catalog: &TypeCatalog<'_>,
        expectation: &FactExpectation,
        values: &JsonObject,
        provenance: &InstanceProvenance,
        observation_id: &str,
        failures: &[InvariantFailure],
    ) -> Result<(), ObservationRefusal> {
        let findings = findings_for(
            catalog,
            &expectation.key,
            values,
            provenance,
            observation_id,
            failures,
            &mut crate::governing_laws::DeferLinkLaws,
        )?;
        // Claim authority retains the checked values even when no finding is emitted.
        if !findings.is_empty()
            || expectation.key.object_type.as_str() == norn_ir::mapping::OBSERVED_LINK_CLAIM_TYPE
        {
            self.read_findings.push(AuthorizedObservedReadFinding {
                key: expectation.key.clone(),
                read_key: expectation.key.clone(),
                expected: expectation.expected,
                values: values.clone(),
                provenance: provenance.clone(),
                observation_id: observation_id.to_owned(),
                findings,
                laws: None,
            });
        }
        Ok(())
    }
}
