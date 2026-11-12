//! OBS-EVIDENCE-1: only cell-owned recomputation can authorize mapped observations.
use crate::{CellError, SqliteCell, StoreReader, engine::SqlEngine, mapping_bundle::MappingBundle};
use norn_ir::{RuntimeObject, RuntimeValue};
use norn_kernel::snapshot_observe::{ObservedSnapshot, prepare_observed_snapshot};
use norn_kernel::{
    CellCommit, DecisionTurn, FactKey, ObservationAuthorization, ObservationEvent, Observed,
    TypeCatalog,
};

impl<S: SqlEngine> SqliteCell<ObservationEvent, RuntimeObject, RuntimeObject, S> {
    /// Recompute a retained bundle and atomically consume its mapped observations.
    /// The inbox identity selects evidence; no caller supplies facts or authority.
    ///
    /// # Errors
    /// Changed pins/outputs refuse `core.boundary.bundle_mismatch`. Source deletes
    /// remain outside this mapping-bundle profile.
    pub fn process_mapping_bundle(&mut self, observation_id: &str) -> Result<bool, CellError> {
        let Some(pending) = self.store().read().pending_observation(observation_id)? else {
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
            world: self.store().world(),
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
            if record.invocations.len() != 1 {
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
            prepared
                .append_to(&mut commit, &mut authorization)
                .map_err(|e| CellError::ObservationPreparation(Box::new(e)))?;
        }
        self.store().read().prepare_mapping_links(
            &bundle,
            &catalog,
            &turn,
            &mut commit,
            &mut authorization,
        )?;
        // LAW-LINK-1: laws that read a link are evaluated once the bundle's
        // links are known, over the state this commit leaves; a changed target
        // re-checks the subjects that link to it.
        authorization
            .refresh_link_findings(
                &catalog,
                &mut commit,
                &crate::host::GuardView(&self.store().read()),
            )
            .map_err(|e| CellError::ObservationPreparation(Box::new(e)))?;
        authorization
            .seal_snapshot_event_chunks(&commit)
            .map_err(|e| CellError::ObservationPreparation(Box::new(e)))?;
        self.commit_observed_evidence(commit, &authorization, Some(&bundle))
            .map_err(|e| CellError::ObservationCommit(Box::new(e)))?;
        Ok(true)
    }
}

/// What processing one captured source row did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MappingOutcome {
    /// Its canonical objects were committed now.
    Landed,
    /// It was already consumed (the same source event, captured before).
    AlreadyLanded,
    /// It stays in the inbox as quarantined evidence, for these reasons
    /// (mapping diagnostic codes, or the cell's refusal code).
    Quarantined(Vec<String>),
}

impl<S: SqlEngine> SqliteCell<ObservationEvent, RuntimeObject, RuntimeObject, S> {
    /// [`Self::process_mapping_bundle`] for a sync: a bundle the mapping or the
    /// cell refuses is not an error but quarantined evidence, left pending in
    /// the inbox with the reasons it was refused.
    ///
    /// # Errors
    /// Store failures only.
    pub fn process_mapping_outcome(
        &mut self,
        observation_id: &str,
    ) -> Result<MappingOutcome, CellError> {
        if observation_id.starts_with("mapping-conflict:")
            || observation_id.starts_with("mapping-identity:")
        {
            let pending = self.store().read().pending_observation(observation_id)?;
            return Ok(if pending.is_some() {
                MappingOutcome::Quarantined(vec!["core.boundary.import_finding".into()])
            } else {
                MappingOutcome::AlreadyLanded
            });
        }
        let Some(pending) = self.store().read().pending_observation(observation_id)? else {
            return Ok(MappingOutcome::AlreadyLanded);
        };
        let observed: Observed<RuntimeValue> =
            norn_ir::runtime::decode(&pending.observed).map_err(CellError::Encoding)?;
        let bundle: MappingBundle =
            norn_ir::runtime::decode(&observed.input).map_err(CellError::Encoding)?;
        if !bundle.diagnostics.is_empty() {
            let mut codes: Vec<String> = bundle
                .diagnostics
                .iter()
                .map(|d| {
                    d.get("code")
                        .and_then(RuntimeValue::as_str)
                        .unwrap_or("mapping.refused")
                        .to_owned()
                })
                .collect();
            codes.sort();
            codes.dedup();
            return Ok(MappingOutcome::Quarantined(codes));
        }
        match self.process_mapping_bundle(observation_id) {
            Ok(true) => Ok(MappingOutcome::Landed),
            Ok(false) => Ok(MappingOutcome::AlreadyLanded),
            Err(error @ (CellError::Sqlite(_) | CellError::Engine(_))) => Err(error),
            Err(error) => Ok(MappingOutcome::Quarantined(vec![refusal_code(&error)])),
        }
    }
}

/// The stable code of a cell refusal: the leading dotted code of its message
/// when it has one, else a generic mapping refusal.
fn refusal_code(error: &CellError) -> String {
    let text = error.to_string();
    let code = text.split([':', ' ']).next().unwrap_or_default();
    if code.contains('.') && !code.is_empty() {
        code.to_owned()
    } else {
        "core.boundary.mapping_refused".to_owned()
    }
}

impl<E: SqlEngine> StoreReader<'_, E> {
    // OBS-EVIDENCE-1: cell-owned observation preparation retains raw versions in quarantine.
    pub(crate) fn mapping_observation_slice(
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
