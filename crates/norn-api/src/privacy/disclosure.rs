//! Sealed, response-ready disclosure projections (B2).
//!
//! Every HTTP and MCP read route constructs its wire document through one of
//! the private-constructor `Disclosed*` types in this module. Because the inner
//! payload field is private and there is no `From<Raw>` or `DerefMut`, callers
//! and transport serializers cannot emit raw `ObjectRecord`, `QueryResult`,
//! `NumericTrace`, `InstanceSourceProvenance`, or `TransformResult` documents
//! without passing through masking, dynamic `_norn_markings`,
//! `discloses_source`, `fact_digest` redaction, keyed handles, and D53 bucket
//! suppression.
//!
//! Every `Disclosed*` wrapper uses `#[serde(transparent)]`, preserving the
//! exact wire JSON and `OpenAPI` contracts.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Deref;

use norn_cell::compute::lineage::{
    ContributionSource, NumericTrace, keyed_cursor_handle, keyed_finding_handle,
    keyed_lineage_handle, keyed_receipt_handle, keyed_revision_handle,
};
use norn_cell::compute::{HistoricalQuestionResult, QueryResult};
use norn_cell::reactive::DecodeBudget;
use norn_cell::source_provenance::InstanceSourceProvenance;
use norn_cell::system::GENESIS_HASH;
use norn_cell::{CellError, CellStore, MAIN_WORLD, ObjectRecord, StoreReader, engine::SqlEngine};
use norn_checker::metric::compute::QuestionPlan;
use norn_ir::{DataQualityFinding, FactRef, MaskPolicy, RuntimeValue};
use serde::Serialize;

use super::{
    MarkingCatalog, NornMaskMode, NornMaskedProperty, Projected, PseudonymKey, apply,
    discloses_source, grain_marked, project, project_columns, redact_trace_contributions, row_ids,
    strongest, version_markings, withhold_values,
};
use crate::host::DevHost;
use crate::object_review::{
    NornEvidenceRef, NornObjectFindings, NornObjectHistory, NornObjectVersion,
    NornQuarantinedObject, NornQuarantinedPage, NornVersionWriter,
};
use crate::objects::{
    NornAppliedQuery, NornLinkCounts, NornLinkEdge, NornObjectLinkPage, NornObjectPage,
    NornObjectQueryPage, NornObjectRecord, NornQueriedRecord, NornQueryTotal,
};
use crate::question_routes::{QuestionAnswer, QuestionRuntime, plan_sources, world_values};
use crate::refusal::Refusal;
use crate::transform_routes::{
    NornMaterialization, NornTransformColumn, NornTransformLineage, NornTransformMember,
    NornTransformPage, NornTransformRow,
};

mod sealed {
    pub trait Sealed {}
}

/// A response-ready wire document produced exclusively by
/// [`crate::privacy::disclosure`].
pub trait DisclosedDocument: Serialize + sealed::Sealed {}

/// Serialize a sealed [`DisclosedDocument`] into its wire JSON representation.
///
/// # Errors
/// A serialization failure.
pub fn serialize_disclosed(document: &impl DisclosedDocument) -> Result<String, Refusal> {
    serde_json::to_string(document).map_err(Refusal::host)
}

macro_rules! sealed_projection {
    (
        $(#[$meta:meta])*
        pub struct $name:ident($inner:ty);
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Serialize)]
        #[serde(transparent)]
        pub struct $name {
            inner: $inner,
        }

        impl $name {
            /// Read-only access to the already-projected wire document.
            #[must_use]
            pub const fn as_inner(&self) -> &$inner {
                &self.inner
            }
        }

        impl Deref for $name {
            type Target = $inner;

            fn deref(&self) -> &Self::Target {
                &self.inner
            }
        }

        impl AsRef<$inner> for $name {
            fn as_ref(&self) -> &$inner {
                &self.inner
            }
        }

        impl sealed::Sealed for $name {}
        impl DisclosedDocument for $name {}
    };
}

macro_rules! sealed_projection_eq {
    (
        $(#[$meta:meta])*
        pub struct $name:ident($inner:ty);
    ) => {
        sealed_projection!(
            $(#[$meta])*
            pub struct $name($inner);
        );

        impl PartialEq for $name {
            fn eq(&self, other: &Self) -> bool {
                self.inner == other.inner
            }
        }
    };
}

sealed_projection_eq!(
    /// One projected object record (`GET` / page item).
    pub struct DisclosedObject(NornObjectRecord);
);

sealed_projection_eq!(
    /// One projected page of objects.
    pub struct DisclosedObjectPage(NornObjectPage);
);

sealed_projection_eq!(
    /// One projected grid record (`GET /v1/objects/{type}/{id}` or grid item).
    pub struct DisclosedQueriedObject(NornQueriedRecord);
);

sealed_projection_eq!(
    /// One projected page of grid query results (`GET /v1/objects/{type}`).
    pub struct DisclosedObjectQueryPage(NornObjectQueryPage);
);

sealed_projection!(
    /// One projected object history version.
    pub struct DisclosedObjectVersion(NornObjectVersion);
);

sealed_projection!(
    /// One projected object history document (`GET /v1/objects/{type}/{id}/history`).
    pub struct DisclosedObjectHistory(NornObjectHistory);
);

sealed_projection!(
    /// One projected object findings document (`GET /v1/objects/{type}/{id}/findings`).
    pub struct DisclosedObjectFindings(NornObjectFindings);
);

sealed_projection!(
    /// One projected page of quarantined objects (`GET /v1/objects/{type}:quarantined`).
    pub struct DisclosedQuarantinedPage(NornQuarantinedPage);
);

sealed_projection_eq!(
    /// One projected page of visible links (`GET /v1/objects/{type}/{id}/links`).
    pub struct DisclosedObjectLinks(NornObjectLinkPage);
);

sealed_projection!(
    /// One projected source-lineage document (`GET /v1/objects/{type}/{id}/lineage`).
    pub struct DisclosedSourceLineage(InstanceSourceProvenance);
);

sealed_projection_eq!(
    /// One projected transform rows page (`GET /v1/transforms/{transform}/rows`).
    pub struct DisclosedTransformPage(NornTransformPage);
);

sealed_projection_eq!(
    /// One projected transform row lineage (`GET /v1/transforms/{transform}/rows/{row}/lineage`).
    pub struct DisclosedTransformLineage(NornTransformLineage);
);

/// One projected question answer (live or historical) with D53 bucket
/// suppression, keyed handles, and cutoff-receipt redaction enforced.
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct DisclosedAnswer {
    inner: QuestionAnswer,
}

impl DisclosedAnswer {
    /// Read-only access to the already-projected answer enum.
    #[must_use]
    pub const fn as_answer(&self) -> &QuestionAnswer {
        &self.inner
    }

    /// The live answer when this document is live.
    #[must_use]
    pub const fn live(&self) -> Option<&QueryResult> {
        match &self.inner {
            QuestionAnswer::Live(result) => Some(result),
            QuestionAnswer::Historical(_) => None,
        }
    }

    /// The historical answer when this document is historical.
    #[must_use]
    pub const fn historical(&self) -> Option<&HistoricalQuestionResult> {
        match &self.inner {
            QuestionAnswer::Live(_) => None,
            QuestionAnswer::Historical(historical) => Some(historical),
        }
    }
}

impl Deref for DisclosedAnswer {
    type Target = QuestionAnswer;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl AsRef<QuestionAnswer> for DisclosedAnswer {
    fn as_ref(&self) -> &QuestionAnswer {
        &self.inner
    }
}

impl sealed::Sealed for DisclosedAnswer {}
impl DisclosedDocument for DisclosedAnswer {}

/// One projected numeric trace with marked contribution redaction, `fact_digest`
/// withholding, `source = Restricted`, keyed receipt handles, and D53 bucket
/// suppression enforced.
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct DisclosedTrace {
    inner: NumericTrace,
}

impl DisclosedTrace {
    /// Read-only access to the already-projected trace.
    #[must_use]
    pub const fn as_inner(&self) -> &NumericTrace {
        &self.inner
    }
}

impl Deref for DisclosedTrace {
    type Target = NumericTrace;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl AsRef<NumericTrace> for DisclosedTrace {
    fn as_ref(&self) -> &NumericTrace {
        &self.inner
    }
}

impl sealed::Sealed for DisclosedTrace {}
impl DisclosedDocument for DisclosedTrace {}

/// A sealed grid-scan row: Population was granted, and when `readable` is true
/// its values have already passed through `privacy::project`. Withheld rows
/// expose neither values nor version.
pub(crate) struct DisclosedListed {
    row: ObjectRecord,
    readable: bool,
    projected: Projected,
}

impl DisclosedListed {
    pub(crate) fn id(&self) -> &str {
        &self.row.id
    }

    pub(crate) const fn readable(&self) -> bool {
        self.readable
    }

    /// Projected values for filtering, sorting, profiling, or faceting;
    /// always `None` when the row is withheld.
    pub(crate) const fn comparable_values(&self) -> Option<&BTreeMap<String, RuntimeValue>> {
        if self.readable {
            Some(&self.row.values)
        } else {
            None
        }
    }

    /// Convert this already-projected listed row into its sealed wire record.
    pub(crate) fn to_queried(&self) -> DisclosedQueriedObject {
        DisclosedQueriedObject {
            inner: NornQueriedRecord {
                object_type: self.row.object_type.clone(),
                id: self.row.id.clone(),
                scope: self.row.scope.clone(),
                version: self.readable.then_some(self.row.version),
                values: self.readable.then(|| self.row.values.clone()),
                withheld: !self.readable,
                withheld_properties: self.projected.withheld.clone(),
                masked: self.projected.masked.clone(),
            },
        }
    }
}

/// Non-cloneable capability token proving an export passed PRIVACY-1 /
/// MANIFEST-MARK-1 disclosure checks at the exact exported cell snapshot.
#[derive(Debug)]
pub(crate) struct DisclosedExportPermit {
    source: norn_cell::analytics::Source,
    snapshot: String,
}

impl DisclosedExportPermit {
    pub(crate) fn into_parts(self) -> (norn_cell::analytics::Source, String) {
        (self.source, self.snapshot)
    }
}

/// Project one readable `ObjectRecord` through PRIVACY-1 masking.
pub(crate) fn project_object<E: SqlEngine>(
    host: &DevHost<E>,
    actor: &str,
    catalog: &MarkingCatalog,
    markings: Option<&BTreeMap<String, Vec<String>>>,
    mut row: ObjectRecord,
) -> DisclosedObject {
    let projected = project(host, actor, catalog, markings, &mut row);
    DisclosedObject {
        inner: NornObjectRecord::projected(row, projected),
    }
}

/// Assemble a sealed page from already-projected [`DisclosedObject`] entries.
pub(crate) fn project_object_page(
    object_type: String,
    records: Vec<DisclosedObject>,
    quarantine_count: u64,
    scanned_through: Option<String>,
) -> DisclosedObjectPage {
    DisclosedObjectPage {
        inner: NornObjectPage {
            object_type,
            records: records.into_iter().map(|record| record.inner).collect(),
            quarantine_count,
            scanned_through,
        },
    }
}

/// Project one grid-scan row under its visibility (`readable`) and effective
/// markings (`None` fails closed to `Refuse` on every property when readable).
pub(crate) fn project_listed_row<E: SqlEngine>(
    host: &DevHost<E>,
    actor: &str,
    readable: bool,
    catalog: Option<&MarkingCatalog>,
    markings: Option<&BTreeMap<String, Vec<String>>>,
    mut row: ObjectRecord,
) -> DisclosedListed {
    if !readable {
        row.values.clear();
        return DisclosedListed {
            row,
            readable: false,
            projected: Projected::default(),
        };
    }
    let projected = match catalog {
        Some(catalog) => project(host, actor, catalog, markings, &mut row),
        None => project(host, actor, &super::built_in_catalog(), None, &mut row),
    };
    DisclosedListed {
        row,
        readable: true,
        projected,
    }
}

/// Project one grid record (`GET /v1/objects/{type}/{id}`).
pub(crate) fn project_queried_object<E: SqlEngine>(
    host: &DevHost<E>,
    actor: &str,
    readable: bool,
    catalog: Option<&MarkingCatalog>,
    markings: Option<&BTreeMap<String, Vec<String>>>,
    row: ObjectRecord,
) -> DisclosedQueriedObject {
    project_listed_row(host, actor, readable, catalog, markings, row).to_queried()
}

/// Project one analytics-engine dataset row under MANIFEST-MARK-1 markings.
pub(crate) fn project_dataset_queried_object(
    object_type: &str,
    scope_key: &str,
    row: norn_cell::analytics::PageRow,
    values: bool,
    catalog: &MarkingCatalog,
    marked_columns: &BTreeMap<String, Vec<String>>,
    key: Option<&PseudonymKey>,
) -> Result<DisclosedQueriedObject, Refusal> {
    let mut masked = BTreeMap::new();
    let mut withheld_properties = Vec::new();
    let read = if values {
        let mut read = row
            .values
            .iter()
            .map(|(id, value)| Ok((id.clone(), norn_cell::analytics::runtime_value(value)?)))
            .collect::<Result<BTreeMap<_, _>, norn_ir::RuntimeValueError>>()
            .map_err(Refusal::host)?;
        for (column, ids) in marked_columns {
            let mask = strongest(catalog, ids).unwrap_or(MaskPolicy::Refuse {});
            let Some(value) = read.get_mut(column) else {
                continue;
            };
            let mode = apply(&mask, key, value);
            if mode == NornMaskMode::Refuse {
                read.remove(column);
                withheld_properties.push(column.clone());
            }
            masked.insert(
                column.clone(),
                NornMaskedProperty {
                    mode,
                    markings: ids.clone(),
                    revealable: false,
                },
            );
        }
        Some(read)
    } else {
        None
    };
    Ok(DisclosedQueriedObject {
        inner: NornQueriedRecord {
            object_type: object_type.to_owned(),
            id: row.id,
            scope: scope_key.to_owned(),
            version: read.as_ref().map(|_| row.version),
            withheld: read.is_none(),
            values: read,
            withheld_properties,
            masked,
        },
    })
}

/// Assemble a sealed grid query page from already-projected records.
pub(crate) fn project_object_query_page(
    object_type: String,
    records: Vec<DisclosedQueriedObject>,
    quarantine_count: u64,
    query: NornAppliedQuery,
    next: Option<String>,
    total: Option<NornQueryTotal>,
) -> DisclosedObjectQueryPage {
    DisclosedObjectQueryPage {
        inner: NornObjectQueryPage {
            object_type,
            records: records.into_iter().map(|record| record.inner).collect(),
            quarantine_count,
            query,
            next,
            total,
        },
    }
}

/// #1175 (DATA-VALUES-1, D53): key `revision.hash` when the reader lacks
/// whole-world unmarked Values (genesis stays raw).
fn withhold_ledger_head(result: &mut QueryResult, key: &[u8], world: bool) {
    let genesis = result.revision.seq == 0 && result.revision.hash == GENESIS_HASH;
    if !(world || genesis) {
        result.revision.hash = keyed_revision_handle(key, &result.revision.hash);
    }
}

/// #1175 (DATA-VALUES-1, D53): key exclusion finding IDs when the reader lacks
/// whole-world unmarked Values.
fn withhold_exclusions(result: &mut QueryResult, key: &[u8], world: bool) {
    if !world {
        for finding in result.exclusions.values_mut() {
            *finding = keyed_finding_handle(key, finding);
        }
    }
}

/// #1175 (DATA-VALUES-1, D53): key the historical cutoff receipt coordinate
/// when the reader lacks whole-world unmarked Values.
fn withhold_cutoff_receipt(
    historical: &mut HistoricalQuestionResult,
    key: &[u8],
    world: bool,
) -> Result<(), Refusal> {
    let norn_kernel::World::Historical { commit } = &mut historical.world else {
        return Err(Refusal::from_compute(CellError::ComputeRefused(
            "core.compute.unsupported_engine",
        )));
    };
    if !world && historical.result.known_as_of != 0 {
        commit.0 = keyed_receipt_handle(key, &commit.0);
    }
    Ok(())
}

/// DATA-SOURCES-1: key raw connector cursors (`source_cursors` and
/// `watermarks.observed`) whenever the caller lacks the independent `Sources`
/// grant, preventing disclosure of upstream record IDs and versions while
/// keeping freshness, completeness, heartbeat, and cursor equality intact.
fn withhold_source_cursors(result: &mut QueryResult, key: &[u8], sources_granted: bool) {
    if sources_granted {
        return;
    }
    for partitions in result.context.source_cursors.values_mut() {
        for cursor in partitions.values_mut() {
            *cursor = keyed_cursor_handle(key, cursor);
        }
    }
    for partitions in result.context.watermarks.values_mut() {
        for watermark in partitions.values_mut() {
            watermark.observed = keyed_cursor_handle(key, &watermark.observed);
        }
    }
}

/// Project one evaluated [`QuestionAnswer`] under D53 bucket suppression,
/// keyed handles, cutoff-receipt keying, and `Sources`-gated cursor keying.
pub(crate) fn project_answer<E: SqlEngine>(
    (host, runtime, store): (&DevHost<E>, &dyn QuestionRuntime<E>, &CellStore<E>),
    (actor, key, plan): (&str, &[u8], &QuestionPlan),
    withheld: &BTreeSet<String>,
    mut answer: QuestionAnswer,
) -> Result<DisclosedAnswer, Refusal> {
    let world = withheld.is_empty() && world_values(host, runtime, store, actor);
    let sources_granted = plan_sources(host, runtime, store, actor, plan);
    let result = match &mut answer {
        QuestionAnswer::Live(result) => result,
        QuestionAnswer::Historical(historical) => {
            withhold_cutoff_receipt(historical, key, world)?;
            &mut historical.result
        }
    };
    withhold_ledger_head(result, key, world);
    withhold_exclusions(result, key, world);
    withhold_source_cursors(result, key, sources_granted);
    withhold_values(result, withheld, runtime.min_disclosed_bucket());
    result.lineage_ref = keyed_lineage_handle(key, &result.lineage_ref);
    Ok(DisclosedAnswer { inner: answer })
}

/// Project one evaluated [`NumericTrace`] under PRIVACY-1 contribution
/// redaction (`fact_digest`, `source`, projected `values`), keyed receipt,
/// cursor, and ledger handles, and D53 bucket suppression.
pub(crate) fn project_trace<E: SqlEngine>(
    (host, runtime, store): (&DevHost<E>, &dyn QuestionRuntime<E>, &CellStore<E>),
    (actor, world_name, plan): (&str, &str, &QuestionPlan),
    (key, lineage): (&[u8], &str),
    withheld: &BTreeSet<String>,
    mut trace: NumericTrace,
) -> Result<DisclosedTrace, Refusal> {
    let any_marked = redact_trace_contributions(
        host,
        (actor, world_name),
        &store.read(),
        &mut trace.contributions,
    )?;
    let world = !any_marked && withheld.is_empty() && world_values(host, runtime, store, actor);
    let sources_granted = plan_sources(host, runtime, store, actor, plan)
        && !trace
            .contributions
            .iter()
            .any(|c| matches!(c.source, ContributionSource::Restricted));
    withhold_ledger_head(&mut trace.result, key, world);
    withhold_exclusions(&mut trace.result, key, world);
    withhold_source_cursors(&mut trace.result, key, sources_granted);
    if !world {
        for receipt in trace
            .contributions
            .iter_mut()
            .filter_map(|c| c.receipt_hash.as_mut())
        {
            *receipt = keyed_receipt_handle(key, receipt);
        }
    }
    withhold_values(&mut trace.result, withheld, runtime.min_disclosed_bucket());
    lineage.clone_into(&mut trace.result.lineage_ref);
    Ok(DisclosedTrace { inner: trace })
}

/// Whether every live cell-stored row of `object_type` has a complete marking
/// snapshot with no label on any of its properties. Fails closed on unknown
/// types, incomplete snapshots, or unbounded summaries.
pub(crate) fn mirror_unlabeled<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    object_type: &str,
) -> bool {
    let Ok(properties) = reader.model().effective_property_types(object_type) else {
        return false;
    };
    properties.iter().all(|(property, _)| {
        reader
            .property_marking_summary(MAIN_WORLD, object_type, property)
            .is_ok_and(|labels| labels.is_some_and(|labels| labels.is_empty()))
    })
}

fn unlabeled<E: SqlEngine>(reader: &StoreReader<'_, E>, object_type: &str) -> bool {
    match reader.dataset_manifest(MAIN_WORLD, object_type) {
        Ok(Some(current)) => reader
            .effective_dataset_markings(&current.manifest)
            .is_ok_and(|markings| markings.is_empty()),
        Ok(None) => mirror_unlabeled(reader, object_type),
        Err(_) => false,
    }
}

/// PRIVACY-1 / MANIFEST-MARK-1: whether `object_type`'s current stored values
/// may leave unprojected at this reader's snapshot.
fn stored_values_leave<E: SqlEngine>(
    host: &DevHost<E>,
    reader: &StoreReader<'_, E>,
    object_type: &str,
) -> Result<bool, Refusal> {
    if let Some(current) = reader
        .dataset_manifest(MAIN_WORLD, object_type)
        .map_err(Refusal::host)?
        && !reader
            .effective_dataset_markings(&current.manifest)
            .map_err(Refusal::host)?
            .is_empty()
    {
        return Ok(false);
    }
    let catalog = host.marking_catalog(reader)?;
    Ok(catalog.is_empty() || (!super::declares(&catalog) && unlabeled(reader, object_type)))
}

/// Issue a sealed export permit only after bracketing `stored_values_leave`
/// and `store.analytics_source` at one cell snapshot (S3, #1547). No caller
/// can supply the decision or construct a permit directly.
pub(crate) fn admit_export<E: SqlEngine>(
    host: &DevHost<E>,
    store: &CellStore<E>,
    object_type: &str,
) -> Result<DisclosedExportPermit, Refusal> {
    let scope_key = host.scope().key();
    for _attempt in 0..3 {
        let reader = store.read();
        let start = crate::object_routes::version_of(&reader)?;
        let leave = stored_values_leave(host, &reader, object_type)?;
        let (source, snapshot) = store
            .analytics_source(MAIN_WORLD, &scope_key, object_type)
            .map_err(|error| {
                if matches!(error, CellError::ComputeRefused(_)) {
                    Refusal::from_compute(error)
                } else {
                    Refusal::host(error)
                }
            })?;
        if crate::object_routes::version_of(&reader)? == start {
            if !leave {
                return Err(crate::authz::read_denied());
            }
            return Ok(DisclosedExportPermit { source, snapshot });
        }
    }
    Err(crate::object_routes::snapshot_moved())
}

/// Read and seal one object version's source lineage, unconditionally gating
/// every retained source row through [`discloses_source`].
pub(crate) fn project_source_lineage<E: SqlEngine>(
    host: &DevHost<E>,
    reader: &StoreReader<'_, E>,
    fact: &FactRef,
    budget: &mut DecodeBudget,
) -> Result<Option<DisclosedSourceLineage>, Refusal> {
    let catalog = host.marking_catalog(reader)?;
    reader
        .instance_source_provenance(MAIN_WORLD, fact, budget, |instance| {
            discloses_source(reader, &catalog, instance)
        })
        .map_err(Refusal::host)
        .map(|opt| opt.map(|inner| DisclosedSourceLineage { inner }))
}

/// Input for projecting one readable history version.
pub(crate) struct VersionProjection<'a> {
    pub(crate) row: &'a ObjectRecord,
    pub(crate) version: u64,
    pub(crate) stored: Option<ObjectRecord>,
    pub(crate) sources: bool,
    pub(crate) receipt: &'a norn_cell::Receipt,
}

/// Project one readable history version under that exact version's markings:
/// masks/refuses marked properties, suppresses mapping lineage and action
/// writer whenever the version is marked, and checks `discloses_source` on
/// unmarked mapping lineage.
pub(crate) fn project_object_version<E: SqlEngine>(
    host: &DevHost<E>,
    reader: &StoreReader<'_, E>,
    actor: &str,
    input: VersionProjection<'_>,
    budget: &mut DecodeBudget,
    resolve_action_writer: impl FnOnce() -> Result<Option<NornVersionWriter>, Refusal>,
) -> Result<(DisclosedObjectVersion, Projected), Refusal> {
    let catalog = host.marking_catalog(reader)?;
    let mut marked = false;
    let mut projected = Projected::default();
    let is_tombstone = input.stored.is_none();
    let values = match input.stored {
        Some(mut record) => {
            let markings = version_markings(reader, &catalog, &record)?;
            marked = markings
                .as_ref()
                .is_none_or(|markings| markings.values().any(|ids| !ids.is_empty()));
            projected = project(host, actor, &catalog, markings.as_ref(), &mut record);
            Some(record.values.into_iter().collect())
        }
        None => None,
    };
    let fact = FactRef::new(input.row.object_type.as_str(), &input.row.id, input.version);
    let lineage = if is_tombstone || marked || !input.sources {
        None
    } else {
        project_source_lineage(host, reader, &fact, budget)?.map(|disclosed| disclosed.inner)
    };
    let writer = match lineage {
        _ if marked => NornVersionWriter::Other,
        Some(lineage) => NornVersionWriter::Mapping { lineage },
        None => resolve_action_writer()?.unwrap_or(NornVersionWriter::Other),
    };
    Ok((
        DisclosedObjectVersion {
            inner: NornObjectVersion {
                version: input.version,
                commit: input.receipt.body.commit_seq,
                logical_time: input.receipt.body.logical_time,
                values,
                writer,
            },
        },
        projected,
    ))
}

/// Assemble a sealed object history from already-projected versions.
pub(crate) fn project_object_history(
    object_type: String,
    id: String,
    versions: Vec<DisclosedObjectVersion>,
    truncated: bool,
) -> DisclosedObjectHistory {
    DisclosedObjectHistory {
        inner: NornObjectHistory {
            object_type,
            id,
            versions: versions.into_iter().map(|entry| entry.inner).collect(),
            truncated,
        },
    }
}

/// Assemble sealed object findings after endpoint and SOURCE-AGREE-1 gating.
pub(crate) fn project_object_findings(
    object_type: String,
    id: String,
    quarantined: bool,
    findings: Vec<DataQualityFinding>,
    evidence: Vec<NornEvidenceRef>,
) -> DisclosedObjectFindings {
    DisclosedObjectFindings {
        inner: NornObjectFindings {
            object_type,
            id,
            quarantined,
            findings,
            evidence,
        },
    }
}

/// Assemble a sealed quarantined-objects page after row & link-finding gating.
pub(crate) fn project_quarantined_page(
    object_type: String,
    items: Vec<NornQuarantinedObject>,
    approximate: bool,
    scan_limit: u64,
) -> DisclosedQuarantinedPage {
    DisclosedQuarantinedPage {
        inner: NornQuarantinedPage {
            object_type,
            items,
            approximate,
            scan_limit,
        },
    }
}

/// Assemble a sealed visible-links page after dual-endpoint & quarantine gating.
pub(crate) fn project_object_links(
    object_type: String,
    id: String,
    items: Vec<NornLinkEdge>,
    next: Option<String>,
    counts: Option<NornLinkCounts>,
) -> DisclosedObjectLinks {
    DisclosedObjectLinks {
        inner: NornObjectLinkPage {
            object_type,
            id,
            items,
            next,
            counts,
        },
    }
}

/// Resolve the public row-id key for a transform whose grain may be marked.
pub(crate) fn transform_row_id_key<E: SqlEngine>(
    host: &DevHost<E>,
    catalog: &MarkingCatalog,
    marked: &[(String, Vec<String>)],
    grain: &[String],
) -> Result<Option<PseudonymKey>, Refusal> {
    row_ids(host, catalog, grain_marked(marked, grain))
}

/// Unprojected transform page fields to seal through column masking.
pub(crate) struct TransformPageInput {
    pub(crate) transform: String,
    pub(crate) version: String,
    pub(crate) commits: u64,
    pub(crate) materialization: Option<NornMaterialization>,
    pub(crate) columns: Vec<NornTransformColumn>,
    pub(crate) grain: Vec<String>,
    pub(crate) rows: Vec<NornTransformRow>,
    pub(crate) total_rows: u64,
    pub(crate) withheld: BTreeMap<String, u64>,
    pub(crate) excluded: u64,
    pub(crate) complete: bool,
    pub(crate) node_rows: BTreeMap<String, u64>,
    pub(crate) next_after: Option<String>,
}

/// Project one page of transform rows through PRIVACY-1 column masking.
pub(crate) fn project_transform_page<E: SqlEngine>(
    host: &DevHost<E>,
    catalog: &MarkingCatalog,
    marked: &[(String, Vec<String>)],
    mut input: TransformPageInput,
) -> DisclosedTransformPage {
    project_columns(
        host,
        catalog,
        marked,
        &mut input
            .rows
            .iter_mut()
            .map(|row| &mut row.values)
            .collect::<Vec<_>>(),
    );
    DisclosedTransformPage {
        inner: NornTransformPage {
            transform: input.transform,
            version: input.version,
            engine: "cell".to_owned(),
            commits: input.commits,
            materialization: input.materialization,
            columns: input.columns,
            grain: input.grain,
            rows: input.rows,
            total_rows: input.total_rows,
            withheld: input.withheld,
            excluded: input.excluded,
            complete: input.complete,
            node_rows: input.node_rows,
            next_after: input.next_after,
        },
    }
}

/// Unprojected transform row-lineage fields to seal through column masking.
pub(crate) struct TransformLineageInput {
    pub(crate) transform: String,
    pub(crate) row_id: String,
    pub(crate) commits: u64,
    pub(crate) materialization: Option<NornMaterialization>,
    pub(crate) values: BTreeMap<String, RuntimeValue>,
    pub(crate) members: Vec<NornTransformMember>,
}

/// Project one transform row's lineage through PRIVACY-1 column masking.
pub(crate) fn project_transform_lineage<E: SqlEngine>(
    host: &DevHost<E>,
    catalog: &MarkingCatalog,
    marked: &[(String, Vec<String>)],
    mut input: TransformLineageInput,
) -> DisclosedTransformLineage {
    project_columns(host, catalog, marked, &mut [&mut input.values]);
    DisclosedTransformLineage {
        inner: NornTransformLineage {
            transform: input.transform,
            row_id: input.row_id,
            commits: input.commits,
            materialization: input.materialization,
            values: input.values,
            members: input.members,
        },
    }
}
