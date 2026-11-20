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
        if observation_id != format!("mapping-bundle:{}", bundle.digest) {
            return Err(CellError::MappingBundleMismatch);
        }
        self.store().read().check_mapping_bundle(&bundle)?;
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
        let bundles = [bundle];
        let reader = self.store().read();
        for bundle in &bundles {
            reader.prepare_mapping_records(
                bundle,
                &catalog,
                &turn,
                &mut commit,
                &mut authorization,
            )?;
        }
        reader.prepare_mapping_links(&bundles, &catalog, &turn, &mut commit, &mut authorization)?;
        // LAW-LINK-1: laws that read a link are evaluated once the bundle's
        // links are known, over the state this commit leaves; a changed target
        // re-checks the subjects that link to it.
        authorization
            .refresh_link_findings(&catalog, &mut commit, &crate::host::GuardView(&reader))
            .map_err(|e| CellError::ObservationPreparation(Box::new(e)))?;
        authorization
            .seal_snapshot_event_chunks(&commit)
            .map_err(|e| CellError::ObservationPreparation(Box::new(e)))?;
        self.commit_observed_evidence(commit, &authorization, &bundles)
            .map_err(|e| CellError::ObservationCommit(Box::new(e)))?;
        Ok(true)
    }
}

/// What processing one captured source row did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MappingOutcome {
    /// Its canonical objects were committed now.
    Landed,
    /// It was consumed and kept as evidence, but SOURCE-RESOLVE-1 wrote
    /// none of these records: a property of each has no deciding report yet
    /// (only checks reported it). Each names the writers it awaits; the
    /// record is written once one of them reports. A sync counts it as held
    /// back (`source.resolution.withheld`), never as landed.
    Withheld(Vec<crate::source_resolution::WithheldSubject>),
    /// It was already consumed (the same source event, captured before).
    AlreadyLanded,
    /// It stays in the inbox as quarantined evidence, for these reasons
    /// (mapping diagnostic codes, or the cell's refusal code).
    Quarantined(Vec<String>),
}

impl MappingOutcome {
    /// Why a sync holds the row back, by code; `None` when it landed (now or
    /// before). A withheld row is held back as [`WITHHELD_REASON`].
    #[must_use]
    pub fn held_back(&self) -> Option<Vec<String>> {
        match self {
            Self::Landed | Self::AlreadyLanded => None,
            Self::Withheld(_) => Some(vec![WITHHELD_REASON.to_owned()]),
            Self::Quarantined(codes) => Some(codes.clone()),
        }
    }
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
            return self.redelivered_outcome(observation_id);
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
            Ok(true) => self.landed_outcome(&bundle),
            Ok(false) => self.redelivered_outcome(observation_id),
            Err(error @ (CellError::Sqlite(_) | CellError::Engine(_))) => Err(error),
            Err(error) => Ok(MappingOutcome::Quarantined(vec![refusal_code(&error)])),
        }
    }
}

impl<S: SqlEngine> SqliteCell<ObservationEvent, RuntimeObject, RuntimeObject, S> {
    /// The outcome of a bundle just consumed: landed, or withheld when
    /// SOURCE-RESOLVE-1 wrote none of some of its records.
    pub(crate) fn landed_outcome(
        &self,
        bundle: &MappingBundle,
    ) -> Result<MappingOutcome, CellError> {
        let withheld = self.store().read().withheld_subjects(bundle)?;
        Ok(if withheld.is_empty() {
            MappingOutcome::Landed
        } else {
            MappingOutcome::Withheld(withheld)
        })
    }
}

impl<S: SqlEngine> SqliteCell<ObservationEvent, RuntimeObject, RuntimeObject, S> {
    /// The outcome of a row consumed before: still [`MappingOutcome::Withheld`]
    /// while SOURCE-RESOLVE-1 withholds some of its records (their deciding
    /// source has not reported), so a redelivery never counts it as landed;
    /// otherwise [`MappingOutcome::AlreadyLanded`].
    fn redelivered_outcome(&self, observation_id: &str) -> Result<MappingOutcome, CellError> {
        let reader = self.store().read();
        let Some(bundle) = reader.consumed_mapping_bundle(observation_id)? else {
            return Ok(MappingOutcome::AlreadyLanded);
        };
        let withheld = reader.withheld_subjects(&bundle)?;
        Ok(if withheld.is_empty() {
            MappingOutcome::AlreadyLanded
        } else {
            MappingOutcome::Withheld(withheld)
        })
    }
}

impl<S: SqlEngine> SqliteCell<ObservationEvent, RuntimeObject, RuntimeObject, S> {
    /// SOURCE-RESOLVE-1: after definitions with changed authorities are
    /// activated, write every subject whose stored values differ from what
    /// the active definitions decide over its retained reports, so the
    /// current resolution read and the stored value agree. Hosts call it
    /// right after each activation; it writes nothing when they agree.
    /// The activation commit marks it pending and this clears the mark once
    /// every chunk has committed, so a host that reopens a cell with the
    /// mark set ([`crate::CellStore::source_resolution_pending`]) resumes it before
    /// serving; a resumed pass recomputes and skips chunks already consumed.
    /// Returns the number of subjects written.
    ///
    /// # Errors
    /// Store failures, or a write the kernel refuses.
    pub fn reconcile_source_resolution(&mut self) -> Result<usize, CellError> {
        let changes = self.store().read().reconciliation()?;
        if changes.is_empty() {
            self.store().finish_source_resolution()?;
            return Ok(0);
        }
        let model = self.store().model().clone();
        let catalog = TypeCatalog::new(model.object_types())
            .with_interfaces(model.interfaces())
            .with_property_types(model.property_types())
            .with_governing_laws(model.law_artifacts());
        let version = self
            .store()
            .read()
            .active_definitions_version()?
            .unwrap_or(0);
        let mut written: usize = 0;
        for chunk in changes.chunks(RECONCILE_CHUNK) {
            // The kernel admits observed writes only as a delivery: each
            // chunk is a cell-owned one naming the activation and subjects,
            // addressed by its content so a retry consumes the same one.
            let subjects: Vec<RuntimeValue> = chunk
                .iter()
                .map(|(t, k, ..)| RuntimeValue::Array(vec![t.as_str().into(), k.as_str().into()]))
                .collect();
            let input = norn_ir::runtime_object! {
                "contract"=>"norn.source_resolution/v1", "definitions_version"=>version, "subjects"=>RuntimeValue::Array(subjects),
            }
            .as_object()
            .cloned()
            .ok_or(CellError::MappingBundleMismatch)?;
            let digest = norn_ir::canonical::domain_hash_hex(
                b"norn.source.resolution.v1\0",
                &serde_json::to_value(&input)?,
            )?;
            let observed = Observed {
                id: norn_kernel::ObservationId(format!("source-resolution:{digest}")),
                provenance: norn_kernel::Provenance {
                    source: "norn.cell".into(),
                    source_id: format!("definitions/{version}"),
                    mapping_version: "source-resolution/1".into(),
                    observed_at: self.now(),
                },
                input,
            };
            if !self.inject(&observed)?
                && self
                    .store()
                    .read()
                    .pending_observation(&observed.id.0)?
                    .is_none()
            {
                // Already consumed: this exact reconciliation was committed.
                continue;
            }
            let turn = DecisionTurn {
                id: observed.id.0.clone(),
                now: self.now(),
                world: self.store().world(),
                consumes: Some(observed.consumed()),
            };
            let mut commit = CellCommit::empty().consuming(observed.consumed());
            let mut authorization = ObservationAuthorization::empty();
            let reader = self.store().read();
            for (object_type, key, values, provenance) in chunk {
                reader.prepare_resolved_write(
                    &catalog,
                    &turn,
                    (object_type, key, values, provenance),
                    &mut commit,
                    &mut authorization,
                )?;
            }
            authorization
                .seal_snapshot_event_chunks(&commit)
                .map_err(|e| CellError::ObservationPreparation(Box::new(e)))?;
            self.commit_observed_evidence(commit, &authorization, &[])
                .map_err(|e| CellError::ObservationCommit(Box::new(e)))?;
            written = written.saturating_add(chunk.len());
        }
        self.store().finish_source_resolution()?;
        Ok(written)
    }
}

/// Subjects one reconciliation commit writes.
const RECONCILE_CHUNK: usize = 256;

/// The reason SOURCE-RESOLVE-1 withholds a row, as a sync counts it.
pub const WITHHELD_REASON: &str = "source.resolution.withheld";

/// The reason a cell refusal quarantines a row: the leading dotted code of
/// its message when it has one; otherwise the generic mapping refusal with
/// the underlying error after it, so the cause is never hidden behind the
/// generic code (`core.boundary.mapping_refused: unknown object type …`).
fn refusal_code(error: &CellError) -> String {
    let text = error.to_string();
    let code = text.split([':', ' ']).next().unwrap_or_default();
    if code.contains('.') && !code.is_empty() {
        code.to_owned()
    } else {
        format!("{MAPPING_REFUSED}: {text}")
    }
}

/// The generic reason of a mapping refusal whose error names no code.
const MAPPING_REFUSED: &str = "core.boundary.mapping_refused";

impl<E: SqlEngine> StoreReader<'_, E> {
    /// The checks every mapped commit makes before preparing a bundle: the
    /// mapping recomputes to the retained bundle under the active pins, and
    /// the source event is neither in conflict nor disputed.
    pub(crate) fn check_mapping_bundle(&self, bundle: &MappingBundle) -> Result<(), CellError> {
        self.recheck_mapping_bundle(bundle)?;
        if self.installed_definitions(None)?.map(|d| d.version) != Some(bundle.definitions_version)
        {
            return Err(CellError::MappingBundleMismatch);
        }
        if self.mapping_conflict(bundle)?.is_some() || self.mapping_event_is_disputed(bundle)? {
            return Err(CellError::UnsupportedMappingProfile);
        }
        if bundle.raw.get("kind").and_then(RuntimeValue::as_str) != Some("upsert")
            || !bundle.diagnostics.is_empty()
        {
            return Err(CellError::UnsupportedMappingProfile);
        }
        Ok(())
    }

    /// Append one bundle's record snapshots to `commit`.
    pub(crate) fn prepare_mapping_records(
        &self,
        bundle: &MappingBundle,
        catalog: &TypeCatalog<'_>,
        turn: &DecisionTurn,
        commit: &mut CellCommit<ObservationEvent, RuntimeObject, RuntimeObject>,
        authorization: &mut ObservationAuthorization,
    ) -> Result<(), CellError> {
        // SOURCE-RESOLVE-1: a subject this source record contributed to and
        // no longer reports is decided again over the remaining reports.
        let id = format!("mapping-bundle:{}", bundle.digest);
        for (object_type, key, values, provenance) in self.withdrawn_changes(bundle, &id)? {
            self.prepare_resolved_write(
                catalog,
                turn,
                (&object_type, &key, &values, &provenance),
                commit,
                authorization,
            )?;
        }
        for record in &bundle.records {
            if record.invocations.len() != 1 {
                return Err(CellError::UnsupportedMappingProfile);
            }
            let key = FactKey::new(record.object_type.as_str(), &record.key);
            let slice = self.mapping_observation_slice(&key)?;
            let mut values = record
                .invocations
                .first()
                .ok_or(CellError::MappingBundleMismatch)?
                .input
                .clone();
            values.remove(norn_checker::mapping::OBJECT_ID_FIELD);
            // SOURCE-RESOLVE-1: a property several sources write takes the
            // value its declared authorities decide over every source's
            // latest report, never this report's merely for arriving last.
            let current = slice.get(&key).and_then(|state| state.values.as_ref());
            match self.resolve_mapped_record(bundle, record, current)? {
                crate::source_resolution::Applied::AsMapped => {}
                crate::source_resolution::Applied::Withheld => continue,
                crate::source_resolution::Applied::Values(resolved) => {
                    if resolved.is_empty() && record.lifecycle_snapshots.is_empty() {
                        continue;
                    }
                    values = resolved;
                }
            }
            let prepared = prepare_observed_snapshot(
                catalog,
                &slice,
                turn,
                ObservedSnapshot {
                    key: &key,
                    values: &values,
                    lifecycles: &record.lifecycle_snapshots,
                    provenance: &record.provenance,
                },
            )
            .map_err(|e| CellError::ObservationPreparation(Box::new(e)))?;
            prepared
                .append_to(commit, authorization)
                .map_err(|e| CellError::ObservationPreparation(Box::new(e)))?;
        }
        Ok(())
    }

    /// Append the observed write of `values` (decided by SOURCE-RESOLVE-1,
    /// attributed to `provenance`) to `object_type`/`key`.
    pub(crate) fn prepare_resolved_write(
        &self,
        catalog: &TypeCatalog<'_>,
        turn: &DecisionTurn,
        (object_type, key, values, provenance): (
            &str,
            &str,
            &RuntimeObject,
            &norn_ir::provenance::InstanceProvenance,
        ),
        commit: &mut CellCommit<ObservationEvent, RuntimeObject, RuntimeObject>,
        authorization: &mut ObservationAuthorization,
    ) -> Result<(), CellError> {
        let key = FactKey::new(object_type, key);
        let slice = self.mapping_observation_slice(&key)?;
        prepare_observed_snapshot(
            catalog,
            &slice,
            turn,
            ObservedSnapshot {
                key: &key,
                values,
                lifecycles: &[],
                provenance,
            },
        )
        .map_err(|e| CellError::ObservationPreparation(Box::new(e)))?
        .append_to(commit, authorization)
        .map_err(|e| CellError::ObservationPreparation(Box::new(e)))
    }

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

#[cfg(test)]
mod tests {
    use super::{CellError, refusal_code};

    #[test]
    fn a_refusal_without_a_code_carries_its_underlying_error() {
        // The cause stays visible: an unknown claim type (a set missing the
        // cell-owned evidence types) or an exhausted claim budget, not only
        // the generic code.
        assert_eq!(
            refusal_code(&CellError::UnknownObjectType(
                "norn.ObservedLinkClaim".into()
            )),
            "core.boundary.mapping_refused: unknown object type `norn.ObservedLinkClaim`"
        );
        assert_eq!(
            refusal_code(&CellError::OutOfRange("link claim evidence bytes")),
            "core.boundary.mapping_refused: link claim evidence bytes is out of range"
        );
        // A coded refusal keeps its stable code.
        assert_eq!(
            refusal_code(&CellError::SourceIdentityDisputed),
            "core.boundary.import_finding"
        );
    }
}
