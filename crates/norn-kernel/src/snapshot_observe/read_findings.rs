//! Read-bound findings an observation authorizes: an unchanged object's
//! data-quality findings, bound to the compare-and-swap read that guards it.

use norn_ir::mapping::ObservedLifecycleSnapshot;
use norn_ir::provenance::InstanceProvenance;
use norn_ir::{DataQualityFinding, DataQualityRuleRef};

use super::{ObservationAuthorization, ObservationRefusal, findings_for};
use crate::cell_host::{FactExpectation, FactKey, FactState, FactVersion, JsonObject, JsonValue};
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

/// When creating a new object whose lifecycle property declares no default value,
/// seed that property from the observed lifecycle snapshot if the observed state
/// is itself a declared initial state of the effective lifecycle.
pub(super) fn seed_initial_lifecycle_states(
    catalog: &TypeCatalog<'_>,
    key: &FactKey,
    original: &FactState,
    values: &JsonObject,
    lifecycles: &[ObservedLifecycleSnapshot],
) -> Option<JsonObject> {
    if original.values.is_some() || lifecycles.is_empty() {
        return None;
    }
    let effective = catalog.effective_type(&key.object_type)?;
    let mut seeded: Option<JsonObject> = None;
    for snapshot in lifecycles {
        let has_default = effective
            .property(snapshot.property.as_str())
            .is_some_and(|prop| prop.default_value().ok().flatten().is_some());
        if has_default {
            continue;
        }
        let Some(lifecycle) = catalog.lifecycle(&key.object_type, &snapshot.property) else {
            continue;
        };
        if lifecycle
            .initial
            .iter()
            .any(|state| state == &snapshot.state)
        {
            seeded.get_or_insert_with(|| values.clone()).insert(
                snapshot.property.as_str().to_owned(),
                JsonValue::String(snapshot.state.clone()),
            );
        }
    }
    seeded
}
