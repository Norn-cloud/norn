//! SOURCE-RESOLVE-1 (07-world-boundary): when several mappings write one
//! canonical object, each property's value is decided by its declared
//! authority ([`norn_ir::mapping::SourceAuthority`]) over the latest report
//! of every source, never by the source that wrote last. A source's latest
//! report is its highest `source_version`, whatever order its versions were
//! consumed in; the consuming commit only bounds what a historical read sees.
//!
//! Every report stays retained as consumed mapping evidence, indexed by
//! subject (`_norn_source_subject`). A report that does not decide is kept,
//! not written; [`StoreReader::source_resolution`] reads each property's
//! decision with every source's report and lineage, an explicit conflict
//! where peers disagree, and a SOURCE-AGREE-2 finding for each report that
//! differs from the canonical value. Findings are recomputed, never stored.

use std::collections::BTreeMap;

use norn_checker::mapping::resolution::{Resolution, ResolutionTable};
use norn_checker::metric::compute::contribution::instant_key;
use norn_ir::mapping::{MappedRecord, SourceAuthority};
use norn_ir::provenance::InstanceProvenance;
use norn_ir::{RuntimeObject, RuntimeValue};
use norn_kernel::Observed;

use crate::CellError;
use crate::engine::{SqlEngine, SqlRow, params};
use crate::mapping_bundle::MappingBundle;
use crate::read::{StoreReader, to_i64, to_u64};
use crate::system::{
    INBOX, META, RECORD_ID, SOURCE_RECORD, SOURCE_RESOLUTION_PENDING, SOURCE_SUBJECT,
    SOURCE_VERSION,
};

pub use crate::mapping_links::LinkConflict;

/// SOURCE-AGREE-2's rule id: a source's report differs from the canonical
/// value its declared authorities decide.
pub const SOURCE_AGREE_2: &str = "SOURCE-AGREE-2";

/// The most retained bundles one subject's resolution reads.
const BOUND: usize = 4096;

/// One source's latest report on one subject.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceReport {
    /// The mapping artifact id that produced it.
    pub mapping: String,
    /// The source system the bundle captured (`synthetic-odoo`).
    pub system: String,
    /// The retained bundle (`mapping-bundle:…`).
    pub observation_id: String,
    /// The commit that consumed it; `None` for the report being applied.
    pub commit_seq: Option<u64>,
    /// The raw `source_version` the bundle captured; within one source the
    /// highest version (lexicographic, as the dataplane orders it) is its
    /// latest report.
    pub source_version: String,
    /// The reported values, by property.
    pub values: RuntimeObject,
    /// Field-level lineage the mapping emitted, by property.
    pub lineage: BTreeMap<String, InstanceProvenance>,
    /// The mapped record's own provenance (where it came from and its
    /// subject rule), which a write it decides is attributed to.
    pub record: Option<InstanceProvenance>,
}

/// How one property of a subject is decided.
#[derive(Debug, Clone, PartialEq)]
pub enum PropertyDecision {
    /// One mapping writes it, with no declaration: its report is the value.
    Single {
        /// The value.
        value: RuntimeValue,
    },
    /// A deciding writer (system of record, or the best-ranked precedence
    /// writer that reported) decides it.
    Decided {
        /// The mapping that decided.
        mapping: String,
        /// The canonical value.
        value: RuntimeValue,
    },
    /// Conflict peers agree.
    Agreed {
        /// The agreed value.
        value: RuntimeValue,
    },
    /// Conflict peers disagree: the value is this explicit conflict (stored
    /// as null), never a guess; `reports` lists each source's value.
    Conflict,
    /// Only writers that may not decide (checks) have reported: the subject
    /// is withheld until one of `awaiting` reports.
    Undecided {
        /// The writers that may decide the property (system of record,
        /// precedence or conflict peers), none of which has reported.
        awaiting: Vec<String>,
    },
}

/// One source's report on one property.
#[derive(Debug, Clone, PartialEq)]
pub struct PropertyReport {
    /// The mapping artifact id.
    pub mapping: String,
    /// The source system.
    pub system: String,
    /// The authority the mapping declares for the property.
    pub authority: Option<SourceAuthority>,
    /// The reported value.
    pub value: RuntimeValue,
    /// The retained bundle.
    pub observation_id: String,
    /// The commit that consumed it.
    pub commit_seq: Option<u64>,
    /// The mapping's field-level lineage for the value, when it emitted one.
    pub lineage: Option<InstanceProvenance>,
}

/// One property of a subject: its decision and every source's report.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedProperty {
    /// The property.
    pub property: String,
    /// How it is decided.
    pub decision: PropertyDecision,
    /// Every source's latest report, by mapping id.
    pub reports: Vec<PropertyReport>,
}

/// SOURCE-AGREE-2: a source's report that differs from the canonical value.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceValueFinding {
    /// Always [`SOURCE_AGREE_2`].
    pub rule: &'static str,
    /// The disagreeing property.
    pub property: String,
    /// The canonical decision it differs from.
    pub decision: PropertyDecision,
    /// The differing report.
    pub report: PropertyReport,
}

/// The resolution of one subject: each property several sources report.
#[derive(Debug, Clone, PartialEq)]
pub struct SubjectResolution {
    /// Every reported property, in property order.
    pub properties: Vec<ResolvedProperty>,
    /// SOURCE-AGREE-2 findings, in property then mapping order.
    pub findings: Vec<SourceValueFinding>,
    /// Exclusive links sources currently claim with different targets (as
    /// the journal stands now): an explicit conflict that changes no edge,
    /// while every property decision above still applies.
    pub links: Vec<LinkConflict>,
}

impl SubjectResolution {
    /// The decision of `property`, when it was reported.
    #[must_use]
    pub fn decision(&self, property: &str) -> Option<&PropertyDecision> {
        self.properties
            .iter()
            .find(|p| p.property == property)
            .map(|p| &p.decision)
    }
}

/// The values one bundle reports for `object_type`/`key` (the mapped
/// record's single observe invocation, without its id field).
fn reported(record: &MappedRecord) -> Option<RuntimeObject> {
    let [invocation] = record.invocations.as_slice() else {
        return None;
    };
    let mut values = invocation.input.clone();
    values.remove(norn_checker::mapping::OBJECT_ID_FIELD);
    Some(values)
}

/// Whether `a` is a later report of one source than `b`: a higher source
/// version, or the same version consumed later.
fn later(a: &SourceReport, b: &SourceReport) -> bool {
    (a.source_version.as_str(), a.commit_seq.unwrap_or(u64::MAX))
        > (b.source_version.as_str(), b.commit_seq.unwrap_or(u64::MAX))
}

fn report_of(
    bundle: &MappingBundle,
    object_type: &str,
    key: &str,
    commit_seq: Option<u64>,
) -> Option<SourceReport> {
    let record = bundle
        .records
        .iter()
        .rev()
        .find(|r| r.object_type.as_str() == object_type && r.key == key)?;
    Some(SourceReport {
        mapping: bundle.input.mapping.clone(),
        system: bundle
            .raw
            .get("source")
            .and_then(RuntimeValue::as_str)
            .unwrap_or_default()
            .to_owned(),
        observation_id: format!("mapping-bundle:{}", bundle.digest),
        commit_seq,
        source_version: bundle
            .raw
            .get("source_version")
            .and_then(RuntimeValue::as_str)
            .unwrap_or_default()
            .to_owned(),
        values: reported(record)?,
        lineage: record
            .property_provenance
            .iter()
            .map(|(p, l)| (p.as_str().to_owned(), l.clone()))
            .collect(),
        record: Some(record.provenance.clone()),
    })
}

/// Decide every property `reports` carry under `table`.
fn decide(
    table: &ResolutionTable,
    object_type: &str,
    reports: &BTreeMap<String, SourceReport>,
) -> SubjectResolution {
    let mut properties: BTreeMap<String, BTreeMap<String, RuntimeValue>> = BTreeMap::new();
    for (mapping, report) in reports {
        for (property, value) in &report.values {
            properties
                .entry(property.clone())
                .or_default()
                .insert(mapping.clone(), value.clone());
        }
    }
    let mut out = SubjectResolution {
        properties: Vec::new(),
        findings: Vec::new(),
        links: Vec::new(),
    };
    for (property, values) in properties {
        let authorities: BTreeMap<&str, Option<SourceAuthority>> =
            table.writers(object_type, &property).into_iter().collect();
        let decision = match table.resolve(object_type, &property, &values) {
            None => values.values().next_back().map_or(
                PropertyDecision::Undecided {
                    awaiting: Vec::new(),
                },
                |value| PropertyDecision::Single {
                    value: value.clone(),
                },
            ),
            Some(Resolution::Decided { mapping, value }) => PropertyDecision::Decided {
                mapping: mapping.to_owned(),
                value: value.clone(),
            },
            Some(Resolution::Agreed { value }) => PropertyDecision::Agreed {
                value: value.clone(),
            },
            Some(Resolution::Conflict) => PropertyDecision::Conflict,
            Some(Resolution::Undecided) => PropertyDecision::Undecided {
                awaiting: authorities
                    .iter()
                    .filter(|(mapping, authority)| {
                        !matches!(authority, None | Some(SourceAuthority::Check))
                            && !reports.contains_key(**mapping)
                    })
                    .map(|(mapping, _)| (*mapping).to_owned())
                    .collect(),
            },
        };
        let canonical = match &decision {
            PropertyDecision::Single { value }
            | PropertyDecision::Decided { value, .. }
            | PropertyDecision::Agreed { value } => Some(value),
            PropertyDecision::Conflict | PropertyDecision::Undecided { .. } => None,
        };
        let mut listed = Vec::new();
        for (mapping, value) in &values {
            let Some(source) = reports.get(mapping) else {
                continue;
            };
            let report = PropertyReport {
                mapping: mapping.clone(),
                system: source.system.clone(),
                authority: authorities.get(mapping.as_str()).copied().flatten(),
                value: value.clone(),
                observation_id: source.observation_id.clone(),
                commit_seq: source.commit_seq,
                lineage: source.lineage.get(&property).cloned(),
            };
            // A report differs from a decided value, or peers disagree.
            let differs = match canonical {
                Some(canonical) => canonical != value,
                None => decision == PropertyDecision::Conflict,
            };
            if differs {
                out.findings.push(SourceValueFinding {
                    rule: SOURCE_AGREE_2,
                    property: property.clone(),
                    decision: decision.clone(),
                    report: report.clone(),
                });
            }
            listed.push(report);
        }
        out.properties.push(ResolvedProperty {
            property,
            decision,
            reports: listed,
        });
    }
    out
}

/// What applying one record does under its type's resolution.
pub(crate) enum Applied {
    /// The type is not resolved across sources: write the record as mapped.
    AsMapped,
    /// Write these values (possibly none: the report decides nothing new).
    Values(RuntimeObject),
    /// Keep the report, write nothing: some property has no deciding report
    /// yet (whether or not the subject exists, so the outcome is the same in
    /// every order); [`StoreReader::withheld_subjects`] names what it awaits.
    Withheld,
    /// Keep the bundle as evidence, write nothing of the record (values,
    /// lifecycle snapshots or links): its source record already has a higher
    /// source version consumed.
    Superseded,
}

impl<E: SqlEngine> StoreReader<'_, E> {
    /// The current report of each mapping on `object_type`/`key` at
    /// `frontier` (a commit; all when `None`). A source record contributes
    /// its highest source version consumed by then, never merely the last
    /// one consumed, and only while that version still reports this subject:
    /// a newer upsert that moved the record to another key or out of the map
    /// withdraws it. Superseded versions are skipped before the work bound
    /// applies, so accumulated history never makes a subject unreadable.
    pub(crate) fn latest_reports(
        &self,
        object_type: &str,
        key: &str,
        frontier: Option<u64>,
    ) -> Result<BTreeMap<String, SourceReport>, CellError> {
        let at = frontier.map(|f| to_i64(f, "frontier")).transpose()?;
        let bound = i64::try_from(BOUND.saturating_add(1)).unwrap_or(i64::MAX);
        let rows = self.conn.query(
            &format!(
                "SELECT s.\"mapping\", s.\"source_version\", s.\"commit_seq\", s.\"position\", s.\"observation_id\" FROM \"{SOURCE_SUBJECT}\" s \
                 WHERE s.\"object_type\" = ?1 AND s.\"key\" = ?2 AND (?3 IS NULL OR s.\"commit_seq\" <= ?3) \
                 AND NOT EXISTS (SELECT 1 FROM \"{SOURCE_RECORD}\" n WHERE n.\"mapping\" = s.\"mapping\" AND n.\"record_id\" = s.\"record_id\" AND (?3 IS NULL OR n.\"commit_seq\" <= ?3) \
                 AND (n.\"source_version\", n.\"commit_seq\", n.\"position\") > (s.\"source_version\", s.\"commit_seq\", s.\"position\")) LIMIT ?4"
            ),
            params![object_type, key, at, bound],
        )?;
        if rows.len() > BOUND {
            return Err(CellError::ComputeRefused("core.compute.bound_exceeded"));
        }
        // Each mapping's best current contribution (several of its source
        // records may report one subject).
        let mut best: BTreeMap<String, (String, u64, i64, String)> = BTreeMap::new();
        for row in rows {
            let row = SqlRow(row);
            let mapping: String = row.get(0)?;
            let order = (
                row.get::<String>(1)?,
                to_u64(row.get(2)?, "commit_seq")?,
                row.get::<i64>(3)?,
                row.get::<String>(4)?,
            );
            if best
                .get(&mapping)
                .is_none_or(|b| (&order.0, order.1, order.2) > (&b.0, b.1, b.2))
            {
                best.insert(mapping, order);
            }
        }
        let mut out = BTreeMap::new();
        for (mapping, (_, commit_seq, _, observation_id)) in best {
            if let Some(report) =
                self.report_in(&observation_id, &mapping, object_type, key, commit_seq)?
            {
                out.insert(mapping, report);
            }
        }
        Ok(out)
    }

    /// The report retained bundle `observation_id` of `mapping` makes on
    /// `object_type`/`key`, rechecked against its stored identity.
    fn report_in(
        &self,
        observation_id: &str,
        mapping: &str,
        object_type: &str,
        key: &str,
        commit_seq: u64,
    ) -> Result<Option<SourceReport>, CellError> {
        let encoded: String = self
            .conn
            .query_optional(
                &format!("SELECT \"observed\" FROM \"{INBOX}\" WHERE \"observation_id\" = ?1"),
                params![observation_id],
                |row| row.get(0),
            )?
            .ok_or(CellError::MappingBundleMismatch)?;
        let observed: Observed<RuntimeValue> = serde_json::from_str(&encoded)?;
        let bundle: MappingBundle =
            norn_ir::runtime::decode(&observed.input).map_err(CellError::Encoding)?;
        if observation_id != format!("mapping-bundle:{}", bundle.digest)
            || bundle.input.mapping != mapping
        {
            return Err(CellError::MappingBundleMismatch);
        }
        Ok(report_of(&bundle, object_type, key, Some(commit_seq)))
    }

    /// SOURCE-RESOLVE-1 for one subject at `frontier` under the active
    /// definitions: each reported property's decision with every source's
    /// report and lineage, and the SOURCE-AGREE-2 findings. `None` when no
    /// mapping of the active set resolves `object_type` across sources.
    ///
    /// # Errors
    /// More than 4096 retained reports (`core.compute.bound_exceeded`), or
    /// retained evidence that no longer matches its bundle.
    pub fn source_resolution(
        &self,
        object_type: &str,
        key: &str,
        frontier: Option<u64>,
    ) -> Result<Option<SubjectResolution>, CellError> {
        let reports = self.latest_reports(object_type, key, frontier)?;
        if reports.is_empty() {
            return Ok(None);
        }
        let Some(table) = self.resolution_at(frontier)? else {
            return Ok(None);
        };
        if !table.governs(object_type) {
            return Ok(None);
        }
        let mut resolution = decide(&table, object_type, &reports);
        resolution.links = self.source_link_conflicts(object_type, key)?;
        Ok(Some(resolution))
    }

    /// The resolution table of the definitions installed at `frontier` (the
    /// active ones when `None`), read from that installed set itself: a
    /// historical read decides under the definitions in force at its
    /// frontier, never under authorities activated since, and whichever
    /// mappings still report. `None` without definitions.
    fn resolution_at(
        &self,
        frontier: Option<u64>,
    ) -> Result<Option<std::sync::Arc<ResolutionTable>>, CellError> {
        let version = match frontier {
            Some(frontier) => self.definitions_version_at(frontier)?,
            None => self.active_definitions_version()?,
        };
        version.map(|v| self.resolution_table(v)).transpose()
    }

    /// The values `record` of `bundle` writes under its type's resolution:
    /// every property is decided over the latest report of each source (this
    /// one included), so the result does not depend on which source arrived
    /// first, and a report that does not decide is kept but not written.
    pub(crate) fn resolve_mapped_record(
        &self,
        bundle: &MappingBundle,
        record: &MappedRecord,
        current: Option<&RuntimeObject>,
    ) -> Result<Applied, CellError> {
        let plan = self.mapping_plan(&bundle.input, bundle.definitions_version)?;
        let object_type = record.object_type.as_str();
        if !plan.resolution.governs(object_type) {
            return Ok(Applied::AsMapped);
        }
        // A version of a source record older than one already consumed is
        // kept as evidence and has no canonical effect at all (values,
        // lifecycle snapshots, links), so a late version never creates or
        // updates an object at a key its record has since moved away from.
        if self.superseded(bundle)? {
            return Ok(Applied::Superseded);
        }
        let mut reports = self.latest_reports(object_type, &record.key, None)?;
        let incoming = report_of(bundle, object_type, &record.key, None)
            .ok_or(CellError::UnsupportedMappingProfile)?;
        // Nor does an older report of a source that already reported a
        // newer one here from another of its records (`later`).
        if reports
            .get(&incoming.mapping)
            .is_none_or(|newer| !later(newer, &incoming))
        {
            reports.insert(incoming.mapping.clone(), incoming);
        }
        let resolution = decide(&plan.resolution, object_type, &reports);
        let mut values = RuntimeObject::new();
        for property in resolution.properties {
            let value = match property.decision {
                PropertyDecision::Single { value }
                | PropertyDecision::Decided { value, .. }
                | PropertyDecision::Agreed { value } => value,
                PropertyDecision::Conflict => RuntimeValue::Null,
                PropertyDecision::Undecided { .. } => return Ok(Applied::Withheld),
            };
            if current.and_then(|c| c.get(&property.property)) != Some(&value) {
                values.insert(property.property, value);
            }
        }
        Ok(Applied::Values(values))
    }

    /// Whether `bundle`'s source record already has a higher source version
    /// consumed (whatever subjects that version reports).
    fn superseded(&self, bundle: &MappingBundle) -> Result<bool, CellError> {
        let id = format!("mapping-bundle:{}", bundle.digest);
        Ok(self
            .superseded_bundles(std::slice::from_ref(&id))?
            .contains(&id))
    }

    /// The bundles of `ids` (pending or consumed) whose source record has a
    /// higher source version consumed, as the store now stands.
    pub(crate) fn superseded_bundles(
        &self,
        ids: &[String],
    ) -> Result<std::collections::BTreeSet<String>, CellError> {
        let rows = self.conn.query(
            &format!(
                "SELECT i.\"observation_id\" FROM \"{INBOX}\" i WHERE i.\"observation_id\" IN (SELECT value FROM json_each(?1)) \
                 AND EXISTS (SELECT 1 FROM \"{SOURCE_RECORD}\" r WHERE r.\"mapping\" = json_extract(i.\"observed\", '$.input.input.mapping') AND r.\"record_id\" = {RECORD_ID} AND r.\"source_version\" > {SOURCE_VERSION})"
            ),
            params![serde_json::to_string(ids)?],
        )?;
        rows.into_iter()
            .map(|row| SqlRow(row).get::<String>(0))
            .collect()
    }

    /// The consumed mapping bundle `observation_id` (`None` when it is not
    /// one, or still pending).
    pub(crate) fn consumed_mapping_bundle(
        &self,
        observation_id: &str,
    ) -> Result<Option<MappingBundle>, CellError> {
        if !observation_id.starts_with("mapping-bundle:") {
            return Ok(None);
        }
        let encoded: Option<String> = self.conn.query_optional(
            &format!(
                "SELECT \"observed\" FROM \"{INBOX}\" WHERE \"observation_id\" = ?1 AND \"consumed_commit_seq\" IS NOT NULL"
            ),
            params![observation_id],
            |row| row.get(0),
        )?;
        let Some(encoded) = encoded else {
            return Ok(None);
        };
        let observed: Observed<RuntimeValue> = serde_json::from_str(&encoded)?;
        let bundle: MappingBundle =
            norn_ir::runtime::decode(&observed.input).map_err(CellError::Encoding)?;
        Ok(Some(bundle))
    }

    /// The changed values `reports` decide for `object_type`/`key` under
    /// `table`, against the stored object, with the provenance of the
    /// report that decides (or of the first one): `None` when nothing is
    /// reported, a property is undecided, or nothing changes.
    fn decided_change(
        &self,
        table: &ResolutionTable,
        object_type: &str,
        key: &str,
        reports: &BTreeMap<String, SourceReport>,
    ) -> Result<Option<(RuntimeObject, InstanceProvenance)>, CellError> {
        let current = self
            .object(crate::MAIN_WORLD, object_type, key)?
            .map(|r| r.values);
        let mut values = RuntimeObject::new();
        let mut decider = None;
        for property in decide(table, object_type, reports).properties {
            let value = match property.decision {
                PropertyDecision::Decided { mapping, value } => {
                    decider.get_or_insert(mapping);
                    value
                }
                PropertyDecision::Single { value } | PropertyDecision::Agreed { value } => value,
                PropertyDecision::Conflict => RuntimeValue::Null,
                PropertyDecision::Undecided { .. } => return Ok(None),
            };
            if current.as_ref().and_then(|c| c.get(&property.property)) != Some(&value) {
                values.insert(property.property, value);
            }
        }
        let provenance = decider
            .and_then(|m| reports.get(&m))
            .or_else(|| reports.values().next())
            .and_then(|r| r.record.clone());
        Ok(match provenance {
            Some(provenance) if !values.is_empty() => Some((values, provenance)),
            _ => None,
        })
    }

    /// SOURCE-RESOLVE-1: the subjects the consumed contribution of
    /// `bundle`'s source record reported and `bundle` (a newer upsert of
    /// it) no longer reports, each with the change its remaining reports
    /// decide. A stale `bundle` withdraws nothing; a subject no other
    /// source reports keeps its values (withdrawing the last report is a
    /// delete, outside the upsert profile).
    pub(crate) fn withdrawn_changes(
        &self,
        bundle: &MappingBundle,
        observation_id: &str,
    ) -> Result<Vec<(String, String, RuntimeObject, InstanceProvenance)>, CellError> {
        let plan = self.mapping_plan(&bundle.input, bundle.definitions_version)?;
        let previous: Option<(String, String)> = self.conn.query_optional(
            &format!(
                "SELECT r.\"observation_id\", r.\"source_version\" FROM \"{SOURCE_RECORD}\" r WHERE r.\"mapping\" = ?2 AND r.\"record_id\" = (SELECT {RECORD_ID} FROM \"{INBOX}\" i WHERE i.\"observation_id\" = ?1) \
                 ORDER BY r.\"source_version\" DESC, r.\"commit_seq\" DESC, r.\"position\" DESC LIMIT 1"
            ),
            params![observation_id, bundle.input.mapping.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let Some((previous, version)) = previous else {
            return Ok(Vec::new());
        };
        let incoming = bundle
            .raw
            .get("source_version")
            .and_then(RuntimeValue::as_str)
            .unwrap_or_default();
        if version.as_str() > incoming {
            return Ok(Vec::new());
        }
        let subjects = self.conn.query(
            &format!(
                "SELECT \"object_type\", \"key\" FROM \"{SOURCE_SUBJECT}\" WHERE \"observation_id\" = ?1"
            ),
            params![previous.as_str()],
        )?;
        let mut out = Vec::new();
        for row in subjects {
            let row = SqlRow(row);
            let (object_type, key): (String, String) = (row.get(0)?, row.get(1)?);
            if !plan.resolution.governs(&object_type)
                || bundle
                    .records
                    .iter()
                    .any(|r| r.object_type.as_str() == object_type && r.key == key)
            {
                continue;
            }
            let mut reports = self.latest_reports(&object_type, &key, None)?;
            reports.retain(|_, r| r.observation_id != previous);
            if let Some((values, provenance)) =
                self.decided_change(&plan.resolution, &object_type, &key, &reports)?
            {
                out.push((object_type, key, values, provenance));
            }
        }
        Ok(out)
    }

    /// SOURCE-RESOLVE-1 after an activation: every subject with retained
    /// reports whose stored values differ from what the active
    /// definitions decide, with that change.
    pub(crate) fn reconciliation(
        &self,
    ) -> Result<Vec<(String, String, RuntimeObject, InstanceProvenance)>, CellError> {
        let Some(table) = self.resolution_at(None)? else {
            return Ok(Vec::new());
        };
        let rows = self.conn.query(
            &format!(
                "SELECT DISTINCT \"object_type\", \"key\" FROM \"{SOURCE_SUBJECT}\" ORDER BY \"object_type\", \"key\""
            ),
            params![],
        )?;
        let mut out = Vec::new();
        for row in rows {
            let row = SqlRow(row);
            let (object_type, key): (String, String) = (row.get(0)?, row.get(1)?);
            if !table.governs(&object_type) {
                continue;
            }
            let reports = self.latest_reports(&object_type, &key, None)?;
            if let Some((values, provenance)) =
                self.decided_change(&table, &object_type, &key, &reports)?
            {
                out.push((object_type, key, values, provenance));
            }
        }
        Ok(out)
    }

    /// The records of the consumed `bundle` that SOURCE-RESOLVE-1 withholds
    /// as the store now stands, each with the writers its undecided
    /// properties await.
    pub(crate) fn withheld_subjects(
        &self,
        bundle: &MappingBundle,
    ) -> Result<Vec<WithheldSubject>, CellError> {
        let plan = self.mapping_plan(&bundle.input, bundle.definitions_version)?;
        let mut out = Vec::new();
        for record in &bundle.records {
            let object_type = record.object_type.as_str();
            if !plan.resolution.governs(object_type) {
                continue;
            }
            let reports = self.latest_reports(object_type, &record.key, None)?;
            let awaiting: Vec<(String, Vec<String>)> =
                decide(&plan.resolution, object_type, &reports)
                    .properties
                    .into_iter()
                    .filter_map(|p| match p.decision {
                        PropertyDecision::Undecided { awaiting } => Some((p.property, awaiting)),
                        PropertyDecision::Single { .. }
                        | PropertyDecision::Decided { .. }
                        | PropertyDecision::Agreed { .. }
                        | PropertyDecision::Conflict => None,
                    })
                    .collect();
            if !awaiting.is_empty() {
                out.push(WithheldSubject {
                    object_type: object_type.to_owned(),
                    key: record.key.clone(),
                    awaiting,
                });
            }
        }
        Ok(out)
    }

    /// SOURCE-RESOLVE-1: the keys of `object_type` with retained reports
    /// known at `frontier` (all when `None`), when the definitions in force
    /// there resolve the type across sources; `None` otherwise.
    ///
    /// # Errors
    /// More than `max` keys (`core.compute.bound_exceeded`), or store failures.
    pub(crate) fn reported_keys(
        &self,
        object_type: &str,
        frontier: Option<u64>,
        max: usize,
    ) -> Result<Option<Vec<String>>, CellError> {
        let Some(table) = self.resolution_at(frontier)? else {
            return Ok(None);
        };
        if !table.governs(object_type) {
            return Ok(None);
        }
        let at = frontier.map(|f| to_i64(f, "frontier")).transpose()?;
        let limit = i64::try_from(max.saturating_add(1)).unwrap_or(i64::MAX);
        let rows = self.conn.query(
            &format!(
                "SELECT DISTINCT \"key\" FROM \"{SOURCE_SUBJECT}\" WHERE \"object_type\" = ?1 AND (?2 IS NULL OR \"commit_seq\" <= ?2) ORDER BY \"key\" LIMIT ?3"
            ),
            params![object_type, at, limit],
        )?;
        if rows.len() > max {
            return Err(CellError::ComputeRefused("core.compute.bound_exceeded"));
        }
        rows.into_iter()
            .map(|row| SqlRow(row).get::<String>(0))
            .collect::<Result<Vec<_>, _>>()
            .map(Some)
    }

    /// Whether `object_type` has retained reports resolved across sources
    /// under the active definitions.
    pub(crate) fn resolves_reports_of(&self, object_type: &str) -> Result<bool, CellError> {
        let rows = self.conn.query(
            &format!("SELECT 1 FROM \"{SOURCE_SUBJECT}\" WHERE \"object_type\" = ?1 LIMIT 1"),
            params![object_type],
        )?;
        if rows.is_empty() {
            return Ok(false);
        }
        Ok(self
            .resolution_at(None)?
            .is_some_and(|table| table.governs(object_type)))
    }

    /// SOURCE-RESOLVE-1 over valid time: the canonical values of
    /// `object_type`/`key` at each valid time (`time_property`) its retained
    /// reports known at `frontier` carry, decided under the definitions in
    /// force at `frontier` (as [`Self::source_resolution`] decides). Each
    /// slice decides over every source record's latest version effective by
    /// then, laid over `base` (the canonical record), so an as-of answer
    /// depends on the reports, not on the order they were synced in. A
    /// version that moved its record to another key, or out of the map,
    /// withdraws that record's report from its own valid time on (from every
    /// valid time when it carries none), as the live value withdraws it; a
    /// time when every report is withdrawn is an explicit withdrawal slice
    /// (`true`, carrying only its valid time), so the subject is absent from
    /// then on rather than carried forward. A slice with an undecided
    /// property is absent; one no report decides carries the previous slice
    /// forward. `None` when the type is not resolved across sources.
    ///
    /// # Errors
    /// As [`Self::source_resolution`].
    pub(crate) fn resolved_slices(
        &self,
        object_type: &str,
        key: &str,
        frontier: Option<u64>,
        time_property: &str,
        base: &RuntimeObject,
    ) -> Result<Option<Vec<(RuntimeObject, bool)>>, CellError> {
        let streams = self.record_streams(object_type, key, frontier)?;
        if streams.is_empty() {
            return Ok(None);
        }
        let Some(table) = self.resolution_at(frontier)? else {
            return Ok(None);
        };
        if !table.governs(object_type) {
            return Ok(None);
        }
        let at = |values: &RuntimeObject| {
            let text = values.get(time_property)?.as_str()?;
            instant_key(text, false)
                .or_else(|| instant_key(text, true))
                .map(|instant| (instant, text.to_owned()))
        };
        // Each version's effective time: its report's valid time here; for
        // one that no longer reports this subject, the valid time of the
        // record it reports instead (`Some(None)`: none, so from the start).
        let mut timed = Vec::new();
        for ((mapping, _), versions) in &streams {
            let mut stream = Vec::new();
            for version in versions {
                let effective = match &version.report {
                    Some(report) => at(&report.values).map(Some),
                    None => Some(version.moved_values.as_ref().and_then(&at)),
                };
                if let Some(effective) = effective {
                    stream.push((effective, version));
                }
            }
            timed.push((mapping.as_str(), stream));
        }
        let mut instants: Vec<(_, String)> = timed
            .iter()
            .flat_map(|(_, stream)| stream.iter().filter_map(|(t, _)| t.clone()))
            .collect();
        instants.sort_unstable();
        instants.dedup_by(|a, b| a.0 == b.0);
        let mut slices = Vec::with_capacity(instants.len());
        'slices: for (time, text) in instants {
            // Each mapping's best current report among its source records.
            let mut reports: BTreeMap<String, (&RecordVersion, &SourceReport)> = BTreeMap::new();
            for (mapping, stream) in &timed {
                let current = stream
                    .iter()
                    .filter(|(t, _)| t.as_ref().is_none_or(|(t, _)| *t <= time))
                    .map(|(_, v)| *v)
                    .max_by(|a, b| a.order.cmp(&b.order));
                let Some(report) = current.and_then(|v| v.report.as_ref().map(|r| (v, r))) else {
                    continue;
                };
                if reports
                    .get(*mapping)
                    .is_none_or(|(best, _)| report.0.order > best.order)
                {
                    reports.insert((*mapping).to_owned(), report);
                }
            }
            if reports.is_empty() {
                slices.push((
                    RuntimeObject::from([(
                        time_property.to_owned(),
                        RuntimeValue::from(text.as_str()),
                    )]),
                    true,
                ));
                continue;
            }
            let reports = reports
                .into_iter()
                .map(|(m, (_, r))| (m, r.clone()))
                .collect();
            let mut values = base.clone();
            for property in decide(&table, object_type, &reports).properties {
                let value = match property.decision {
                    PropertyDecision::Single { value }
                    | PropertyDecision::Decided { value, .. }
                    | PropertyDecision::Agreed { value } => value,
                    PropertyDecision::Conflict => RuntimeValue::Null,
                    PropertyDecision::Undecided { .. } => continue 'slices,
                };
                values.insert(property.property, value);
            }
            // The slice holds from its own valid time, whichever report decided.
            values.insert(time_property.to_owned(), RuntimeValue::from(text.as_str()));
            slices.push((values, false));
        }
        Ok(Some(slices))
    }

    /// Every consumed version, at `frontier`, of each source record (by
    /// mapping and record identity) that reported `object_type`/`key`:
    /// its report here, or, once it reports this subject no more, the
    /// values of the record of `object_type` it reports instead.
    fn record_streams(
        &self,
        object_type: &str,
        key: &str,
        frontier: Option<u64>,
    ) -> Result<BTreeMap<(String, String), Vec<RecordVersion>>, CellError> {
        let frontier = frontier.map(|f| to_i64(f, "frontier")).transpose()?;
        let bound = i64::try_from(BOUND.saturating_add(1)).unwrap_or(i64::MAX);
        let rows = self.conn.query(
            &format!(
                "SELECT r.\"mapping\", r.\"record_id\", r.\"source_version\", r.\"commit_seq\", r.\"position\", i.\"observation_id\", i.\"observed\" FROM \"{SOURCE_RECORD}\" r JOIN \"{INBOX}\" i ON i.\"observation_id\" = r.\"observation_id\" \
                 WHERE (?3 IS NULL OR r.\"commit_seq\" <= ?3) AND EXISTS (SELECT 1 FROM \"{SOURCE_SUBJECT}\" s WHERE s.\"object_type\" = ?1 AND s.\"key\" = ?2 AND s.\"mapping\" = r.\"mapping\" AND s.\"record_id\" = r.\"record_id\" AND (?3 IS NULL OR s.\"commit_seq\" <= ?3)) \
                 ORDER BY r.\"commit_seq\", r.\"position\" LIMIT ?4"
            ),
            params![object_type, key, frontier, bound],
        )?;
        if rows.len() > BOUND {
            return Err(CellError::ComputeRefused("core.compute.bound_exceeded"));
        }
        let mut out: BTreeMap<(String, String), Vec<RecordVersion>> = BTreeMap::new();
        for row in rows {
            let row = SqlRow(row);
            let mapping: String = row.get(0)?;
            let record_id: String = row.get(1)?;
            let commit_seq = to_u64(row.get(3)?, "commit_seq")?;
            let order = (row.get::<String>(2)?, commit_seq, row.get::<i64>(4)?);
            let stored_id: String = row.get(5)?;
            let observed: Observed<RuntimeValue> = serde_json::from_str(&row.get::<String>(6)?)?;
            let bundle: MappingBundle =
                norn_ir::runtime::decode(&observed.input).map_err(CellError::Encoding)?;
            if stored_id != format!("mapping-bundle:{}", bundle.digest)
                || bundle.input.mapping != mapping
            {
                return Err(CellError::MappingBundleMismatch);
            }
            let report = report_of(&bundle, object_type, key, Some(commit_seq));
            let moved_values = if report.is_none() {
                bundle
                    .records
                    .iter()
                    .rev()
                    .find(|r| r.object_type.as_str() == object_type)
                    .and_then(reported)
            } else {
                None
            };
            out.entry((mapping, record_id))
                .or_default()
                .push(RecordVersion {
                    order,
                    report,
                    moved_values,
                });
        }
        Ok(out)
    }
}

impl<E: SqlEngine> crate::CellStore<E> {
    /// SOURCE-RESOLVE-1: whether a definitions activation committed without
    /// its reconciliation having finished (the cell was interrupted between
    /// them). A host resumes it with
    /// [`crate::SqliteCell::reconcile_source_resolution`] before serving.
    ///
    /// # Errors
    /// Store failures.
    pub fn source_resolution_pending(&self) -> Result<bool, CellError> {
        Ok(self
            .conn
            .query_optional(
                &format!("SELECT 1 FROM \"{META}\" WHERE \"key\" = ?1"),
                params![SOURCE_RESOLUTION_PENDING],
                |row| row.get::<i64>(0),
            )?
            .is_some())
    }

    /// Clear the pending mark once every reconciliation chunk committed.
    pub(crate) fn finish_source_resolution(&self) -> Result<(), CellError> {
        self.conn.execute(
            &format!("DELETE FROM \"{META}\" WHERE \"key\" = ?1"),
            params![SOURCE_RESOLUTION_PENDING],
        )?;
        Ok(())
    }
}

/// One consumed version of a source record, for [`StoreReader::resolved_slices`].
struct RecordVersion {
    /// (source version, commit, position): later versions order higher.
    order: (String, u64, i64),
    /// Its report on the subject, when it still reports it.
    report: Option<SourceReport>,
    /// The values it reports instead (the record moved to another key).
    moved_values: Option<RuntimeObject>,
}

/// A record a consumed bundle reported but SOURCE-RESOLVE-1 did not write:
/// some property has no deciding report yet. Its reports are kept; it is
/// written once a writer in `awaiting` reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WithheldSubject {
    /// The record's object type.
    pub object_type: String,
    /// The record's key.
    pub key: String,
    /// Each undecided property, with the mappings that may decide it.
    pub awaiting: Vec<(String, Vec<String>)>,
}

#[cfg(test)]
mod tests {
    use super::{PropertyDecision, SourceReport, decide};
    use norn_checker::mapping::resolution::ResolutionTable;
    use norn_ir::RuntimeValue;
    use std::collections::BTreeMap;

    #[test]
    fn a_type_no_mapping_resolves_decides_each_property_from_its_one_writer() {
        let reports = BTreeMap::from([(
            "a".to_owned(),
            SourceReport {
                mapping: "a".into(),
                system: "s".into(),
                observation_id: "mapping-bundle:x".into(),
                commit_seq: Some(1),
                source_version: "1".into(),
                values: BTreeMap::from([("q".to_owned(), RuntimeValue::from("one"))]),
                lineage: BTreeMap::new(),
                record: None,
            },
        )]);
        let resolution = decide(&ResolutionTable::default(), "t.T", &reports);
        assert_eq!(
            resolution.decision("q"),
            Some(&PropertyDecision::Single {
                value: RuntimeValue::from("one")
            })
        );
        assert!(resolution.findings.is_empty());
    }
}
