//! OBS-EVIDENCE-1: only cell-owned recomputation can authorize mapped observations.
use crate::{CellError, SqliteCell, StoreReader, engine::SqlEngine, mapping_bundle::MappingBundle};
use norn_ir::{RuntimeObject, RuntimeValue};
use norn_kernel::snapshot_observe::{ObservedSnapshot, prepare_observed_snapshot};
use norn_kernel::{
    CellCommit, DecisionTurn, FactKey, ObservationAuthorization, ObservationEvent, Observed,
    TypeCatalog, World,
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
        if observation_id.starts_with("mapping-conflict:")
            || observation_id.starts_with("mapping-identity:")
        {
            return Err(CellError::UnsupportedMappingProfile);
        }
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
        if self.store().read().mapping_conflict(&bundle)?.is_some()
            || self.store().read().mapping_event_is_disputed(&bundle)?
        {
            return Err(CellError::UnsupportedMappingProfile);
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
        for record in &bundle.records {
            if !record.links.is_empty()
                || !record.cleared_links.is_empty()
                || record.invocations.len() != 1
            {
                return Err(CellError::UnsupportedMappingProfile);
            }
            let key = FactKey::new(record.object_type.as_str(), &record.key);
            let slice = self.store().read().mapping_observation_slice(&key)?;
            let mut values = record
                .invocations
                .first()
                .ok_or(CellError::MappingBundleMismatch)?
                .input
                .clone();
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
        self.commit_observed_evidence(commit, &authorization, Some(&bundle))
            .map_err(|e| CellError::ObservationCommit(Box::new(e)))?;
        Ok(true)
    }
}

impl<E: SqlEngine> StoreReader<'_, E> {
    // OBS-EVIDENCE-1: cell-owned observation preparation retains raw versions in quarantine.
    fn mapping_observation_slice(
        &self,
        key: &FactKey,
    ) -> Result<norn_kernel::FactSlice, CellError> {
        let state = norn_kernel::FactState {
            version: norn_kernel::FactVersion(self.version(
                crate::MAIN_WORLD,
                key.object_type.as_str(),
                &key.key,
            )?),
            values: self
                .object(crate::MAIN_WORLD, key.object_type.as_str(), &key.key)?
                .map(|r| r.values),
        };
        Ok(norn_kernel::FactSlice::from([(key.clone(), state)]))
    }
}
