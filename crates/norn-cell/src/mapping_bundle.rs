//! OBS-EVIDENCE-1: retained mapping output is evidence, never write authority.
use crate::{
    CellError, CellStore, StoreReader,
    engine::{SqlEngine, params},
    system::INBOX,
};
use norn_checker::Diagnostic;
use norn_checker::mapping::{
    CompiledMapping, Derivation, SourceDocument, compile_mapping_artifact,
};
use norn_ir::{ArtifactEnvelope, RuntimeValue, mapping::MappedRecord, provenance::UtcTimestamp};
use norn_kernel::Observed;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

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
    /// MAP-DERIVATION-1: the runtime derivation the bundle was captured
    /// under (`1`, absent on bundles captured before it, or `2`). A bundle is
    /// always verified under its own derivation.
    #[serde(
        default = "legacy_derivation",
        skip_serializing_if = "is_legacy_derivation"
    )]
    pub derivation: u32,
    /// MAP-REPROCESS-1: the quarantined `mapping-bundle:` row this bundle
    /// explicitly re-derives under a newer derivation
    /// ([`crate::SqliteCell::reprocess_quarantined_bundle`]); its commit
    /// consumes that row. Never set by capture or replay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reprocesses: Option<String>,
}

/// The derivation number new captures record ([`Derivation::CURRENT`]).
pub(crate) const CURRENT_DERIVATION: u32 = derivation_number(Derivation::CURRENT);

const fn legacy_derivation() -> u32 {
    1
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde skip_serializing_if signature"
)]
const fn is_legacy_derivation(derivation: &u32) -> bool {
    *derivation == 1
}

/// The checker derivations a retained bundle's number can name. An
/// unversioned bundle (`1`) was captured either before decimal text
/// conformed or under main's MAPPING-DECIMAL-TEXT-1, so it verifies under
/// either (S1, #1488).
fn derivations_of(version: u32) -> Result<&'static [Derivation], CellError> {
    match version {
        1 => Ok(&[Derivation::V1DecimalText, Derivation::V1]),
        2 => Ok(&[Derivation::V2]),
        _ => Err(mismatch()),
    }
}

const fn derivation_number(derivation: Derivation) -> u32 {
    match derivation {
        Derivation::V1 | Derivation::V1DecimalText => 1,
        Derivation::V2 => 2,
    }
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

/// INGEST-GROUP-1: the identity roots one mapping input can write, and the
/// roots its links can name ([`StoreReader::mapping_footprint`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MappingFootprint {
    /// Roots the input's maps write.
    pub writes: BTreeSet<String>,
    /// Roots the input's links name.
    pub links: BTreeSet<String>,
}

impl MappingFootprint {
    /// Whether records of the two inputs commit the same way in either
    /// relative order: no root both write, no link from one into the other's
    /// writes, and no link target both name (pending-link records are kept
    /// per target).
    #[must_use]
    pub fn independent(&self, other: &Self) -> bool {
        self.writes.is_disjoint(&other.writes)
            && self.links.is_disjoint(&other.writes)
            && other.links.is_disjoint(&self.writes)
            && self.links.is_disjoint(&other.links)
    }
}

/// Everything a bundle needs from one exact retained mapping, derived once.
#[derive(Debug)]
pub(crate) struct MappingPlan {
    source_system: String,
    source_instance: String,
    datasets: Vec<String>,
    feed_declaration: Option<norn_ir::feed::FeedDeclaration>,
    compiled: Result<CompiledMapping, Vec<Diagnostic>>,
    artifact_pins: Vec<(String, String, String)>,
    definitions_digest: String,
    /// Journal model snapshot the full retained proof ran on; a hit requires
    /// the journal to still hold exactly this snapshot.
    model_snapshot: crate::ModelDefinitionSnapshot,
}

impl MappingPlan {
    pub(crate) fn declared_feed(&self, dataset: &str) -> Option<norn_ir::feed::FeedContract> {
        self.feed_declaration
            .as_ref()
            .and_then(|decl| decl.feeds.get(dataset).copied())
    }
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
    /// INGEST-DERIVE-ONCE-1: bundles this store derived, by digest, with the
    /// exact plan they were derived under. A bundle equal to one derived under
    /// the very plan a recheck verifies is that recheck's derivation: the
    /// derivation is a pure function of the plan and the bundle's own inputs
    /// (raw, input, version, derivation, reprocesses), all compared here.
    derived: BTreeMap<String, (Arc<MappingPlan>, MappingBundle)>,
    /// INGEST-DERIVE-ONCE-1: plans proven inside the current transaction scope
    /// ([`PinnedPlans`]), by version and mapping. Nothing writes the retained
    /// definitions, the export journal or receipts inside such a scope, so the
    /// install a plan was proven from is the one every later read in it sees.
    pinned: Option<BTreeMap<(u64, String), Arc<MappingPlan>>>,
}

/// Most bundles [`MappingPlanCache`] keeps as derived: two full pages, so a
/// page's capture is still known when its commit rechecks it.
const MAX_DERIVED_BUNDLES: usize = 2 * crate::mapping_page::MAX_PAGE_RECORDS;

/// INGEST-DERIVE-ONCE-1: a scope inside one engine transaction in which a
/// retained mapping plan, once proven, is reused instead of re-reading and
/// re-verifying the install on every derivation. Only a scope that writes no
/// retained definitions, export journal rows or receipts may open one (a
/// page's capture, a commit's read-only guard). A nested scope leaves the
/// outer one in place.
pub(crate) struct PinnedPlans<'a> {
    cache: &'a std::cell::RefCell<MappingPlanCache>,
    opened: bool,
}

impl<'a> PinnedPlans<'a> {
    pub(crate) fn open(cache: &'a std::cell::RefCell<MappingPlanCache>) -> Self {
        let mut plans = cache.borrow_mut();
        let opened = plans.pinned.is_none();
        if opened {
            plans.pinned = Some(BTreeMap::new());
        }
        drop(plans);
        Self { cache, opened }
    }
}

impl Drop for PinnedPlans<'_> {
    fn drop(&mut self) {
        if self.opened {
            // No borrow of the cache outlives a call, so this never conflicts;
            // a scope's plans never survive it.
            self.cache.borrow_mut().pinned = None;
        }
    }
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
        self.check_live()?;
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
        self.check_live()?;
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
        let pin = (version, input.mapping.clone());
        if let Some(cache) = self.mapping_plans
            && let Some(plan) = cache
                .borrow()
                .pinned
                .as_ref()
                .and_then(|pinned| pinned.get(&pin))
        {
            return Ok(Arc::clone(plan));
        }
        let plan = self.proven_mapping_plan(input, version)?;
        if let Some(cache) = self.mapping_plans
            && let Some(pinned) = cache.borrow_mut().pinned.as_mut()
        {
            pinned.insert(pin, Arc::clone(&plan));
        }
        Ok(plan)
    }

    /// [`Self::mapping_plan`] proven from the store: the full MAPPING-PLAN-1
    /// verification of the installed row, journal record and receipt.
    fn proven_mapping_plan(
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
        let feed_declaration =
            norn_ir::feed::FeedDeclaration::from_extensions(document.extensions.as_ref())
                .map_err(|_err| mismatch())?;
        let plan = Arc::new(MappingPlan {
            source_system: schema.source.system,
            source_instance: schema.source.instance,
            datasets: schema.datasets.into_iter().map(|d| d.id).collect(),
            feed_declaration,
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

    /// A fresh capture's bundle, under the current derivation.
    pub(crate) fn mapping_bundle(
        &self,
        raw: &RuntimeValue,
        input: &MappingInput,
        version: u64,
    ) -> Result<MappingBundle, CellError> {
        self.derived_mapping_bundle(raw, input, version, Derivation::CURRENT, None)
    }

    /// The bundle `raw` derives under an exact `derivation` (MAP-DERIVATION-1).
    /// Its address is the capture address every derivation shares, so a replay
    /// of a captured event finds the row it already has; only an explicit
    /// reprocessing (`reprocesses`) gets an address of its own.
    pub(crate) fn derived_mapping_bundle(
        &self,
        raw: &RuntimeValue,
        input: &MappingInput,
        version: u64,
        derivation: Derivation,
        reprocesses: Option<String>,
    ) -> Result<MappingBundle, CellError> {
        let plan = self.mapping_plan(input, version)?;
        self.derive_under(plan, raw, input, version, derivation, reprocesses)
    }

    /// [`Self::derived_mapping_bundle`] under an already proven `plan`,
    /// remembered as derived under it (INGEST-DERIVE-ONCE-1).
    fn derive_under(
        &self,
        plan: Arc<MappingPlan>,
        raw: &RuntimeValue,
        input: &MappingInput,
        version: u64,
        derivation: Derivation,
        reprocesses: Option<String>,
    ) -> Result<MappingBundle, CellError> {
        let bundle = Self::compute_under(&plan, raw, input, version, derivation, reprocesses)?;
        if let Some(cache) = self.mapping_plans {
            let mut cache = cache.borrow_mut();
            if cache.derived.len() >= MAX_DERIVED_BUNDLES {
                cache.derived.clear();
            }
            cache
                .derived
                .insert(bundle.digest.clone(), (plan, bundle.clone()));
        }
        Ok(bundle)
    }

    /// The bundle `raw` derives under `plan`: a pure function of its
    /// arguments, which reads and writes no cache.
    fn compute_under(
        plan: &MappingPlan,
        raw: &RuntimeValue,
        input: &MappingInput,
        version: u64,
        derivation: Derivation,
        reprocesses: Option<String>,
    ) -> Result<MappingBundle, CellError> {
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
        let feed_mode = parse_raw_feed_mode(plan, &input.dataset, raw, field("source_version")?)?;
        let source_document =
            SourceDocument::from_json(&input.dataset, &payload_text, None)?.with_mode(feed_mode);
        let (records, diagnostics) = if field("kind")? == "upsert" {
            let outcome = match &plan.compiled {
                Ok(compiled) => compiled
                    .apply_derived(&[source_document], &input.recorded_at, derivation)
                    .map_err(|refusals| match derivation {
                        // Exactly the diagnostics captured before MAP-EXPLAIN-1.
                        Derivation::V1 | Derivation::V1DecimalText => refusals
                            .iter()
                            .map(|refusal| refusal.diagnostic().to_json())
                            .collect::<Vec<_>>(),
                        Derivation::V2 => refusals.iter().map(explained).collect(),
                    }),
                Err(diagnostics) => Err(diagnostics.iter().map(Diagnostic::to_json).collect()),
            };
            match outcome {
                Ok(records) => (records, Vec::new()),
                Err(diagnostics) => (
                    Vec::new(),
                    diagnostics
                        .iter()
                        .map(|d| norn_ir::runtime::encode(d).map_err(CellError::Encoding))
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
        let mut address = digest(&(
            digest(raw)?,
            input,
            version,
            &plan.definitions_digest,
            &plan.artifact_pins,
        ))?;
        if let Some(original) = &reprocesses {
            // MAP-REPROCESS-1: only the reprocessed row's own capture address,
            // re-derived under a newer derivation, ever has one.
            if *original != format!("mapping-bundle:{address}")
                || derivation_number(derivation) == 1
            {
                return Err(mismatch());
            }
            address = norn_ir::canonical::domain_hash_hex(
                b"norn.mapping.reprocess.v1\0",
                &(&address, derivation_number(derivation)),
            )?;
        }
        Ok(MappingBundle {
            digest: address,
            raw: raw.clone(),
            input: input.clone(),
            definitions_version: version,
            definitions_digest: plan.definitions_digest.clone(),
            artifact_pins: plan.artifact_pins.clone(),
            records,
            diagnostics,
            derivation: derivation_number(derivation),
            reprocesses,
        })
    }

    /// INGEST-DERIVE-ONCE-1: whether `bundle` equals one this store derived
    /// under exactly `plan` (the same proven plan object), field for field.
    fn derived_under(&self, plan: &Arc<MappingPlan>, bundle: &MappingBundle) -> bool {
        self.mapping_plans.is_some_and(|cache| {
            cache
                .borrow()
                .derived
                .get(&bundle.digest)
                .is_some_and(|(derived_plan, derived)| {
                    Arc::ptr_eq(derived_plan, plan) && derived == bundle
                })
        })
    }

    /// INGEST-GROUP-1: what records of `input` can write and link to under
    /// the active definitions, as identity roots (a subtype and its root
    /// share keys).
    ///
    /// # Errors
    /// No active definitions, a mapping that does not compile, or a type the
    /// model cannot root (an interface link target): the caller then keeps
    /// the batch's own order.
    pub fn mapping_footprint(&self, input: &MappingInput) -> Result<MappingFootprint, CellError> {
        self.check_live()?;
        let version = self.active_definitions_version()?.ok_or_else(mismatch)?;
        let plan = self.mapping_plan(input, version)?;
        let compiled = plan.compiled.as_ref().map_err(|_diagnostics| mismatch())?;
        let (writes, links) = compiled.footprint(&input.dataset);
        let root = |types: BTreeSet<String>| {
            types
                .iter()
                .map(|ty| self.retained_mapping_root(version, ty))
                .collect::<Result<BTreeSet<_>, _>>()
        };
        Ok(MappingFootprint {
            writes: root(writes)?,
            links: root(links)?,
        })
    }

    /// Re-run the admitted mapping against the exact retained pins before processing.
    /// This returns evidence only; callers cannot obtain observation authority here.
    ///
    /// # Errors
    /// Changed pins, raw evidence or derived output refuse `core.boundary.bundle_mismatch`.
    pub fn recheck_mapping_bundle(&self, bundle: &MappingBundle) -> Result<(), CellError> {
        self.check_live()?;
        let active = self.active_definitions_version()?.ok_or_else(mismatch)?;
        if bundle.definitions_version > active
            || !self.definitions_version_retained(bundle.definitions_version)?
        {
            return Err(mismatch());
        }
        let derivations = derivations_of(bundle.derivation)?;
        // INGEST-DERIVE-ONCE-1: the plan is proven exactly as a derivation
        // proves it. A bundle this store derived under that same plan, equal
        // field for field, was derived under one of `derivations` (its own
        // derivation number names them), so it is this recheck's result and
        // is not derived again.
        let plan = self.mapping_plan(&bundle.input, bundle.definitions_version)?;
        if self.derived_under(&plan, bundle) {
            // A debug-only cross-check, never an admission decision: it
            // derives afresh without touching the cache and asserts the
            // remembered bundle is that derivation, so debug and release
            // admit exactly the same bundles.
            debug_assert!(
                derivations.iter().any(|derivation| {
                    Self::compute_under(
                        &plan,
                        &bundle.raw,
                        &bundle.input,
                        bundle.definitions_version,
                        *derivation,
                        bundle.reprocesses.clone(),
                    )
                    .is_ok_and(|fresh| fresh == *bundle)
                }),
                "a remembered derivation differs from a fresh one"
            );
            return Ok(());
        }
        // MAP-DERIVATION-1: verified under the derivation it was captured
        // with, so a newer parser never changes what retained evidence proves.
        for derivation in derivations {
            if self.derive_under(
                Arc::clone(&plan),
                &bundle.raw,
                &bundle.input,
                bundle.definitions_version,
                *derivation,
                bundle.reprocesses.clone(),
            )? == *bundle
            {
                return Ok(());
            }
        }
        Err(mismatch())
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
        self.write_tx(|| self.capture_mapping_bundle(raw, input))
    }

    /// MAP-REPROCESS-1: retain an explicitly re-derived bundle at its own
    /// address, with the provenance of the row it reprocesses. Only
    /// [`crate::SqliteCell::reprocess_quarantined_bundle`] calls this.
    pub(crate) fn inject_reprocessed_bundle(
        &mut self,
        provenance: &norn_kernel::Provenance,
        bundle: &MappingBundle,
    ) -> Result<String, CellError> {
        if bundle.reprocesses.is_none() {
            return Err(mismatch());
        }
        let id = format!("mapping-bundle:{}", bundle.digest);
        let observed = Observed {
            id: norn_kernel::ObservationId(id.clone()),
            provenance: provenance.clone(),
            input: norn_ir::runtime::encode(bundle).map_err(CellError::Encoding)?,
        };
        let encoded = norn_ir::runtime::encode(&observed).map_err(CellError::Encoding)?;
        self.write_tx(|| crate::store::inject_observation(&self.conn, &id, &encoded))?;
        Ok(id)
    }

    /// [`Self::inject_mapping_bundle`] inside the caller's transaction.
    pub(crate) fn capture_mapping_bundle(
        &self,
        raw: &Observed<RuntimeValue>,
        input: &MappingInput,
    ) -> Result<(String, bool), CellError> {
        // The active version alone; the plan proves the set it names.
        let version = self
            .read()
            .active_definitions_version()?
            .ok_or_else(mismatch)?;
        if let Some(captured) = crate::mapping_identity::capture(self, raw, input, version)? {
            return Ok(captured);
        }
        let bundle = self.read().mapping_bundle(&raw.input, input, version)?;
        crate::mapping_conflict::capture(self, raw, &bundle)
    }
}

/// The most bytes of one retained diagnostic text: a refused row's own
/// field can be as large as the request, and its full value is already in
/// the retained raw evidence, so a diagnostic never repeats it unbounded.
pub(crate) const DIAGNOSTIC_TEXT_BYTES: usize = 256;

/// `text`, cut to [`DIAGNOSTIC_TEXT_BYTES`] at a character boundary.
pub(crate) fn bounded_text(text: &str) -> String {
    bounded(text).0
}

/// `text`, cut to [`DIAGNOSTIC_TEXT_BYTES`] at a character boundary, and
/// whether it was cut.
fn bounded(text: &str) -> (String, bool) {
    if text.len() <= DIAGNOSTIC_TEXT_BYTES {
        return (text.to_owned(), false);
    }
    let mut end = DIAGNOSTIC_TEXT_BYTES;
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    (format!("{}…", text.get(..end).unwrap_or_default()), true)
}

/// MAP-EXPLAIN-1: a row refusal as retained evidence, the mapping's
/// diagnostic plus what it refused: the map (mapping rule) and, for a field
/// that does not conform, the field, a bounded preview of its value, the
/// declared type and why. Every text is bounded ([`DIAGNOSTIC_TEXT_BYTES`]):
/// the value itself is the retained raw evidence's (`valueTruncated` says a
/// preview was cut), so a malformed field never multiplies a row's size.
fn explained(refusal: &norn_checker::mapping::RowRefusal) -> serde_json::Value {
    let mut value = refusal.diagnostic().to_json();
    let text = |text: &str| bounded(text).0.into();
    if let Some(fields) = value.as_object_mut() {
        if let Some(message) = fields.get("message").and_then(serde_json::Value::as_str) {
            let message = text(message);
            fields.insert("message".to_owned(), message);
        }
        fields.insert("map".to_owned(), text(&refusal.map));
        fields.insert("reason".to_owned(), text(&refusal.reason));
        if let Some(field) = &refusal.field {
            fields.insert("field".to_owned(), text(&field.field));
            if let Some(raw) = &field.value {
                let (preview, cut) = bounded(raw);
                fields.insert("value".to_owned(), preview.into());
                if cut {
                    fields.insert("valueTruncated".to_owned(), true.into());
                }
            } else {
                fields.insert("value".to_owned(), serde_json::Value::Null);
            }
            fields.insert("expected".to_owned(), text(&field.expected));
            fields.insert("reason".to_owned(), text(&field.reason));
        }
    }
    value
}

impl<E: SqlEngine> StoreReader<'_, E> {
    /// Authoritative [`norn_ir::VersionKind`] for `bundle`: combines the
    /// retained mapping's `norn.feed/1` declaration with any envelope
    /// `"version_kind"`, refusing contradictions instead of silently falling
    /// back to lexical order.
    pub(crate) fn bundle_version_kind(
        &self,
        bundle: &MappingBundle,
    ) -> Result<norn_ir::VersionKind, CellError> {
        let plan = self.mapping_plan(&bundle.input, bundle.definitions_version)?;
        let declared = plan.declared_feed(&bundle.input.dataset);
        resolve_version_kind(declared, &bundle.raw)
    }
}

fn resolve_version_kind(
    declared: Option<norn_ir::feed::FeedContract>,
    raw: &RuntimeValue,
) -> Result<norn_ir::VersionKind, CellError> {
    let raw_kind = match raw.get("version_kind") {
        None => norn_ir::VersionKind::Lexicographical,
        Some(value) => value
            .as_str()
            .and_then(norn_ir::VersionKind::parse_wire)
            .ok_or_else(mismatch)?,
    };
    if let Some(contract) = declared
        && raw_kind != contract.version_kind
    {
        return Err(mismatch());
    }
    Ok(raw_kind)
}

/// Resolve and cross-check `feed_mode` and `version_kind` between the retained
/// mapping's `norn.feed/1` declaration and the raw observation envelope
/// (`FEED-MODE-1`). Unbound or contradictory deliveries fail closed so a
/// snapshot-projected payload can never enter a `Patch` mapping as clears.
fn parse_raw_feed_mode(
    plan: &MappingPlan,
    dataset: &str,
    raw: &RuntimeValue,
    source_version: &str,
) -> Result<norn_ir::FeedMode, CellError> {
    let declared = plan.declared_feed(dataset);
    let raw_mode = match raw.get("feed_mode") {
        None => norn_ir::FeedMode::FullSnapshot,
        Some(value) => value
            .as_str()
            .and_then(norn_ir::FeedMode::parse_wire)
            .ok_or_else(mismatch)?,
    };
    if let Some(contract) = declared
        && raw_mode != contract.mode
    {
        return Err(mismatch());
    }
    let version_kind = resolve_version_kind(declared, raw)?;
    if declared.is_some() || raw.get("version_kind").is_some() {
        norn_ir::SourceVersion::parse(version_kind, source_version).map_err(|_err| mismatch())?;
    }
    Ok(raw_mode)
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

#[cfg(all(test, feature = "native", not(target_arch = "wasm32")))]
#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "INGEST-DERIVE-ONCE-1 recheck fixture"
)]
mod derive_once_recheck {
    use super::*;
    use crate::{CellStore, InstalledDefinitionInput, Model};
    use norn_ir::{InterfaceDef, ObjectTypeDef, PropertyTypeDef};
    use std::path::PathBuf;

    fn root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    /// The three source packs' artifacts, in id order.
    fn artifacts() -> Vec<ArtifactEnvelope> {
        let mut artifacts: Vec<ArtifactEnvelope> = Vec::new();
        for dir in [
            "core",
            "packs/canon",
            "packs/erpnext",
            "packs/shopify",
            "packs/odoo",
        ] {
            for file in std::fs::read_dir(root().join(format!("registry/{dir}/artifacts")))
                .expect("artifacts")
            {
                let artifact: ArtifactEnvelope = serde_json::from_str(
                    &std::fs::read_to_string(file.expect("file").path()).expect("read"),
                )
                .expect("artifact");
                if ![
                    "commerce.SalesOrder.entity",
                    "core.Order.interface",
                    "core.Entitlement.entity",
                    "identity.BusinessUnit.entity",
                    "identity.Directory.interface",
                    "location.Region.entity",
                    "location.Space.entity",
                    "party.Brand.entity",
                    "party.Organization.entity",
                ]
                .contains(&artifact.artifact_id.as_str())
                {
                    artifacts.push(artifact);
                }
            }
        }
        artifacts.sort_by(|a, b| a.artifact_id.cmp(&b.artifact_id));
        artifacts
    }

    /// A store with the three source packs installed, and one `ERPNext`
    /// snapshot row with the mapping input it derives under.
    fn installed() -> (CellStore, RuntimeValue, MappingInput) {
        let artifacts = artifacts();
        let (mut types, mut interfaces, mut properties) = (
            Vec::<ObjectTypeDef>::new(),
            Vec::<InterfaceDef>::new(),
            Vec::<PropertyTypeDef>::new(),
        );
        for artifact in &artifacts {
            let payload = serde_json::to_value(artifact).expect("artifact")["payload"].clone();
            if let Some(value) = payload.get("objectType") {
                types.push(serde_json::from_value(value.clone()).expect("object"));
            }
            if let Some(value) = payload.get("interface") {
                interfaces.push(serde_json::from_value(value.clone()).expect("interface"));
            }
            if let Some(value) = payload.get("propertyTypes") {
                properties.extend(
                    serde_json::from_value::<Vec<PropertyTypeDef>>(value.clone())
                        .expect("properties"),
                );
            }
        }
        let model = Model::from_definitions_with_interfaces(&types, &properties, &interfaces)
            .expect("model")
            .with_governing_laws(&artifacts)
            .expect("laws");
        let mut store = CellStore::open_in_memory(model.clone()).expect("store");
        store.bind("scope", "org_demo/dev").expect("scope");
        store
            .install_definitions(
                model,
                InstalledDefinitionInput {
                    canonical_set: String::from_utf8(
                        norn_ir::canonical::canonical_json_of(&artifacts).expect("set"),
                    )
                    .expect("UTF8"),
                    registry: vec![],
                    receipt_sha256: norn_ir::canonical::domain_hash_hex(
                        b"norn.test.receipt\0",
                        &"derive-once-recheck",
                    )
                    .expect("receipt digest"),
                    admission_receipt: None,
                    readmission: None,
                    diff: serde_json::json!({}),
                    installed_at: 1_793_702_400_000,
                    layout: None,
                },
                None,
                "org_demo/dev",
            )
            .expect("install");
        let source: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(root().join("registry/packs/erpnext/synthetic.json"))
                .expect("fixture"),
        )
        .expect("json");
        let row = &source["snapshot"][0];
        let payload = RuntimeValue::from_json(row["record"].clone()).expect("payload");
        let digest = norn_ir::canonical::HashProfile::Norn1
            .hash_json(serde_json::to_string(&payload).expect("json"))
            .expect("digest");
        let raw = norn_ir::runtime_object! {
            "contract"=>"norn.raw_observation/v1", "source"=>"synthetic-erpnext", "source_id"=>"row-0",
            "event_id"=>"snapshot", "source_version"=>"0", "pipeline_version"=>"synthetic/1", "kind"=>"upsert",
            "extracted_at"=>1_793_702_400_000u64, "payload"=>payload, "payload_digest"=>digest,
        };
        let input = MappingInput {
            mapping: "erpnext.three.mapping".into(),
            dataset: row["dataset"].as_str().expect("dataset").into(),
            connector_instance: "demo".into(),
            partition: "snapshot".into(),
            recorded_at: norn_ir::provenance::UtcTimestamp("2026-11-03T00:00:00Z".into()),
        };
        (store, raw, input)
    }

    /// The remembered bundle, and the derived cache filled to capacity with
    /// it among the entries.
    fn remembered_at_capacity(
        store: &CellStore,
        raw: &RuntimeValue,
        input: &MappingInput,
    ) -> MappingBundle {
        let reader = store.read();
        let version = reader
            .active_definitions_version()
            .expect("active")
            .expect("installed");
        let bundle = reader.mapping_bundle(raw, input, version).expect("derived");
        let mut cache = store.mapping_plans.borrow_mut();
        let (plan, _) = cache
            .derived
            .get(&bundle.digest)
            .cloned()
            .expect("remembered");
        for filler in 0..MAX_DERIVED_BUNDLES - 1 {
            let mut other = bundle.clone();
            other.digest = format!("filler-{filler}");
            cache
                .derived
                .insert(other.digest.clone(), (Arc::clone(&plan), other));
        }
        assert_eq!(cache.derived.len(), MAX_DERIVED_BUNDLES);
        bundle
    }

    /// INGEST-DERIVE-ONCE-1: a recheck that hits a remembered derivation,
    /// including its debug-only cross-check, leaves the derived cache exactly
    /// as it was, even at capacity, where a remembering derivation clears it.
    #[test]
    fn a_remembered_recheck_never_touches_the_derived_cache() {
        let (store, raw, input) = installed();
        let bundle = remembered_at_capacity(&store, &raw, &input);
        let before: Vec<String> = store
            .mapping_plans
            .borrow()
            .derived
            .keys()
            .cloned()
            .collect();
        store
            .read()
            .recheck_mapping_bundle(&bundle)
            .expect("rechecked");
        let after: Vec<String> = store
            .mapping_plans
            .borrow()
            .derived
            .keys()
            .cloned()
            .collect();
        assert_eq!(after, before);
    }

    /// The debug cross-check asserts and never returns an error: a remembered
    /// bundle that differs from a fresh derivation is a broken invariant.
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "a remembered derivation differs from a fresh one")]
    fn a_remembered_bundle_unlike_its_fresh_derivation_is_an_assertion() {
        let (store, raw, input) = installed();
        let mut bundle = remembered_at_capacity(&store, &raw, &input);
        // Remember a bundle no derivation produces, under the same plan.
        bundle
            .diagnostics
            .push(RuntimeValue::from_json(serde_json::json!("forged")).expect("value"));
        {
            let mut cache = store.mapping_plans.borrow_mut();
            let (plan, _) = cache
                .derived
                .get(&bundle.digest)
                .cloned()
                .expect("remembered");
            cache
                .derived
                .insert(bundle.digest.clone(), (plan, bundle.clone()));
        }
        // Any return, an error included, fails the expected panic.
        store
            .read()
            .recheck_mapping_bundle(&bundle)
            .expect("only the assertion ends this recheck");
    }
}
