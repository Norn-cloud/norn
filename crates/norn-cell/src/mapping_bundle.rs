//! OBS-EVIDENCE-1: retained mapping output is evidence, never write authority.
use crate::{
    CellError, CellStore, StoreReader,
    engine::{SqlEngine, params},
    system::INBOX,
};
use norn_checker::Diagnostic;
use norn_checker::mapping::{CompiledMapping, SourceDocument, compile_mapping_artifact};
use norn_ir::{ArtifactEnvelope, RuntimeValue, mapping::MappedRecord, provenance::UtcTimestamp};
use norn_kernel::Observed;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};

/// Trusted source selection; every evaluation input participates in bundle identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingInput {
    /// Admitted mapping artifact ID.
    pub mapping: String,
    /// Source dataset selected by connector configuration.
    pub dataset: String,
    /// Configured connector instance.
    pub connector_instance: String,
    /// Configured partition.
    pub partition: String,
    /// Stable capture context, reused during replay.
    pub recorded_at: UtcTimestamp,
}

/// Ordinary captured evidence, including unsuccessful mapping diagnostics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingBundle {
    /// Digest of the sealed raw envelope and complete evaluation pins.
    pub digest: String,
    /// Exact sealed raw envelope, including nonconforming payloads.
    pub raw: RuntimeValue,
    /// Trusted source selection at capture.
    pub input: MappingInput,
    /// Exact retained definition version.
    pub definitions_version: u64,
    /// Digest of the retained canonical artifact set.
    pub definitions_digest: String,
    /// Source and mapping artifact identity, version and digest.
    pub artifact_pins: Vec<(String, String, String)>,
    /// Full mapping outputs, retaining provenance and declared relation outputs.
    pub records: Vec<MappedRecord>,
    /// Mapping failures retained as ordinary evidence.
    pub diagnostics: Vec<RuntimeValue>,
}

impl MappingBundle {
    /// OBS-LINK-CONTRIBUTION-1: bind claims to the existing capture identity namespace.
    pub(crate) fn link_claims(
        &self,
    ) -> Result<Vec<norn_ir::mapping::ObservedLinkClaim>, CellError> {
        let [connector, partition, source, row, _] =
            crate::mapping_identity::keys(&self.raw, &self.input)?;
        let scope = norn_ir::canonical::domain_hash_hex(
            b"norn.link.scope.v1\0",
            &(connector, partition, source, row),
        )?;
        let id = format!("mapping-bundle:{}", self.digest);
        let claims = self.records.iter().flat_map(|r| r.link_claims(&id, &scope));
        Ok(claims.collect())
    }
}

/// Everything a bundle needs from one exact retained mapping, derived once.
#[derive(Debug)]
pub(crate) struct MappingPlan {
    source_system: String,
    source_instance: String,
    datasets: Vec<String>,
    compiled: Result<CompiledMapping, Vec<Diagnostic>>,
    artifact_pins: Vec<(String, String, String)>,
    definitions_digest: String,
    /// Journal model snapshot the full retained proof ran on; a hit requires
    /// the journal to still hold exactly this snapshot.
    model_snapshot: crate::ModelDefinitionSnapshot,
}

/// MAPPING-PLAN-1: retained-definition work is memoized per store, keyed by the
/// version, a digest of the whole installed row read on this call and the
/// mapping. Any change to any column of the row misses, and a miss repeats the
/// full retained proof. A hit still re-reads and verifies the export-journal
/// record, requires its installed row to equal the row read, requires the
/// stored receipt at `commit_seq` to equal the journal's, and requires the
/// journal's model snapshot to equal the one the plan was proven on. The model
/// hash and governing-law checks are pure functions of that snapshot and the
/// keyed row, so a hit performs or provably covers every retained check.
#[derive(Debug, Default)]
pub(crate) struct MappingPlanCache {
    plans: BTreeMap<(u64, String, String), Arc<MappingPlan>>,
    /// The last fully verified install: the raw bytes of every row the
    /// verification read, with what it proved (the set's canonical digest and
    /// the journal's install record).
    verified: Option<(Vec<u8>, String, crate::store::DefinitionInstallRecord)>,
    /// Consumed claim bundles that rechecked under that verified install, by
    /// inbox id, with the exact row text they were decoded from: a row whose
    /// text is unchanged needs no second decode and recomputation.
    pub(crate) claims: BTreeMap<String, (String, MappingBundle)>,
}

/// Most claim bundles [`MappingPlanCache`] keeps.
pub(crate) const MAX_CACHED_CLAIM_BUNDLES: usize = 8_192;

const MAX_MAPPING_PLANS: usize = 16;

fn mismatch() -> CellError {
    CellError::MappingBundleMismatch
}
fn digest(value: &impl Serialize) -> Result<String, CellError> {
    Ok(norn_ir::canonical::domain_hash_hex(
        b"norn.mapping.bundle.v1\0",
        value,
    )?)
}

/// LABEL-LOCALE-1: the definitions a bundle's evidence identity binds. Label catalogs are
/// presentation, never semantics, so adding or changing labels never changes a bundle digest;
/// a set without labels digests exactly as before.
fn semantic_definitions_digest(artifacts: &[ArtifactEnvelope]) -> Result<String, CellError> {
    let semantic: Vec<&ArtifactEnvelope> = artifacts
        .iter()
        .filter(|artifact| artifact.kind() != norn_ir::ArtifactKind::Labels)
        .collect();
    digest(&semantic)
}

impl<E: SqlEngine> StoreReader<'_, E> {
    /// Captured, rechecked mapping evidence for one source event, including consumed inputs.
    /// Callers enforce source disclosure; these records confer no mutation authority.
    ///
    /// # Errors
    /// Corrupt identity, missing historical pins, recomputation mismatch or storage failure.
    pub fn source_event_mapping_bundles(
        &self,
        connector_instance: &str,
        partition: &str,
        source: &str,
        source_id: &str,
        event_id: &str,
    ) -> Result<Vec<MappingBundle>, CellError> {
        let [connector_instance, partition, source, source_id, event_id]: [String; 5] =
            serde_json::from_slice(&norn_ir::canonical::canonical_json_of(&(
                connector_instance,
                partition,
                source,
                source_id,
                event_id,
            ))?)?;
        let rows = self.conn.query(
            &format!("SELECT observation_id, observed FROM {INBOX} WHERE observation_id LIKE 'mapping-bundle:%' AND json_extract(observed, '$.input.input.connectorInstance') = ?1 AND json_extract(observed, '$.input.input.partition') = ?2 AND json_extract(observed, '$.input.raw.source') = ?3 AND json_extract(observed, '$.input.raw.source_id') = ?4 AND json_extract(observed, '$.input.raw.event_id') = ?5 ORDER BY position"),
            params![&connector_instance, &partition, &source, &source_id, &event_id],
        )?;
        let mut bundles = Vec::new();
        for values in rows {
            let row = crate::engine::SqlRow(values);
            let stored_id: String = row.get(0)?;
            let captured: String = row.get(1)?;
            let observed: Observed<RuntimeValue> = serde_json::from_str(&captured)?;
            let bundle: MappingBundle =
                norn_ir::runtime::decode(&observed.input).map_err(CellError::Encoding)?;
            self.recheck_mapping_bundle(&bundle)?;
            if observed.id.0 != stored_id
                || stored_id != format!("mapping-bundle:{}", bundle.digest)
            {
                return Err(CellError::MappingBundleMismatch);
            }
            bundles.push(bundle);
        }
        Ok(bundles)
    }

    /// Resolve a mapped subject's identity root under its retained definition version.
    ///
    /// # Errors
    /// Missing/corrupt retained definitions or an unknown subject type.
    pub fn retained_mapping_root(
        &self,
        version: u64,
        object_type: &str,
    ) -> Result<String, CellError> {
        self.retained_definition_model(version)?
            .chain(object_type)?
            .first()
            .cloned()
            .ok_or_else(|| CellError::UnknownObjectType(object_type.into()))
    }

    /// MAPPING-PLAN-1: the installed row, export-journal record and receipt are
    /// re-read and compared on every call; only the pure model projection,
    /// governing-law check and mapping compilation are reused.
    fn mapping_plan(
        &self,
        input: &MappingInput,
        version: u64,
    ) -> Result<Arc<MappingPlan>, CellError> {
        let Some(installed) = self.installed_definitions(Some(version))? else {
            self.retained_definition_model(version)?;
            return Err(mismatch());
        };
        // MAPPING-PLAN-1 still re-reads the installed row, the journal record
        // with its payload bodies and the receipt on every call. When their
        // raw bytes equal those of the last copy that verified, verification
        // is a function of those bytes and gives the same result, so only a
        // change pays for re-canonicalizing the whole set.
        let fingerprint = self.install_fingerprint(&installed)?;
        let known = self.mapping_plans.and_then(|cache| {
            cache
                .borrow()
                .verified
                .as_ref()
                .filter(|(seen, ..)| *seen == fingerprint)
                .map(|(_, digest, installation)| (digest.clone(), installation.clone()))
        });
        let (set_digest, installation) = if let Some(known) = known {
            known
        } else {
            let proved = (
                digest(&installed)?,
                self.retained_definition_install(&installed)?,
            );
            if let Some(cache) = self.mapping_plans {
                let mut cache = cache.borrow_mut();
                cache.verified = Some((fingerprint, proved.0.clone(), proved.1.clone()));
                // Rechecks were made under the install verified before.
                cache.claims.clear();
            }
            proved
        };
        let key = (version, set_digest, input.mapping.clone());
        let cached = self
            .mapping_plans
            .and_then(|cache| cache.borrow().plans.get(&key).cloned());
        if let Some(plan) = cached
            && plan.model_snapshot == installation.model
        {
            return Ok(plan);
        }
        Self::retained_model_from(&installed, &installation)?;
        let artifacts: Vec<ArtifactEnvelope> = serde_json::from_str(&installed.canonical_set)?;
        let mapping = artifacts
            .iter()
            .find(|a| a.artifact_id == input.mapping)
            .ok_or_else(mismatch)?;
        let document: norn_ir::mapping::MappingIr =
            serde_json::from_value(serde_json::to_value(mapping.payload.to_json_value())?)?;
        let source = artifacts
            .iter()
            .find(|a| a.artifact_id == document.source.schema_id)
            .ok_or_else(mismatch)?;
        let schema: norn_ir::source_schema::SourceSchemaIr =
            serde_json::from_value(serde_json::to_value(source.payload.to_json_value())?)?;
        let artifact_pins = [source, mapping]
            .into_iter()
            .map(|a| {
                let pin = norn_checker::ArtifactRef::of(a)
                    .map_err(|e| CellError::Encoding(e.to_string()))?;
                Ok((pin.artifact_id, pin.version, pin.sha256))
            })
            .collect::<Result<Vec<_>, CellError>>()?;
        let plan = Arc::new(MappingPlan {
            source_system: schema.source.system,
            source_instance: schema.source.instance,
            datasets: schema.datasets.into_iter().map(|d| d.id).collect(),
            compiled: compile_mapping_artifact(mapping, &artifacts, &[]),
            artifact_pins,
            definitions_digest: semantic_definitions_digest(&artifacts)?,
            model_snapshot: installation.model,
        });
        if let Some(cache) = self.mapping_plans {
            let mut cache = cache.borrow_mut();
            if cache.plans.len() >= MAX_MAPPING_PLANS {
                cache.plans.clear();
            }
            cache.plans.insert(key, Arc::clone(&plan));
        }
        Ok(plan)
    }

    /// The raw bytes of every row the install verification reads, each
    /// length-prefixed: the installed row, its journal record, the payload
    /// bodies the record references and the receipt. No canonicalization, and
    /// compared exactly, not by a digest.
    fn install_fingerprint(
        &self,
        installed: &crate::InstalledDefinitionSet,
    ) -> Result<Vec<u8>, CellError> {
        let mut bytes = Vec::new();
        let mut field = |part: &[u8]| {
            bytes.extend_from_slice(&u64::try_from(part.len()).unwrap_or(u64::MAX).to_be_bytes());
            bytes.extend_from_slice(part);
        };
        // The large canonical set as its bytes, unescaped; every other field
        // through the struct's own serialization (a new field either joins
        // through `..` or fails to compile here until it is listed).
        field(installed.canonical_set.as_bytes());
        let mut rest = serde_json::to_value(crate::InstalledDefinitionSet {
            canonical_set: String::new(),
            registry: installed.registry.clone(),
            receipt_sha256: installed.receipt_sha256.clone(),
            admission_receipt: installed.admission_receipt.clone(),
            model_hash: installed.model_hash.clone(),
            ddl_hash: installed.ddl_hash.clone(),
            layout: installed.layout.clone(),
            ..*installed
        })?;
        if let Some(object) = rest.as_object_mut() {
            object.remove("canonicalSet");
        }
        field(serde_json::to_string(&rest)?.as_bytes());
        let seq = crate::read::to_i64(installed.commit_seq, "commit_seq")?;
        let journal: Option<(String, String)> = self.conn.query_optional(
            r#"SELECT "hash", "record" FROM "_norn_export_journal" WHERE "commit_seq" = ?1"#,
            crate::engine::params![seq],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if let Some((stored, record)) = journal {
            field(stored.as_bytes());
            field(record.as_bytes());
        }
        let bodies = self.conn.query(
            r#"SELECT p."scope", p."world", p."body" FROM "_norn_export_payload" e JOIN "_norn_payload" p ON p."hash" = e."hash" WHERE e."commit_seq" = ?1 ORDER BY e."ordinal""#,
            crate::engine::params![seq],
        )?;
        for row in bodies {
            let row = crate::engine::SqlRow(row);
            for column in 0..3 {
                field(row.get::<String>(column)?.as_bytes());
            }
        }
        field(serde_json::to_string(&self.receipt_at(installed.commit_seq)?)?.as_bytes());
        Ok(bytes)
    }

    pub(crate) fn mapping_bundle(
        &self,
        raw: &RuntimeValue,
        input: &MappingInput,
        version: u64,
    ) -> Result<MappingBundle, CellError> {
        let plan = self.mapping_plan(input, version)?;
        let field = |name: &str| {
            raw.get(name)
                .and_then(RuntimeValue::as_str)
                .ok_or_else(mismatch)
        };
        if field("contract")? != "norn.raw_observation/v1"
            || field("source")? != plan.source_system
            || input.connector_instance != plan.source_instance
            || input.partition.trim().is_empty()
            || !plan.datasets.contains(&input.dataset)
        {
            return Err(mismatch());
        }
        let payload = raw.get("payload").ok_or_else(mismatch)?;
        let payload_text = serde_json::to_string(payload)?;
        if norn_ir::canonical::HashProfile::Norn1.hash_json(&payload_text)?
            != field("payload_digest")?
        {
            return Err(mismatch());
        }
        let source_document = SourceDocument::from_json(&input.dataset, &payload_text, None)?;
        let (records, diagnostics) = if field("kind")? == "upsert" {
            let outcome = match &plan.compiled {
                Ok(compiled) => compiled.apply(&[source_document], &input.recorded_at),
                Err(diagnostics) => Err(diagnostics.clone()),
            };
            match outcome {
                Ok(records) => (records, Vec::new()),
                Err(diagnostics) => (
                    Vec::new(),
                    diagnostics
                        .into_iter()
                        .map(|d| {
                            norn_ir::runtime::encode(&d.to_json()).map_err(CellError::Encoding)
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                ),
            }
        } else {
            (
                Vec::new(),
                vec![
                    norn_ir::runtime_object! {"code"=>norn_checker::mapping::codes::EVALUATION_FAILED, "message"=>"CP2 mapping bundle does not apply source deletes"},
                ],
            )
        };
        let address = digest(&(
            digest(raw)?,
            input,
            version,
            &plan.definitions_digest,
            &plan.artifact_pins,
        ))?;
        Ok(MappingBundle {
            digest: address,
            raw: raw.clone(),
            input: input.clone(),
            definitions_version: version,
            definitions_digest: plan.definitions_digest.clone(),
            artifact_pins: plan.artifact_pins.clone(),
            records,
            diagnostics,
        })
    }

    /// Re-run the admitted mapping against the exact retained pins before processing.
    /// This returns evidence only; callers cannot obtain observation authority here.
    ///
    /// # Errors
    /// Changed pins, raw evidence or derived output refuse `core.boundary.bundle_mismatch`.
    pub fn recheck_mapping_bundle(&self, bundle: &MappingBundle) -> Result<(), CellError> {
        let active = self.active_definitions_version()?.ok_or_else(mismatch)?;
        if bundle.definitions_version > active
            || self
                .installed_definitions(Some(bundle.definitions_version))?
                .is_none()
        {
            return Err(mismatch());
        }
        if self.mapping_bundle(&bundle.raw, &bundle.input, bundle.definitions_version)? != *bundle {
            return Err(mismatch());
        }
        Ok(())
    }
}

impl<E: SqlEngine> CellStore<E> {
    /// Capture cell-derived mapping evidence through the existing Observed inbox.
    /// Caller-provided mapped outputs and claims are never accepted.
    ///
    /// # Errors
    /// Missing admitted pins, invalid raw framing or durable inbox failure.
    pub fn inject_mapping_bundle(
        &mut self,
        raw: &Observed<RuntimeValue>,
        input: &MappingInput,
    ) -> Result<(String, bool), CellError> {
        self.conn
            .transaction(|| self.capture_mapping_bundle(raw, input))
    }

    /// [`Self::inject_mapping_bundle`] inside the caller's transaction.
    pub(crate) fn capture_mapping_bundle(
        &self,
        raw: &Observed<RuntimeValue>,
        input: &MappingInput,
    ) -> Result<(String, bool), CellError> {
        let version = self
            .read()
            .installed_definitions(None)?
            .ok_or_else(mismatch)?
            .version;
        if let Some(captured) = crate::mapping_identity::capture(self, raw, input, version)? {
            return Ok(captured);
        }
        let bundle = self.read().mapping_bundle(&raw.input, input, version)?;
        crate::mapping_conflict::capture(self, raw, &bundle)
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[allow(clippy::expect_used, reason = "test fixtures")]
mod label_identity {
    use super::*;
    use proptest::prelude::*;

    fn envelope(id: &str, kind: &str, payload: &serde_json::Value) -> ArtifactEnvelope {
        serde_json::from_value(serde_json::json!({
            "irVersion": "0.1", "artifactId": id, "kind": kind, "namespace": "t", "name": id,
            "version": "1.0.0", "lifecycle": "active", "scope": {}, "dependencies": [],
            "provenance": { "source": "system" }, "payload": payload,
        }))
        .expect("envelope")
    }

    proptest! {
        /// LABEL-LOCALE-1: any label catalogs, anywhere in the installed set, leave the
        /// definitions digest a mapping bundle binds exactly as the label-free set's.
        #[test]
        fn labels_never_change_the_bundle_definitions_digest(
            texts in prop::collection::vec("[a-z\u{0621}-\u{064A}]{1,8}", 0..4),
            at in 0usize..3,
        ) {
            let definitions = vec![
                envelope("t.a", "rule", &serde_json::json!({ "ruleId": "t.a" })),
                envelope("t.b", "rule", &serde_json::json!({ "ruleId": "t.b" })),
            ];
            let mut labelled = definitions.clone();
            for (index, text) in texts.iter().enumerate() {
                let catalog = envelope(&format!("t.labels{index}"), "labels", &serde_json::json!({
                    "labelsVersion": "0.1",
                    "entries": [{ "target": { "kind": "rule", "ruleId": "t.a" }, "label": { "ar": text } }],
                }));
                labelled.insert(at.min(labelled.len()), catalog);
            }
            prop_assert_eq!(
                semantic_definitions_digest(&labelled).expect("digest"),
                digest(&definitions.iter().collect::<Vec<_>>()).expect("digest")
            );
        }
    }
}
