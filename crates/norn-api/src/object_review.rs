//! HOST-READ-6 (#588, #343): an object's quality findings and its version
//! history, the reads behind "why is this value what it is, and who changed it".
//!
//! Both are value-level and need Population, Values and Sources on the row,
//! the same capabilities as its lineage (D67). Unlike an authoritative read,
//! they also serve a quarantined object: OBS-EVIDENCE-1 keeps imported source
//! truth readable to those allowed to review it, and a finding without its
//! subject cannot be reviewed. A withheld, source-withheld or absent object
//! refuses with the same opaque 404 as lineage.
//!
//! The history lists every proven version of the row, newest first, with the
//! receipt that committed it and what wrote it: a mapping (the version's
//! source lineage, as `/lineage?version=` serves it) or an admitted action
//! (the action-call record of that commit: which action, which verified actor,
//! and the effective input, which carries a correction's reason and evidence).

use std::collections::BTreeMap;

use norn_cell::reactive::{DecodeBudget, ReactiveLimits};
use norn_cell::source_provenance::InstanceSourceProvenance;
use norn_cell::{MAIN_WORLD, ObjectRecord, StoreReader, engine::SqlEngine};
use norn_ir::{DataQualityFinding, FactRef, RuntimeValue};
use serde::Serialize;
use utoipa::ToSchema;

use crate::authz::ReadCapability::{Population, Sources, Values};
use crate::host::DevHost;
use crate::object_routes::{ObjectAccess, collection, locked, not_found, permitted};
use crate::refusal::Refusal;
use crate::scope::ScopeQuery;

/// Most versions one history lists, newest first.
pub const MAX_HISTORY_VERSIONS: u64 = 64;

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
    #[schema(value_type = Vec<Object>)]
    pub findings: Vec<DataQualityFinding>,
}

/// What wrote one version.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum NornVersionWriter {
    /// A mapping produced the version from a source record.
    #[serde(rename_all = "camelCase")]
    Mapping {
        /// The version's source lineage (`/lineage?version=`).
        #[schema(value_type = Object)]
        lineage: InstanceSourceProvenance,
    },
    /// An admitted action committed the version.
    #[serde(rename_all = "camelCase")]
    Action {
        /// The action identity.
        action_id: String,
        /// The verified caller the server attributed the call to.
        actor: String,
        /// The effective input (caller input plus server derivations).
        #[schema(value_type = Object)]
        input: RuntimeValue,
    },
    /// Another writer (an import, a migration or a kernel rule).
    Other,
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
    /// Every stored property by property id; absent for a tombstone.
    #[schema(value_type = Object, nullable = true)]
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
    /// The versions listed, newest first.
    pub versions: Vec<NornObjectVersion>,
    /// True when older versions exist beyond [`MAX_HISTORY_VERSIONS`].
    pub truncated: bool,
}

/// The raw row, when the caller holds Population, Values and Sources on it.
/// A quarantined row is served: review reads keep source truth.
fn reviewable<E: SqlEngine>(
    host: &DevHost<E>,
    access: &dyn ObjectAccess,
    actor: &str,
    (object_type, id): (&str, &str),
    start: &mut Option<(u64, Option<(u64, u64)>)>,
) -> Result<ObjectRecord, Refusal> {
    let row = locked(host, access, start, |reader| {
        if !reader.model().has_type(object_type) {
            return Err(not_found());
        }
        reader
            .object(MAIN_WORLD, object_type, id)
            .map_err(Refusal::host)
    })?
    .ok_or_else(not_found)?;
    if [Population, Values, Sources]
        .into_iter()
        .all(|capability| permitted(host, access, actor, &row, capability))
    {
        Ok(row)
    } else {
        Err(not_found())
    }
}

/// HOST-READ-6: an object's quality findings, including while it is quarantined.
///
/// # Errors
/// A denied gate (the opaque 403); an absent, withheld or source-withheld
/// object (one 404); a commit between the reads (409 `object.snapshot_moved`);
/// or a host failure.
pub fn get_object_findings<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    object_type: &str,
    id: &str,
    access: &dyn ObjectAccess,
) -> Result<NornObjectFindings, Refusal> {
    let (_epoch, actor) = collection(host, scope, actor, object_type, access)?;
    let mut start = None;
    let row = reviewable(host, access, &actor, (object_type, id), &mut start)?;
    locked(host, access, &mut start, |reader| {
        Ok(NornObjectFindings {
            object_type: row.object_type.clone(),
            id: row.id.clone(),
            quarantined: reader
                .observation_is_quarantined(MAIN_WORLD, &row)
                .map_err(Refusal::host)?,
            findings: reader
                .observation_findings(MAIN_WORLD, &row.object_type, &row.id)
                .map_err(Refusal::host)?,
        })
    })
}

fn budget() -> DecodeBudget {
    DecodeBudget::new(&ReactiveLimits::from_profile(
        &norn_ir::ResourceLimitProfile::norn_limits_v1(),
    ))
}

/// The action-call record written in `commit`, if an action wrote it.
fn action_writer<E: SqlEngine>(
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
    Ok(Some(NornVersionWriter::Action {
        action_id,
        actor,
        input: record
            .values
            .get("effectiveInput")
            .cloned()
            .unwrap_or(RuntimeValue::Null),
    }))
}

/// One version with its receipt and writer.
fn version_entry<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    row: &ObjectRecord,
    version: u64,
) -> Result<NornObjectVersion, Refusal> {
    let receipt = reader
        .receipt_for_fact_version(MAIN_WORLD, &row.object_type, &row.id, version)
        .map_err(Refusal::host)?;
    let values = reader
        .object_at_version(MAIN_WORLD, &row.object_type, &row.id, version)
        .map_err(Refusal::host)?
        .map(|record| record.values.into_iter().collect());
    let fact = FactRef::new(row.object_type.as_str(), &row.id, version);
    let writer = match reader
        .instance_source_provenance(MAIN_WORLD, &fact, &mut budget(), |_| Ok(true))
        .map_err(Refusal::host)?
    {
        Some(lineage) => NornVersionWriter::Mapping { lineage },
        None => action_writer(reader, &receipt)?.unwrap_or(NornVersionWriter::Other),
    };
    Ok(NornObjectVersion {
        version,
        commit: receipt.body.commit_seq,
        logical_time: receipt.body.logical_time,
        values,
        writer,
    })
}

/// HOST-READ-6: an object's proven versions, newest first, with what wrote each.
///
/// # Errors
/// A denied gate (the opaque 403); an absent, withheld or source-withheld
/// object (one 404); a commit between the reads (409 `object.snapshot_moved`);
/// or a host failure.
pub fn get_object_history<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    object_type: &str,
    id: &str,
    access: &dyn ObjectAccess,
) -> Result<NornObjectHistory, Refusal> {
    let (_epoch, actor) = collection(host, scope, actor, object_type, access)?;
    let mut start = None;
    let row = reviewable(host, access, &actor, (object_type, id), &mut start)?;
    let oldest = row
        .version
        .saturating_sub(MAX_HISTORY_VERSIONS)
        .saturating_add(1)
        .max(1);
    locked(host, access, &mut start, |reader| {
        let versions = (oldest..=row.version)
            .rev()
            .map(|version| version_entry(reader, &row, version))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(NornObjectHistory {
            object_type: row.object_type.clone(),
            id: row.id.clone(),
            versions,
            truncated: oldest > 1,
        })
    })
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
) -> Result<InstanceSourceProvenance, Refusal> {
    let (_epoch, actor) = collection(host, scope, actor, object_type, access)?;
    let mut start = None;
    let row = reviewable(host, access, &actor, (object_type, id), &mut start)?;
    if version == 0 || version > row.version {
        return Err(not_found());
    }
    let fact = FactRef::new(row.object_type.as_str(), &row.id, version);
    locked(host, access, &mut start, |reader| {
        reader
            .instance_source_provenance(MAIN_WORLD, &fact, &mut budget(), |_| Ok(true))
            .map_err(Refusal::host)?
            .ok_or_else(not_found)
    })
}
