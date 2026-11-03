//! OBS-EVIDENCE-1: only cell-owned recomputation can authorize mapped observations.
use crate::{CellError, SqliteCell, engine::SqlEngine, mapping_bundle::MappingBundle};
use norn_ir::{RuntimeObject, RuntimeValue};
use norn_kernel::snapshot_observe::{ObservedSnapshot, prepare_observed_snapshot};
use norn_kernel::{
    CellCommit, CellHost, DecisionTurn, FactKey, ObservationAuthorization, ObservationEvent,
    Observed, TypeCatalog, World, load_slice,
};

impl<S: SqlEngine> SqliteCell<ObservationEvent, RuntimeObject, RuntimeObject, S> {
    /// Recompute a retained bundle and atomically consume its mapped observations.
    /// The inbox identity selects evidence; no caller supplies facts or authority.
    ///
    /// # Errors
    /// Changed pins/outputs refuse `core.boundary.bundle_mismatch`. Relation and
    /// delete processing are outside this CP2 property/lifecycle profile.
    pub fn process_mapping_bundle(&mut self, observation_id: &str) -> Result<bool, CellError> {
        let Some(pending) = self
            .store()
            .read()
            .pending_observations()?
            .into_iter()
            .find(|p| p.observation_id == observation_id)
        else {
            return Ok(false);
        };
        let observed: Observed<RuntimeValue> =
            norn_ir::runtime::decode(&pending.observed).map_err(CellError::Encoding)?;
        if observed.id.0 != observation_id {
            return Err(CellError::MappingBundleMismatch);
        }
        let bundle: MappingBundle =
            norn_ir::runtime::decode(&observed.input).map_err(CellError::Encoding)?;
        self.store().read().recheck_mapping_bundle(&bundle)?;
        if self
            .store()
            .read()
            .installed_definitions(None)?
            .map(|d| d.version)
            != Some(bundle.definitions_version)
            || observation_id != format!("mapping-bundle:{}", bundle.digest)
        {
            return Err(CellError::MappingBundleMismatch);
        }
        let model = self.store().model().clone();
        let catalog = TypeCatalog::new(model.object_types())
            .with_interfaces(model.interfaces())
            .with_property_types(model.property_types())
            .with_governing_laws(model.law_artifacts());
        let turn = DecisionTurn {
            id: observation_id.to_owned(),
            now: self.now(),
            world: World::Main,
            consumes: Some(observed.consumed()),
        };
        let mut commit = CellCommit::empty().consuming(observed.consumed());
        let mut authorization = ObservationAuthorization::empty();
        if bundle.raw.get("kind").and_then(RuntimeValue::as_str) != Some("upsert")
            || !bundle.diagnostics.is_empty()
        {
            return Err(CellError::UnsupportedMappingProfile);
        }
        for record in bundle.records {
            if !record.links.is_empty()
                || !record.cleared_links.is_empty()
                || record.invocations.len() != 1
            {
                return Err(CellError::UnsupportedMappingProfile);
            }
            let key = FactKey::new(record.object_type.as_str(), &record.key);
            let slice = load_slice(self, [key.clone()])?;
            let mut values = record
                .invocations
                .into_iter()
                .next()
                .ok_or(CellError::MappingBundleMismatch)?
                .input;
            values.remove(norn_checker::mapping::OBJECT_ID_FIELD);
            let prepared = prepare_observed_snapshot(
                &catalog,
                &slice,
                &turn,
                ObservedSnapshot {
                    key: &key,
                    values: &values,
                    lifecycles: &record.lifecycle_snapshots,
                    provenance: &record.provenance,
                },
            )
            .map_err(|e| CellError::ObservationPreparation(Box::new(e)))?;
            let (part, authority) = prepared.into_parts();
            commit.reads.extend(part.reads);
            commit.writes.extend(part.writes);
            commit.events.extend(part.events);
            commit.effects.extend(part.effects);
            commit.links.extend(part.links);
            commit.link_reads.extend(part.link_reads);
            authorization.merge(authority);
        }
        authorization
            .seal_snapshot_event_chunks(&commit)
            .map_err(|e| CellError::ObservationPreparation(Box::new(e)))?;
        self.commit_observed(commit, authorization)
            .map_err(|e| CellError::ObservationCommit(Box::new(e)))?;
        Ok(true)
    }
}
