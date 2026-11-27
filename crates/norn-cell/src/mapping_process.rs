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
        self.process_mapping_bundle_with_fence(
            observation_id,
            crate::source_cursor::SourceReportFence::default(),
        )
    }

    pub(crate) fn process_mapping_bundle_with_fence(
        &mut self,
        observation_id: &str,
        source_fence: crate::source_cursor::SourceReportFence<'_>,
    ) -> Result<bool, CellError> {
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
        self.commit_observed_evidence_with_fence(commit, &authorization, &bundles, source_fence)
            .map_err(|e| match e {
                norn_kernel::CommitError::Host(err @ CellError::SourceLeaseFenced { .. }) => err,
                other => CellError::ObservationCommit(Box::new(other)),
            })?;
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
    /// (mapping diagnostics, or the cell's refusal), each explained.
    Quarantined(Vec<QuarantineReason>),
}

/// MAP-EXPLAIN-1: why a captured row is quarantined, in structure a client
/// acts on without parsing a message: the stable code, the mapping rule
/// (map) that refused it and, when a source field does not conform to its
/// declared type, that field, the value it held and the declared type.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct QuarantineReason {
    /// The stable code (`mapping.runtime.*`, `core.boundary.*`).
    pub code: String,
    /// The map (mapping rule) that refused the row, when a map did.
    pub map: Option<String>,
    /// The nonconforming source field, when the refusal is one.
    pub field: Option<String>,
    /// The value that field held, as canonical JSON text.
    pub value: Option<String>,
    /// The field's declared type, in words.
    pub expected: Option<String>,
    /// The refusal in words.
    pub reason: String,
    /// SEC-QUARANTINE-1: the cell refusal's own text, bounded, for a trusted
    /// operator surface only (the dev `--load` report). It can name a stored
    /// subject's live type or values, so no route answers it.
    pub detail: Option<String>,
}

impl QuarantineReason {
    /// A reason with only a code and words (a cell refusal or finding).
    #[must_use]
    pub fn coded(code: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            map: None,
            field: None,
            value: None,
            expected: None,
            reason: reason.into(),
            detail: None,
        }
    }

    /// A commit the cell refused, as an allowlisted code and fixed words
    /// (SEC-QUARANTINE-1): never the refusal's own text.
    #[must_use]
    pub fn refused(error: &CellError) -> Self {
        let code = refusal_code(error);
        let words = refusal_words(&code);
        Self {
            detail: Some(crate::mapping_bundle::bounded_text(&error.to_string())),
            ..Self::coded(code, words)
        }
    }

    /// The reason a retained mapping diagnostic records (MAP-EXPLAIN-1
    /// fields when the bundle retained them; the code and message always).
    fn of_diagnostic(diagnostic: &RuntimeValue) -> Self {
        // Bounded, as a retained diagnostic is (a bundle captured before
        // the bound repeats the field's whole text).
        let text = |key: &str| {
            diagnostic
                .get(key)
                .and_then(RuntimeValue::as_str)
                .map(crate::mapping_bundle::bounded_text)
        };
        Self {
            code: text("code").unwrap_or_else(|| "mapping.refused".to_owned()),
            map: text("map"),
            field: text("field"),
            value: text("value"),
            expected: text("expected"),
            reason: text("reason")
                .or_else(|| text("message"))
                .unwrap_or_default(),
            detail: None,
        }
    }
}

impl MappingOutcome {
    /// The stable codes of a quarantined row, sorted and unique; none otherwise.
    #[must_use]
    pub fn codes(&self) -> Vec<String> {
        let Self::Quarantined(reasons) = self else {
            return Vec::new();
        };
        let mut codes: Vec<String> = reasons.iter().map(|r| r.code.clone()).collect();
        codes.sort();
        codes.dedup();
        codes
    }
}

impl<E, C, I, S: SqlEngine> SqliteCell<E, C, I, S> {
    /// See [`crate::CellStore::capture_source_page`].
    /// # Errors
    /// Lease fencing, cursor conflict, framing refusals, or store errors.
    pub fn capture_source_page(
        &mut self,
        raws: &[Observed<RuntimeValue>],
        input: Option<&crate::mapping_bundle::MappingInput>,
        advance: &crate::source_cursor::CursorAdvanceSpec,
        max_run_rows: usize,
        expected_binding: Option<(&str, &str)>,
    ) -> Result<crate::mapping_page::CapturedSourcePage, CellError> {
        self.store_mut()
            .capture_source_page(raws, input, advance, max_run_rows, expected_binding)
    }

    /// See [`crate::CellStore::capture_source_dispute_row`].
    /// # Errors
    /// Lease fencing, cursor conflict, framing refusals, or store errors.
    pub fn capture_source_dispute_row(
        &mut self,
        raw: &Observed<RuntimeValue>,
        input: &crate::mapping_bundle::MappingInput,
        advance: &crate::source_cursor::CursorAdvanceSpec,
        pending_key: &str,
        expected_binding: Option<(&str, &str)>,
    ) -> Result<crate::mapping_page::CapturedPage, CellError> {
        self.store_mut().capture_source_dispute_row(
            raw,
            input,
            advance,
            pending_key,
            expected_binding,
        )
    }

    /// See [`crate::CellStore::capture_source_report_checked`].
    /// # Errors
    /// [`CellError::SourceLeaseFenced`], framing refusals, or store errors.
    pub fn capture_source_report_checked(
        &mut self,
        raws: &[Observed<RuntimeValue>],
        input: &crate::mapping_bundle::MappingInput,
        fence: crate::source_cursor::SourceReportFence<'_>,
    ) -> Result<crate::mapping_page::CapturedPage, CellError> {
        self.store_mut()
            .capture_source_report_checked(raws, input, fence)
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
        self.process_mapping_outcome_checked(
            observation_id,
            crate::source_cursor::SourceReportFence::default(),
        )
    }

    /// Like [`Self::process_mapping_outcome`], additionally verifying `fence`
    /// (active source lease and connector mapping revision pin) inside the
    /// SQLite commit transaction.
    ///
    /// # Errors
    /// [`CellError::SourceLeaseFenced`] or store failures.
    pub fn process_mapping_outcome_checked(
        &mut self,
        observation_id: &str,
        fence: crate::source_cursor::SourceReportFence<'_>,
    ) -> Result<MappingOutcome, CellError> {
        if observation_id.starts_with("mapping-conflict:")
            || observation_id.starts_with("mapping-identity:")
        {
            let pending = self.store().read().pending_observation(observation_id)?;
            return Ok(if pending.is_some() {
                MappingOutcome::Quarantined(vec![QuarantineReason::coded(
                    "core.boundary.import_finding",
                    if observation_id.starts_with("mapping-conflict:") {
                        "the same source event (connector, partition, source, id, version) was already captured with different content; review it to accept one (FEED-CONFLICT)"
                    } else {
                        "the source identity is disputed (a spelling or binding conflict); review it before it maps"
                    },
                )])
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
            let mut reasons: Vec<QuarantineReason> = bundle
                .diagnostics
                .iter()
                .map(QuarantineReason::of_diagnostic)
                .collect();
            reasons.sort();
            reasons.dedup();
            return Ok(MappingOutcome::Quarantined(reasons));
        }
        match self.process_mapping_bundle_with_fence(observation_id, fence) {
            Ok(true) => Ok(MappingOutcome::Landed),
            Ok(false) => Ok(MappingOutcome::AlreadyLanded),
            Err(error) if crate::mapping_page::is_store_failure(&error) => Err(error),
            Err(error) => Ok(MappingOutcome::Quarantined(vec![
                QuarantineReason::refused(&error),
            ])),
        }
    }

    /// Like [`Self::process_mapping_page`], additionally verifying `fence`
    /// (active source lease and connector mapping revision pin) inside the
    /// SQLite commit transaction that promotes the page.
    ///
    /// # Errors
    /// [`CellError::SourceLeaseFenced`] or store failures.
    pub fn process_mapping_page_checked(
        &mut self,
        page_id: &str,
        fence: crate::source_cursor::SourceReportFence<'_>,
    ) -> Result<Option<Vec<(String, MappingOutcome)>>, CellError> {
        let Some((observed, members)) = self.pending_page(page_id)? else {
            return Ok(None);
        };
        match self.commit_page_with_fence(&observed, &members, fence) {
            Ok(()) => Ok(Some(
                members
                    .into_iter()
                    .map(|(id, _)| (id, MappingOutcome::Landed))
                    .collect(),
            )),
            Err(error) if crate::mapping_page::is_store_failure(&error) => Err(error),
            Err(_refused) => {
                let mut outcomes = Vec::with_capacity(members.len());
                for (id, _) in members {
                    let outcome = self.process_mapping_outcome_checked(&id, fence)?;
                    outcomes.push((id, outcome));
                }
                if let Some((page_obs, remaining)) = self.pending_page(page_id)?
                    && remaining.is_empty()
                {
                    self.commit_page_with_fence(&page_obs, &[], fence)?;
                }
                Ok(Some(outcomes))
            }
        }
    }

    /// MAP-REPROCESS-1: explicitly re-derive one quarantined `mapping-bundle:`
    /// row under the current derivation (MAP-DERIVATION-1), for a row an older
    /// derivation refused that the current one accepts (decimal text before
    /// MAP-DECIMAL-TEXT-1). The original stays retained evidence, verified
    /// under its own derivation; the re-derived bundle has an address of its
    /// own, names the original, and the commit that lands it consumes the
    /// original. Capture and replay never do this: only a trusted host acting
    /// on an operator's decision calls it. A row the current derivation still
    /// refuses is left as it is, with its current reasons.
    ///
    /// # Errors
    /// A row that is not a pending, quarantined bundle of an older derivation
    /// under the active definitions; store failures.
    pub fn reprocess_quarantined_bundle(
        &mut self,
        observation_id: &str,
    ) -> Result<MappingOutcome, CellError> {
        let Some(pending) = self.store().read().pending_observation(observation_id)? else {
            return Ok(MappingOutcome::AlreadyLanded);
        };
        if !observation_id.starts_with("mapping-bundle:") {
            return Err(CellError::UnsupportedMappingProfile);
        }
        let observed: Observed<RuntimeValue> =
            norn_ir::runtime::decode(&pending.observed).map_err(CellError::Encoding)?;
        let reader = self.store().read();
        let original = reader.inbox_bundle(observation_id)?;
        reader.recheck_mapping_bundle(&original)?;
        if original.diagnostics.is_empty()
            || original.reprocesses.is_some()
            || original.derivation >= crate::mapping_bundle::CURRENT_DERIVATION
        {
            return Err(CellError::UnsupportedMappingProfile);
        }
        if reader.installed_definitions(None)?.map(|d| d.version)
            != Some(original.definitions_version)
        {
            return Err(CellError::MappingBundleMismatch);
        }
        let bundle = reader.derived_mapping_bundle(
            &original.raw,
            &original.input,
            original.definitions_version,
            norn_checker::mapping::Derivation::CURRENT,
            Some(observation_id.to_owned()),
        )?;
        if !bundle.diagnostics.is_empty() {
            let mut reasons: Vec<QuarantineReason> = bundle
                .diagnostics
                .iter()
                .map(QuarantineReason::of_diagnostic)
                .collect();
            reasons.sort();
            reasons.dedup();
            return Ok(MappingOutcome::Quarantined(reasons));
        }
        let id = self
            .store_mut()
            .inject_reprocessed_bundle(&observed.provenance, &bundle)?;
        self.process_mapping_outcome(&id)
    }
}

impl<S: SqlEngine> SqliteCell<ObservationEvent, RuntimeObject, RuntimeObject, S> {
    /// The mapping's own findings for a quarantined bundle, as
    /// `code: message` lines (`mapping.runtime.invalid_record: field
    /// "grand_total" value "7,800" is not a number …`): what a load refusal
    /// shows its author. Empty when the bundle has none or is not pending.
    ///
    /// # Errors
    /// Store or decoding failures.
    pub fn mapping_findings(&self, observation_id: &str) -> Result<Vec<String>, CellError> {
        let Some(pending) = self.store().read().pending_observation(observation_id)? else {
            return Ok(Vec::new());
        };
        let observed: Observed<RuntimeValue> =
            norn_ir::runtime::decode(&pending.observed).map_err(CellError::Encoding)?;
        let bundle: MappingBundle =
            norn_ir::runtime::decode(&observed.input).map_err(CellError::Encoding)?;
        Ok(bundle
            .diagnostics
            .iter()
            .map(|diagnostic| {
                let text = |key: &str| {
                    diagnostic
                        .get(key)
                        .and_then(RuntimeValue::as_str)
                        .unwrap_or_default()
                };
                format!("{}: {}", text("code"), text("message"))
            })
            .collect())
    }
}

/// The reason a cell refusal quarantines a row: the leading dotted code of
/// its message when it has one; otherwise the generic mapping refusal with
/// the underlying error after it, so the cause is never hidden behind the
/// generic code (`core.boundary.mapping_refused: unknown object type …`).
/// The stable code a cell refusal names, when its text starts with a token
/// in the code grammar (`core.boundary.bundle_mismatch`: lowercase dotted
/// identifiers); otherwise [`MAPPING_REFUSED`]. Never the refusal's text.
fn refusal_code(error: &CellError) -> String {
    let text = error.to_string();
    let head = text.split([':', ' ']).next().unwrap_or_default();
    let grammar = head.len() <= 96
        && head.split('.').count() >= 2
        && head.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        });
    if grammar {
        head.to_owned()
    } else {
        MAPPING_REFUSED.to_owned()
    }
}

/// SEC-QUARANTINE-1: the codes and words a load may answer for a commit the
/// cell refused. A refusal's own text can name a stored subject's live type
/// or values, which a caller allowed only to sync sources may not read, so
/// the answer is this fixed allowlist, never the refusal's text; the
/// quarantined row keeps its evidence for an operator who can read it.
const KNOWN_REFUSALS: &[(&str, &str)] = &[
    (
        "core.boundary.bundle_mismatch",
        "the retained mapping evidence no longer matches its pinned definitions",
    ),
    (
        "core.boundary.import_finding",
        "the source event is disputed; review it before it maps",
    ),
    (
        "core.boundary.unsupported_mapping_profile",
        "this row is outside the mapping profile the cell applies",
    ),
    (
        MAPPING_REFUSED,
        "the cell refused the mapped change; an operator who can read the affected objects can review the quarantined row",
    ),
];

/// The allowlisted words for `code` (the generic words for any other code).
fn refusal_words(code: &str) -> &'static str {
    KNOWN_REFUSALS
        .iter()
        .find(|(known, _)| *known == code)
        .or_else(|| {
            KNOWN_REFUSALS
                .iter()
                .find(|(known, _)| *known == MAPPING_REFUSED)
        })
        .map_or("", |(_, words)| words)
}

/// The generic reason of a mapping refusal whose error names no code.
const MAPPING_REFUSED: &str = "core.boundary.mapping_refused";

impl<E: SqlEngine> StoreReader<'_, E> {
    /// The checks every mapped commit makes before preparing a bundle: the
    /// mapping recomputes to the retained bundle under the active pins, and
    /// the source event is neither in conflict nor disputed.
    pub(crate) fn check_mapping_bundle(&self, bundle: &MappingBundle) -> Result<(), CellError> {
        self.recheck_mapping_bundle(bundle)?;
        if self.active_definitions_version()? != Some(bundle.definitions_version) {
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
        // The cause stays visible to a trusted operator (an unknown claim
        // type, a set missing the cell-owned evidence types, or an exhausted
        // claim budget), in `detail`, never in the code (SEC-QUARANTINE-1).
        for (error, cause) in [
            (
                CellError::UnknownObjectType("norn.ObservedLinkClaim".into()),
                "unknown object type `norn.ObservedLinkClaim`",
            ),
            (
                CellError::OutOfRange("link claim evidence bytes"),
                "link claim evidence bytes is out of range",
            ),
        ] {
            assert_eq!(refusal_code(&error), "core.boundary.mapping_refused");
            let reason = super::QuarantineReason::refused(&error);
            assert_eq!(reason.code, "core.boundary.mapping_refused");
            assert!(!reason.reason.contains(cause), "{reason:?}");
            assert!(
                reason.detail.as_deref().is_some_and(|d| d.contains(cause)),
                "{reason:?}"
            );
        }
        // A coded refusal keeps its stable code.
        assert_eq!(
            refusal_code(&CellError::SourceIdentityDisputed),
            "core.boundary.import_finding"
        );
    }
}

#[cfg(test)]
mod refusal_disclosure {
    use super::{KNOWN_REFUSALS, QuarantineReason};
    use crate::CellError;

    /// SEC-QUARANTINE-1: a cell refusal answers an allowlisted code and fixed
    /// words, never its own text (which can name a stored subject's live
    /// type), and never puts text into the code.
    #[test]
    fn a_cell_refusal_is_answered_in_allowlisted_words_only() {
        let secret = "subject canon.Item/demo is live as canon.SecretLedger";
        for error in [
            CellError::Encoding(secret.to_owned()),
            CellError::Encoding(format!("core.commit.concrete_type_mismatch: {secret}")),
            CellError::MappingBundleMismatch,
            CellError::UnsupportedMappingProfile,
        ] {
            let reason = QuarantineReason::refused(&error);
            assert!(!reason.reason.contains("Secret"), "{reason:?}");
            assert!(!reason.code.contains("Secret"), "{reason:?}");
            assert!(
                !reason.code.contains([':', ' ']),
                "a code is an identifier: {reason:?}"
            );
            assert!(
                KNOWN_REFUSALS
                    .iter()
                    .any(|(_, words)| *words == reason.reason),
                "{reason:?}"
            );
            assert!(reason.map.is_none() && reason.field.is_none() && reason.value.is_none());
        }
    }
}
