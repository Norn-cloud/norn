//! HOST-READ-6 (#588, #343): an object's quality findings and its version
//! history, the reads behind "why is this value what it is, and who changed it".
//!
//! Both are value-level. The findings need Population, Values and Sources on
//! the row, the same capabilities as its lineage (D67); the history needs
//! Population and Values, and names a version's source lineage only to a
//! caller who also holds Sources on that version. Unlike an authoritative read, they also
//! serve a quarantined object: OBS-EVIDENCE-1 keeps imported source truth
//! readable to those allowed to review it, and a finding without its subject
//! cannot be reviewed. A refused object refuses with the same opaque 404 as
//! lineage.
//!
//! The history lists the proven versions of the row the caller may read,
//! newest first, with the receipt that committed it and what wrote it. Each
//! version is decided on its own record, not on the current row: a version
//! the caller could not read is not listed, nor is one that changed only what
//! the caller may not see. What wrote a version is a mapping (the version's
//! source lineage, as `/lineage?version=` serves it, under that version's
//! Sources) or an
//! admitted action (the action-call record of that commit: which action and
//! which verified actor). An action's call record is disclosed under its own
//! read decision: a caller who may not read it (Population and Values on that
//! record) is told no writer. The call's effective input is never listed: the
//! kernel writes it as a plain fact, and it can carry values (a derived value,
//! a prepared input) whose disclosure the call record's read does not decide.

use std::collections::BTreeMap;

use norn_cell::reactive::{DecodeBudget, ReactiveLimits};
use norn_cell::read::LinkDirection;
use norn_cell::source_provenance::InstanceSourceProvenance;
use norn_cell::{MAIN_WORLD, ObjectRecord, StoreReader, engine::SqlEngine};
use norn_ir::{DataQualityFinding, FactRef, RuntimeValue};
use serde::Serialize;
use utoipa::ToSchema;
use utoipa::openapi::Ref;
use utoipa::openapi::schema::{
    AdditionalProperties, ArrayBuilder, Object, ObjectBuilder, SchemaType,
};

use crate::authz::ReadCapability::{Population, Sources, Values};
use crate::host::DevHost;
use crate::object_links::{GatedFinding, disclosed_findings, gate_findings};
use crate::object_routes::{
    MAX_QUERY_SCAN, ObjectAccess, SCAN_BATCH, collection, locked, not_found, permitted,
    question_sources_permitted,
};
use crate::privacy::{
    DisclosedObjectFindings, DisclosedObjectHistory, DisclosedObjectVersion,
    DisclosedQuarantinedPage, DisclosedSourceLineage,
    disclosure::{
        VersionProjection, project_object_findings, project_object_history, project_object_version,
        project_quarantined_page, project_source_lineage,
    },
};
use crate::refusal::Refusal;
use crate::scope::ScopeQuery;

/// Most versions one history lists, newest first.
pub const MAX_HISTORY_VERSIONS: u64 = 64;
/// Most raw versions scanned backward to establish visible changes and skip
/// hidden-only writes before declaring the history truncated (#1534 S7).
const MAX_HISTORY_SCAN: u64 = 1_024;
/// Most evidence links one findings read lists.
pub const MAX_EVIDENCE: usize = 200;
/// The core interface of evidence objects (`registry/core/src/evidence.ts`).
pub const SOURCE_EVIDENCE: &str = "norn.SourceEvidence";

/// One evidence object linked to the subject: where one of its values was read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornEvidenceRef {
    /// The evidence object's type (it implements `norn.SourceEvidence`).
    pub object_type: String,
    /// The evidence object's id.
    pub id: String,
}

/// An object's quality findings (OBS-EVIDENCE-1), open and resolved.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornObjectFindings {
    /// The concrete object type.
    pub object_type: String,
    /// The object id.
    pub id: String,
    /// Whether an open finding holds the object out of authoritative reads.
    pub quarantined: bool,
    /// Every finding on the object, each pinned to the rule it records; a
    /// correction resolves a finding without rewriting it.
    #[schema(schema_with = findings_schema)]
    pub findings: Vec<DataQualityFinding>,
    /// The evidence objects linked to it that the caller may read (types
    /// implementing `norn.SourceEvidence`), at most [`MAX_EVIDENCE`]. They are
    /// read at `/v1/objects/{type}/{id}`; a quarantined subject's evidence is
    /// not quarantined with it.
    pub evidence: Vec<NornEvidenceRef>,
}

/// What wrote one version.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum NornVersionWriter {
    /// A mapping produced the version from a source record.
    #[serde(rename_all = "camelCase")]
    Mapping {
        /// The version's source lineage (`/lineage?version=`).
        #[schema(schema_with = lineage_schema)]
        lineage: InstanceSourceProvenance,
    },
    /// An admitted action committed the version.
    #[serde(rename_all = "camelCase")]
    Action {
        /// The action identity.
        action_id: String,
        /// The verified caller the server attributed the call to.
        actor: String,
        /// Deprecated: always `{}`. The call's effective input is never
        /// disclosed (see the module docs); the property stays, empty, so the
        /// frozen contract keeps its shape.
        #[schema(schema_with = withheld_input_schema)]
        input: WithheldInput,
        /// Optional provenance trace context recorded on the action call.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schema(nullable = false)]
        trace: Option<crate::actions::TraceContext>,
    },
    /// Another writer (an import, a migration or a kernel rule), or one this
    /// caller is not told: an action whose call record the caller may not
    /// read, a mapping whose lineage the caller may not read (no Sources), or
    /// a marked version.
    Other,
}

/// The action writer's deprecated `input`: it serializes as the empty object
/// and carries nothing, so no call input can reach a history reader.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WithheldInput;

impl Serialize for WithheldInput {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        serializer.serialize_map(Some(0))?.end()
    }
}

/// One proven version of an object.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornObjectVersion {
    /// The version number.
    pub version: u64,
    /// The commit (ledger receipt sequence) that wrote it.
    pub commit: u64,
    /// The commit's logical time, in milliseconds.
    pub logical_time: u64,
    /// Every stored property by property id; null for a tombstone.
    #[schema(schema_with = values_schema)]
    pub values: Option<BTreeMap<String, RuntimeValue>>,
    /// What wrote it.
    pub writer: NornVersionWriter,
}

/// An object's versions, newest first.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornObjectHistory {
    /// The concrete object type.
    pub object_type: String,
    /// The object id.
    pub id: String,
    /// The versions the caller may read, newest first. A version is listed
    /// only when the caller holds Population and Values on that version's own
    /// record, and only when it changed a value the caller may see.
    pub versions: Vec<NornObjectVersion>,
    /// True when older versions exist beyond [`MAX_HISTORY_VERSIONS`].
    pub truncated: bool,
}

// `schema_with` replaces the field's schema, doc comment included, so each
// helper restates its field's description.
fn findings_schema() -> utoipa::openapi::schema::Array {
    ArrayBuilder::new()
        .items(Ref::from_schema_name("NornIrDataQualityFinding"))
        .description(Some(
            "Every finding on the object, each pinned to the rule it records; a correction resolves a finding without rewriting it.",
        ))
        .build()
}

fn lineage_schema() -> Ref {
    Ref::builder()
        .ref_location_from_schema_name("NornInstanceSourceProvenance")
        .description(Some("The version's source lineage (`/lineage?version=`)."))
        .build()
}

fn withheld_input_schema() -> Object {
    ObjectBuilder::new()
        .schema_type(SchemaType::Type(utoipa::openapi::Type::Object))
        .additional_properties(Some(AdditionalProperties::FreeForm(true)))
        .deprecated(Some(utoipa::openapi::Deprecated::True))
        .description(Some(
            "Deprecated: always the empty object. The call's effective input is never disclosed.",
        ))
        .build()
}

fn values_schema() -> Object {
    crate::objects::free_form(
        "Every stored property by property id, null for a tombstone; decimals are exact JSON number tokens.",
        true,
    )
}

/// The raw row, when the caller holds Population, Values and Sources on it.
/// A quarantined row is served: review reads keep source truth.
fn reviewable<E: SqlEngine>(
    host: &DevHost<E>,
    access: &dyn ObjectAccess,
    actor: &str,
    object: (&str, &str),
    start: &mut Option<(u64, Option<(u64, u64)>)>,
) -> Result<ObjectRecord, Refusal> {
    let (row, sources) = raw_row(host, access, object, start)?;
    if permitted(host, access, actor, &row, Sources) {
        readable(host, access, actor, row, &sources)
    } else {
        Err(not_found())
    }
}

/// `row`, when the caller holds Population and Values on it (and on any
/// `norn.question@1` source types persisted into a Run object); otherwise the
/// opaque 404.
fn readable<E: SqlEngine>(
    host: &DevHost<E>,
    access: &dyn ObjectAccess,
    actor: &str,
    row: ObjectRecord,
    sources: &[String],
) -> Result<ObjectRecord, Refusal> {
    if [Population, Values].into_iter().all(|capability| {
        permitted(host, access, actor, &row, capability)
            && question_sources_permitted(host, access, actor, sources, capability)
    }) {
        Ok(row)
    } else {
        Err(not_found())
    }
}

/// The raw row, quarantined or not, before any read decision, together with
/// any persisted `norn.question@1` source object types recorded on it.
fn raw_row<E: SqlEngine>(
    host: &DevHost<E>,
    access: &dyn ObjectAccess,
    (object_type, id): (&str, &str),
    start: &mut Option<(u64, Option<(u64, u64)>)>,
) -> Result<(ObjectRecord, Vec<String>), Refusal> {
    locked(host, access, start, |reader| {
        if !reader.model().has_type(object_type) {
            return Err(not_found());
        }
        let Some(row) = reader
            .object(MAIN_WORLD, object_type, id)
            .map_err(Refusal::host)?
        else {
            return Ok(None);
        };
        let sources = reader
            .run_question_source_types(MAIN_WORLD, &row.object_type, &row.id)
            .map_err(Refusal::host)?;
        Ok(Some((row, sources)))
    })?
    .ok_or_else(not_found)
}

/// HOST-READ-6: an object's quality findings, including while it is quarantined.
///
/// # Errors
/// A denied gate (the opaque 403); an absent, withheld or source-withheld
/// object (one 404); a commit between the reads (409 `object.snapshot_moved`);
/// or a host failure.
#[utoipa::path(get, path = "/v1/objects/{type}/{id}/findings", operation_id = "get_object_findings", params(ScopeQuery, ("type" = String, Path, description = "The object type (concept) name; percent-escapes are decoded."), ("id" = String, Path, description = "The object identity; percent-escapes are decoded.")), responses((status = 200, description = "The object's quality findings, open and resolved, and whether it is quarantined.", body = NornObjectFindings), (status = 400, description = "The request was refused as invalid (`request.invalid`): an unknown or repeated query parameter, a query over the host bound, or a path segment that is not UTF-8 once decoded.", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied, with the same document whatever the reason.", body = Refusal), (status = 404, description = "The request was refused (`object.not_found`): the object is absent, withheld or source-withheld; these are not told apart. A quarantined object is served.", body = Refusal), (status = 409, description = "The request was refused (`object.snapshot_moved`, retryable): the cell committed between the reads.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal), (status = 503, description = "The request was refused (`object.snapshot_unavailable`): the engine cannot pin one snapshot for the read. The shipped engines do not return it.", body = Refusal)))]
pub fn get_object_findings<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    object_type: &str,
    id: &str,
    access: &dyn ObjectAccess,
) -> Result<DisclosedObjectFindings, Refusal> {
    let (_epoch, actor) = collection(host, scope, actor, object_type, access)?;
    let mut start = None;
    let row = reviewable(host, access, &actor, (object_type, id), &mut start)?;
    let (quarantined, linked, disputes, gated) = locked(host, access, &mut start, |reader| {
        let page = reader
            .links_touching_page(
                MAIN_WORLD,
                &row.object_type,
                &row.id,
                LinkDirection::In,
                None,
                None,
                MAX_EVIDENCE,
            )
            .map_err(Refusal::host)?;
        let linked: Vec<(String, String)> = page
            .items
            .into_iter()
            .filter(|edge| reader.model().is_a(&edge.edge.source_type, SOURCE_EVIDENCE))
            .map(|edge| (edge.edge.source_type, edge.edge.source))
            .collect();
        Ok((
            reader
                .observation_is_quarantined(MAIN_WORLD, &row)
                .map_err(Refusal::host)?,
            linked,
            reader
                .source_disagreements(MAIN_WORLD, &row.object_type, &row.id, None)
                .map_err(Refusal::host)?,
            gate_findings(
                reader,
                reader
                    .observation_findings(MAIN_WORLD, &row.object_type, &row.id)
                    .map_err(Refusal::host)?,
            )
            .map_err(Refusal::host)?,
        ))
    })?;
    // OBS-LINK-DISPUTE-1: a held link is disclosed as the link would be,
    // at the snapshot the findings were read at.
    let mut findings: Vec<DataQualityFinding> =
        disclosed_findings(host, access, &actor, &mut start, vec![((), gated)])?
            .into_iter()
            .flat_map(|((), findings)| findings)
            .collect();
    let mut evidence = Vec::new();
    // Each evidence row is disclosed on its own Population and Values.
    for (object_type, id) in linked {
        let item = locked(host, access, &mut start, |reader| {
            reader
                .object(MAIN_WORLD, &object_type, &id)
                .map_err(Refusal::host)
        })?;
        if let Some(item) = item
            && [Population, Values]
                .into_iter()
                .all(|capability| permitted(host, access, &actor, &item, capability))
        {
            evidence.push(NornEvidenceRef { object_type, id });
        }
    }
    // SOURCE-AGREE-1: a disagreement is disclosed only where the caller may
    // read both versions it names: their values and their sources.
    for disputed in disputes {
        if [&disputed.earlier, &disputed.later]
            .into_iter()
            .all(|version| {
                [Population, Values, Sources]
                    .into_iter()
                    .all(|capability| permitted(host, access, &actor, version, capability))
            })
        {
            findings.push(disputed.finding);
        }
    }
    Ok(project_object_findings(
        row.object_type,
        row.id,
        quarantined,
        findings,
        evidence,
    ))
}

fn budget() -> DecodeBudget {
    DecodeBudget::new(&ReactiveLimits::from_profile(
        &norn_ir::ResourceLimitProfile::norn_limits_v1(),
    ))
}

/// The action-call record written in `commit`, if an action wrote it and
/// the caller may read it.
///
/// The writer's action and actor are the call record's own values, so they
/// are disclosed only under the read decision that covers that record:
/// Population and Values on it, unmarked. Otherwise the version names no
/// writer ([`NornVersionWriter::Other`]). The call's effective input is never
/// disclosed here: the kernel stores it as one plain value even when
/// preparation or a derivation put restricted values in it, so no read of the
/// call record establishes its disclosure.
fn action_writer<E: SqlEngine>(
    (host, access, actor): (&DevHost<E>, &dyn ObjectAccess, &str),
    reader: &StoreReader<'_, E>,
    receipt: &norn_cell::Receipt,
) -> Result<Option<NornVersionWriter>, Refusal> {
    let Some(call) = receipt
        .body
        .written
        .iter()
        .find(|row| row.object_type == norn_kernel::ACTION_CALL_TYPE)
    else {
        return Ok(None);
    };
    let Some(record) = reader
        .object_at_version(MAIN_WORLD, &call.object_type, &call.key, call.version)
        .map_err(Refusal::host)?
    else {
        return Ok(None);
    };
    let disclosed = [Population, Values]
        .into_iter()
        .all(|capability| permitted(host, access, actor, &record, capability));
    if !disclosed {
        return Ok(None);
    }
    let catalog = host.marking_catalog(reader)?;
    let marked = crate::privacy::version_markings(reader, &catalog, &record)?
        .is_none_or(|markings| markings.values().any(|ids| !ids.is_empty()));
    if marked {
        return Ok(None);
    }
    let text = |name: &str| {
        record
            .values
            .get(name)
            .and_then(RuntimeValue::as_str)
            .map(str::to_owned)
    };
    let (Some(action_id), Some(actor)) = (text("actionId"), text("actor")) else {
        return Ok(None);
    };
    let Ok(trace) = decode_writer_trace(&record.values) else {
        return Ok(None);
    };
    Ok(Some(NornVersionWriter::Action {
        action_id,
        actor,
        input: WithheldInput,
        trace,
    }))
}

fn decode_writer_trace(
    values: &BTreeMap<String, RuntimeValue>,
) -> Result<Option<crate::actions::TraceContext>, ()> {
    match values.get("trace") {
        None | Some(RuntimeValue::Null) => Ok(None),
        Some(value) => {
            let decoded: norn_ir::TraceContext =
                norn_ir::runtime::decode(value).map_err(|_decode| ())?;
            decoded.validate().map_err(|_invalid| ())?;
            Ok(Some(crate::actions::TraceContext::from(decoded)))
        }
    }
}

/// The read decision over one version: the version's own record (the row
/// it deletes, for a tombstone) holds Population and Values for the caller,
/// and whether it also holds Sources. `None` withholds the whole version.
///
/// Each version is decided on its own row, never on the current one: a
/// permission field the row once held is the one its earlier values were
/// written under, and a grant made later does not open them.
fn version_read<E: SqlEngine>(
    (host, access, actor): (&DevHost<E>, &dyn ObjectAccess, &str),
    reader: &StoreReader<'_, E>,
    row: &ObjectRecord,
    version: u64,
) -> Result<Option<(Option<ObjectRecord>, bool)>, Refusal> {
    let stored = reader
        .object_at_version(MAIN_WORLD, &row.object_type, &row.id, version)
        .map_err(Refusal::host)?;
    let decided_on = match (&stored, version.checked_sub(1).filter(|before| *before > 0)) {
        (Some(record), _) => Some(record.clone()),
        (None, Some(before)) => reader
            .object_at_version(MAIN_WORLD, &row.object_type, &row.id, before)
            .map_err(Refusal::host)?,
        (None, None) => None,
    };
    let Some(decided_on) = decided_on else {
        return Ok(None);
    };
    let readable = [Population, Values]
        .into_iter()
        .all(|capability| permitted(host, access, actor, &decided_on, capability));
    Ok(readable.then(|| (stored, permitted(host, access, actor, &decided_on, Sources))))
}

/// Disclosure-relevant state of one readable version (#1534 S6): two
/// consecutive readable versions collapse only when all three match:
/// - `values`: projected property map (`BTreeMap`, property order immaterial),
///   or `None` for a tombstone.
/// - `writer_json`: canonical serialization of the disclosed [`NornVersionWriter`]
///   (including disclosed action id/actor/trace and disclosed mapping lineage).
/// - `projected`: [`crate::privacy::Projected`] marking metadata (mask mode,
///   effective markings, revealability, and withheld properties), preserved
///   even though history entries do not serialize marking metadata separately,
///   so marking changes with equal projected values are not over-merged.
#[derive(PartialEq)]
struct VisibleSignature {
    values: Option<BTreeMap<String, RuntimeValue>>,
    writer_json: String,
    projected: crate::privacy::Projected,
}

/// One version with its receipt, writer, and disclosure-relevant signature, or
/// `None` when the caller may not read it ([`version_read`]); its source
/// lineage only under that version's Sources (skipped for tombstones, which
/// carry no live instance provenance).
fn version_entry<E: SqlEngine>(
    (host, access, actor): (&DevHost<E>, &dyn ObjectAccess, &str),
    reader: &StoreReader<'_, E>,
    row: &ObjectRecord,
    version: u64,
) -> Result<Option<(DisclosedObjectVersion, VisibleSignature)>, Refusal> {
    let Some((stored, sources)) = version_read((host, access, actor), reader, row, version)? else {
        return Ok(None);
    };
    let receipt = reader
        .receipt_for_fact_version(MAIN_WORLD, &row.object_type, &row.id, version)
        .map_err(Refusal::host)?;
    let mut decode_budget = budget();
    let (entry, projected) = project_object_version(
        host,
        reader,
        actor,
        VersionProjection {
            row,
            version,
            stored,
            sources,
            receipt: &receipt,
        },
        &mut decode_budget,
        || action_writer((host, access, actor), reader, &receipt),
    )?;
    let writer_json = serde_json::to_string(&entry.writer).map_err(Refusal::host)?;
    let signature = VisibleSignature {
        values: entry.values.clone(),
        writer_json,
        projected,
    };
    Ok(Some((entry, signature)))
}

/// Walk backward from `head_version` down to `1` (bounded by `scan_limit`) to
/// collapse consecutive equal-signature runs onto their earliest version and
/// suppress any boundary run whose predecessor could not be inspected (#1534
/// S6, S7).
fn collect_history_versions<V>(
    head_version: u64,
    max_versions: usize,
    scan_limit: u64,
    mut load_entry: impl FnMut(u64) -> Result<Option<(V, VisibleSignature)>, Refusal>,
) -> Result<(Vec<V>, bool), Refusal> {
    let mut versions: Vec<V> = Vec::new();
    let mut current_run: Option<(V, VisibleSignature)> = None;
    let mut truncated = false;
    let mut scanned = 0_u64;
    let mut version = head_version;
    while version >= 1 {
        if scanned >= scan_limit {
            // Stopped before version 1 without establishing whether
            // `current_run` changed anything relative to `version`; fail
            // closed by suppressing the unverified boundary run.
            current_run = None;
            truncated = true;
            break;
        }
        scanned = scanned.saturating_add(1);
        if let Some((entry, signature)) = load_entry(version)? {
            match &mut current_run {
                Some((earliest, run_sig)) if *run_sig == signature => {
                    *earliest = entry;
                }
                Some(_) => {
                    if let Some((earliest, _)) = current_run.replace((entry, signature)) {
                        versions.push(earliest);
                    }
                    if versions.len() >= max_versions {
                        current_run = None;
                        truncated = true;
                        break;
                    }
                }
                None => {
                    current_run = Some((entry, signature));
                }
            }
        }
        if version == 1 {
            break;
        }
        version = version.saturating_sub(1);
    }
    if let Some((earliest, _)) = current_run {
        if versions.len() < max_versions {
            versions.push(earliest);
        } else {
            truncated = true;
        }
    }
    Ok((versions, truncated))
}

/// HOST-READ-6: an object's proven versions, newest first, with what wrote each.
///
/// # Errors
/// A denied gate (the opaque 403); an absent or withheld object (one 404;
/// Sources is not needed, and gates only each version's lineage); a commit
/// between the reads (409 `object.snapshot_moved`); or a host failure.
#[utoipa::path(get, path = "/v1/objects/{type}/{id}/history", operation_id = "get_object_history", params(ScopeQuery, ("type" = String, Path, description = "The object type (concept) name; percent-escapes are decoded."), ("id" = String, Path, description = "The object identity; percent-escapes are decoded.")), responses((status = 200, description = "The object's proven versions the caller may read, newest first, each with its commit and what wrote it.", body = NornObjectHistory), (status = 400, description = "The request was refused as invalid (`request.invalid`): an unknown or repeated query parameter, a query over the host bound, or a path segment that is not UTF-8 once decoded.", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied, with the same document whatever the reason.", body = Refusal), (status = 404, description = "The request was refused (`object.not_found`): the object is absent or withheld (no Population or Values); these are not told apart. Sources is not needed: without it no version names its source lineage. A quarantined object is served.", body = Refusal), (status = 409, description = "The request was refused (`object.snapshot_moved`, retryable): the cell committed between the reads.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal), (status = 503, description = "The request was refused (`object.snapshot_unavailable`): the engine cannot pin one snapshot for the read. The shipped engines do not return it.", body = Refusal)))]
pub fn get_object_history<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    object_type: &str,
    id: &str,
    access: &dyn ObjectAccess,
) -> Result<DisclosedObjectHistory, Refusal> {
    let (_epoch, actor) = collection(host, scope, actor, object_type, access)?;
    let mut start = None;
    // The history is the row's values over time: Population and Values on the
    // current row to read it at all, then each version's own decision.
    let (row, sources) = raw_row(host, access, (object_type, id), &mut start)?;
    let row = readable(host, access, &actor, row, &sources)?;
    let max_versions = usize::try_from(MAX_HISTORY_VERSIONS).unwrap_or(usize::MAX);
    locked(host, access, &mut start, |reader| {
        let (versions, truncated) =
            collect_history_versions(row.version, max_versions, MAX_HISTORY_SCAN, |version| {
                version_entry((host, access, &actor), reader, &row, version)
            })?;
        Ok(project_object_history(
            row.object_type.clone(),
            row.id.clone(),
            versions,
            truncated,
        ))
    })
}

/// One quarantined object the caller may review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornQuarantinedObject {
    /// The object id.
    pub id: String,
    /// Open findings that hold it out of authoritative reads.
    pub open_findings: u64,
}

/// A type's quarantined objects, as the caller may see them.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornQuarantinedPage {
    /// The concept read.
    pub object_type: String,
    /// Quarantined rows whose values the caller may read, in id order. A row
    /// whose values are withheld is never named: quarantine is value-level.
    pub items: Vec<NornQuarantinedObject>,
    /// True when the scan bound was reached: the list covers only the rows read.
    pub approximate: bool,
    /// The row scan bound.
    pub scan_limit: u64,
}

/// HOST-READ-6: the ids of a type's quarantined objects the caller may review,
/// so a grid's quarantine count can lead to them.
///
/// # Errors
/// A denied gate (the opaque 403), an undeclared type (404), a commit while
/// the rows were read (409 `object.snapshot_moved`), or a host failure.
#[utoipa::path(get, path = "/v1/objects/{type}:quarantined", operation_id = "get_quarantined_objects", params(ScopeQuery, ("type" = String, Path, description = "The object type (concept) name; percent-escapes are decoded.")), responses((status = 200, description = "The type's quarantined objects whose values the caller may read, with their open finding counts.", body = NornQuarantinedPage), (status = 400, description = "The request was refused as invalid (`request.invalid`): an unknown or repeated query parameter, or a path segment that is not UTF-8 once decoded.", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied, with the same document whatever the reason.", body = Refusal), (status = 404, description = "The request was refused (`object.not_found`): the type is not declared.", body = Refusal), (status = 409, description = "The request was refused (`object.snapshot_moved`, retryable): the cell committed while the rows were read.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal), (status = 503, description = "The request was refused (`object.snapshot_unavailable`): the engine cannot pin one snapshot for the read. The shipped engines do not return it.", body = Refusal)))]
pub fn get_quarantined_objects<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    object_type: &str,
    access: &dyn ObjectAccess,
) -> Result<DisclosedQuarantinedPage, Refusal> {
    let (_epoch, actor) = collection(host, scope, actor, object_type, access)?;
    let mut start = None;
    let mut after: Option<String> = None;
    let mut scanned = 0usize;
    let mut candidates: Vec<(ObjectRecord, Vec<GatedFinding>)> = Vec::new();
    let exhausted = loop {
        let (batch, done) = locked(host, access, &mut start, |reader| {
            if !reader.model().has_type(object_type) {
                return Err(not_found());
            }
            let rows = reader
                .list_after(MAIN_WORLD, object_type, after.as_deref(), SCAN_BATCH)
                .map_err(Refusal::host)?;
            let done = rows.len() < SCAN_BATCH;
            let mut held = Vec::new();
            for row in rows {
                if reader
                    .observation_is_quarantined(MAIN_WORLD, &row)
                    .map_err(Refusal::host)?
                {
                    let open: Vec<DataQualityFinding> = reader
                        .observation_findings(MAIN_WORLD, &row.object_type, &row.id)
                        .map_err(Refusal::host)?
                        .into_iter()
                        .filter(|finding| finding.status == norn_ir::DataQualityFindingStatus::Open)
                        .collect();
                    held.push((row, gate_findings(reader, open).map_err(Refusal::host)?));
                } else {
                    held.push((row, Vec::new()));
                }
            }
            Ok((held, done))
        })?;
        scanned = scanned.saturating_add(batch.len());
        after = batch.last().map(|(row, _)| row.id.clone());
        candidates.extend(batch.into_iter().filter(|(_, open)| !open.is_empty()));
        if done {
            break true;
        }
        if scanned >= MAX_QUERY_SCAN {
            break false;
        }
    };
    // Authorization runs outside the cell lock, row by row.
    let readable: Vec<_> = candidates
        .into_iter()
        .filter(|(row, _)| {
            [Population, Values]
                .into_iter()
                .all(|capability| permitted(host, access, &actor, row, capability))
        })
        .collect();
    // OBS-LINK-DISPUTE-1: a held link counts only where it is disclosed, at
    // the snapshot the rows were read at.
    let items = disclosed_findings(host, access, &actor, &mut start, readable)?
        .into_iter()
        .map(|(row, open)| NornQuarantinedObject {
            id: row.id,
            open_findings: u64::try_from(open.len()).unwrap_or(u64::MAX),
        })
        .collect();
    Ok(project_quarantined_page(
        object_type.to_owned(),
        items,
        !exhausted,
        u64::try_from(MAX_QUERY_SCAN).unwrap_or(u64::MAX),
    ))
}

/// HOST-READ-6: where one version of an object came from, by version.
///
/// # Errors
/// As [`get_object_findings`]; a version that a mapping did not write, or
/// that does not exist, is the same 404.
pub fn get_object_lineage_at<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    (object_type, id, version): (&str, &str, u64),
    access: &dyn ObjectAccess,
) -> Result<DisclosedSourceLineage, Refusal> {
    let (_epoch, actor) = collection(host, scope, actor, object_type, access)?;
    let mut start = None;
    let row = reviewable(host, access, &actor, (object_type, id), &mut start)?;
    if version == 0 || version > row.version {
        return Err(not_found());
    }
    let fact = FactRef::new(row.object_type.as_str(), &row.id, version);
    locked(host, access, &mut start, |reader| {
        project_source_lineage(host, reader, &fact, &mut budget())?.ok_or_else(not_found)
    })
}

#[cfg(test)]
mod trace_tests {
    use super::*;

    #[test]
    fn decode_writer_trace_fails_closed_on_corrupt_persisted_trace() -> Result<(), String> {
        let empty = BTreeMap::new();
        assert_eq!(decode_writer_trace(&empty), Ok(None));

        let explicit_null = BTreeMap::from([("trace".to_owned(), RuntimeValue::Null)]);
        assert_eq!(decode_writer_trace(&explicit_null), Ok(None));

        let valid_value =
            RuntimeValue::from_json_text(r#"{"traceId":"tr-1","intentRef":"goal:1"}"#)
                .map_err(|error| error.to_string())?;
        let valid = BTreeMap::from([("trace".to_owned(), valid_value)]);
        assert_eq!(
            decode_writer_trace(&valid),
            Ok(Some(crate::actions::TraceContext {
                trace_id: "tr-1".to_owned(),
                intent_ref: Some("goal:1".to_owned()),
                session_digest: None,
            }))
        );

        for bad in [
            r#"["tr-1"]"#,
            r#"{"traceId":""}"#,
            r#"{"traceId":"tr\u202e1"}"#,
            r#"{"traceId":"tr-1","sessionDigest":"raw transcript"}"#,
            r#"{"traceId":"tr-1","unknown":"x"}"#,
        ] {
            let value = RuntimeValue::from_json_text(bad).map_err(|error| error.to_string())?;
            let map = BTreeMap::from([("trace".to_owned(), value)]);
            assert_eq!(
                decode_writer_trace(&map),
                Err(()),
                "must fail closed on {bad}"
            );
        }
        Ok(())
    }

    #[test]
    fn collect_history_suppresses_unverified_boundary_run_when_scan_limit_hits()
    -> Result<(), String> {
        let make = |version: u64, name: &str| {
            let values = Some(BTreeMap::from([(
                "name".to_owned(),
                RuntimeValue::from(name),
            )]));
            let sig = VisibleSignature {
                values: values.clone(),
                writer_json: String::from(r#"{"kind":"other"}"#),
                projected: crate::privacy::Projected::default(),
            };
            let entry = NornObjectVersion {
                version,
                commit: version,
                logical_time: version.saturating_mul(10),
                values,
                writer: NornVersionWriter::Other,
            };
            (entry, sig)
        };
        // 5 versions with identical visible state (`v1`..`v5`) and scan_limit = 4:
        // scanning `v5..=v2` cannot inspect `v1`, so `v2` is suppressed (never
        // emitted as a false visible change) and `truncated` is true.
        let (versions, truncated) =
            collect_history_versions(5, 64, 4, |version| Ok(Some(make(version, "Ada"))))
                .map_err(|err| format!("{err:?}"))?;
        assert!(versions.is_empty(), "{versions:?}");
        assert!(truncated);

        // When `v5` visibly changed relative to `v4..=v1` and scan_limit = 4:
        // `v5` IS proven to differ from `v4`, while the unverified `v4..=v2` run
        // is suppressed at the scan cap with `truncated = true`.
        let (versions, truncated) = collect_history_versions(5, 64, 4, |version| {
            let name = if version == 5 { "Ada B." } else { "Ada" };
            Ok(Some(make(version, name)))
        })
        .map_err(|err| format!("{err:?}"))?;
        assert_eq!(
            versions.iter().map(|v| v.version).collect::<Vec<_>>(),
            vec![5]
        );
        assert!(truncated);
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    struct AllowAll;

    #[cfg(not(target_arch = "wasm32"))]
    impl crate::authz::Authorizer for AllowAll {
        fn authorize(&self, _: &crate::authz::AccessRequest<'_>) -> Result<(), Refusal> {
            Ok(())
        }
        fn read(&self, _: &crate::authz::ReadRequest<'_>) -> Result<(), Refusal> {
            Ok(())
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    struct TestAccess;

    #[cfg(not(target_arch = "wasm32"))]
    impl ObjectAccess for TestAccess {
        fn access(
            &self,
            scope: &crate::scope::CellScope,
            actor: &str,
            capability: crate::authz::ReadCapability,
            object_type: &str,
            row: Option<&ObjectRecord>,
        ) -> Result<norn_kernel::authz::AccessRequest, Refusal> {
            let key = norn_kernel::authz::ScopeKey {
                org: scope.org_id.clone(),
                environment: Some(scope.environment_id.clone()),
                tenant: None,
                workspace: None,
                business_unit: None,
            };
            Ok(norn_kernel::authz::AccessRequest::new(
                &norn_kernel::authz::VerifiedPrincipal::new(
                    actor,
                    norn_kernel::authz::ActorKind::User,
                    None,
                    vec![],
                    BTreeMap::new(),
                ),
                Some(key.clone()),
                format!("test.{capability:?}"),
                "norn.read",
                norn_kernel::authz::ResourceInput {
                    object_type: row.map_or(object_type, |r| r.object_type.as_str()).into(),
                    id: row.map_or(object_type, |r| r.id.as_str()).into(),
                    scope: key,
                    attributes: BTreeMap::new(),
                },
                BTreeMap::new(),
                norn_kernel::authz::Binding::Unbound(
                    norn_kernel::authz::UnboundProfile::HostedRead,
                ),
            ))
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn version_entry_serves_readable_tombstone_without_source_provenance_error()
    -> Result<(), String> {
        use crate::scope::CellScope;
        use norn_cell::engine::NativeEngine;
        use norn_cell::{CellStore, Model, SqliteCell, StoreOptions};
        use norn_kernel::{
            CellCommit, CellHost as _, FactKey, FactMutation, FactVersion, FactWrite, LogicalTime,
        };

        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let path = dir.path().join("cell.sqlite");
        let item: norn_ir::ObjectTypeDef = serde_json::from_value(serde_json::json!({
            "name": "shop.Item",
            "properties": [{"id": "name", "type": {"kind": "text"}}]
        }))
        .map_err(|e| e.to_string())?;
        let model = Model::from_definitions(&[item], &[]).map_err(|e| e.to_string())?;
        let scope = CellScope {
            org_id: "org_a".into(),
            environment_id: "dev".into(),
        };
        let store =
            CellStore::open(&path, model, StoreOptions::default()).map_err(|e| e.to_string())?;
        let mut cell: SqliteCell<
            RuntimeValue,
            norn_kernel::cell_host::JsonObject,
            norn_kernel::cell_host::JsonObject,
            NativeEngine,
        > = SqliteCell::new(
            store,
            scope.key(),
            |command: &norn_kernel::Command<norn_kernel::cell_host::JsonObject>| {
                command.payload.clone()
            },
        )
        .map_err(|e| format!("{e:?}"))?;
        cell.advance_to(LogicalTime::from_millis(2));
        cell.commit(CellCommit {
            writes: vec![FactWrite {
                key: FactKey::new("shop.Item", "i1"),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: BTreeMap::from([("name".to_owned(), RuntimeValue::from("Lathe"))]),
                },
            }],
            ..CellCommit::empty()
        })
        .map_err(|e| format!("{e:?}"))?;
        cell.advance_to(LogicalTime::from_millis(3));
        cell.commit(CellCommit {
            writes: vec![FactWrite {
                key: FactKey::new("shop.Item", "i1"),
                expected: FactVersion(1),
                mutation: FactMutation::Delete,
            }],
            ..CellCommit::empty()
        })
        .map_err(|e| format!("{e:?}"))?;
        let store = cell.into_store();
        let host = DevHost::from_store(store, scope.clone(), |command| command.payload.clone())
            .map_err(|e| format!("{e:?}"))?
            .with_authorizer(AllowAll);
        let row = ObjectRecord {
            object_type: "shop.Item".into(),
            id: "i1".into(),
            scope: scope.key(),
            version: 2,
            values: BTreeMap::new(),
        };
        let cell_ref = host.cell().map_err(|e| format!("{e:?}"))?;
        let reader = cell_ref.store().read();
        let (entry, _sig) = version_entry((&host, &TestAccess, "viewer"), &reader, &row, 2)
            .map_err(|e| format!("{e:?}"))?
            .ok_or_else(|| String::from("tombstone version must be readable"))?;
        assert_eq!(entry.version, 2);
        assert!(entry.values.is_none());
        assert!(matches!(entry.writer, NornVersionWriter::Other));
        Ok(())
    }
}
