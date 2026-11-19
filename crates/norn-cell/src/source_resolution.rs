//! SOURCE-RESOLVE-1 (07-world-boundary): when several mappings write one
//! canonical object, each property's value is decided by its declared
//! authority ([`norn_ir::mapping::SourceAuthority`]) over the latest report
//! of every source, never by the source that wrote last.
//!
//! Every report stays retained as consumed mapping evidence, indexed by
//! subject (`_norn_source_subject`). A report that does not decide is kept,
//! not written; [`StoreReader::source_resolution`] reads each property's
//! decision with every source's report and lineage, an explicit conflict
//! where peers disagree, and a SOURCE-AGREE-2 finding for each report that
//! differs from the canonical value. Findings are recomputed, never stored.

use std::collections::BTreeMap;

use norn_checker::mapping::resolution::{Resolution, ResolutionTable};
use norn_ir::mapping::{MappedRecord, SourceAuthority};
use norn_ir::provenance::InstanceProvenance;
use norn_ir::{RuntimeObject, RuntimeValue};
use norn_kernel::Observed;

use crate::CellError;
use crate::engine::{SqlEngine, SqlRow, params};
use crate::mapping_bundle::MappingBundle;
use crate::read::{StoreReader, to_i64, to_u64};
use crate::system::{INBOX, SOURCE_SUBJECT};

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
    /// The reported values, by property.
    pub values: RuntimeObject,
    /// Field-level lineage the mapping emitted, by property.
    pub lineage: BTreeMap<String, InstanceProvenance>,
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
    /// Only writers that may not decide (checks) have reported.
    Undecided,
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
        values: reported(record)?,
        lineage: record
            .property_provenance
            .iter()
            .map(|(p, l)| (p.as_str().to_owned(), l.clone()))
            .collect(),
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
    };
    for (property, values) in properties {
        let authorities: BTreeMap<&str, Option<SourceAuthority>> =
            table.writers(object_type, &property).into_iter().collect();
        let decision = match table.resolve(object_type, &property, &values) {
            None => values
                .values()
                .next_back()
                .map_or(PropertyDecision::Undecided, |value| {
                    PropertyDecision::Single {
                        value: value.clone(),
                    }
                }),
            Some(Resolution::Decided { mapping, value }) => PropertyDecision::Decided {
                mapping: mapping.to_owned(),
                value: value.clone(),
            },
            Some(Resolution::Agreed { value }) => PropertyDecision::Agreed {
                value: value.clone(),
            },
            Some(Resolution::Conflict) => PropertyDecision::Conflict,
            Some(Resolution::Undecided) => PropertyDecision::Undecided,
        };
        let canonical = match &decision {
            PropertyDecision::Single { value }
            | PropertyDecision::Decided { value, .. }
            | PropertyDecision::Agreed { value } => Some(value),
            PropertyDecision::Conflict | PropertyDecision::Undecided => None,
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
    /// Keep the report, write nothing: the subject does not exist yet and
    /// some property has no deciding report.
    Withheld,
}

impl<E: SqlEngine> StoreReader<'_, E> {
    /// The latest consumed report of each mapping on `object_type`/`key` at
    /// `frontier` (a commit; all when `None`).
    fn latest_reports(
        &self,
        object_type: &str,
        key: &str,
        frontier: Option<u64>,
    ) -> Result<BTreeMap<String, SourceReport>, CellError> {
        let frontier = frontier.map(|f| to_i64(f, "frontier")).transpose()?;
        let bound = i64::try_from(BOUND.saturating_add(1)).unwrap_or(i64::MAX);
        let rows = self.conn.query(
            &format!(
                "SELECT s.\"mapping\", s.\"commit_seq\", i.\"observation_id\", i.\"observed\" FROM \"{SOURCE_SUBJECT}\" s JOIN \"{INBOX}\" i ON i.\"observation_id\" = s.\"observation_id\" WHERE s.\"object_type\" = ?1 AND s.\"key\" = ?2 AND (?3 IS NULL OR s.\"commit_seq\" <= ?3) ORDER BY s.\"commit_seq\" DESC, s.\"position\" DESC LIMIT ?4"
            ),
            params![object_type, key, frontier, bound],
        )?;
        if rows.len() > BOUND {
            return Err(CellError::ComputeRefused("core.compute.bound_exceeded"));
        }
        let mut out = BTreeMap::new();
        for row in rows {
            let row = SqlRow(row);
            let mapping: String = row.get(0)?;
            if out.contains_key(&mapping) {
                continue;
            }
            let commit_seq = to_u64(row.get(1)?, "commit_seq")?;
            let stored_id: String = row.get(2)?;
            let observed: Observed<RuntimeValue> = serde_json::from_str(&row.get::<String>(3)?)?;
            let bundle: MappingBundle =
                norn_ir::runtime::decode(&observed.input).map_err(CellError::Encoding)?;
            if stored_id != format!("mapping-bundle:{}", bundle.digest)
                || bundle.input.mapping != mapping
            {
                return Err(CellError::MappingBundleMismatch);
            }
            if let Some(report) = report_of(&bundle, object_type, key, Some(commit_seq)) {
                out.insert(mapping, report);
            }
        }
        Ok(out)
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
        let Some(table) = self.active_resolution(&reports)? else {
            return Ok(None);
        };
        if !table.governs(object_type) {
            return Ok(None);
        }
        Ok(Some(decide(&table, object_type, &reports)))
    }

    /// The resolution table of the active definitions, read through the plan
    /// of any reporting mapping (every plan of one set carries the same
    /// table).
    fn active_resolution(
        &self,
        reports: &BTreeMap<String, SourceReport>,
    ) -> Result<Option<std::sync::Arc<ResolutionTable>>, CellError> {
        let Some(active) = self.active_definitions_version()? else {
            return Ok(None);
        };
        let Some(report) = reports.values().next() else {
            return Ok(None);
        };
        let input = crate::mapping_bundle::MappingInput {
            mapping: report.mapping.clone(),
            dataset: String::new(),
            connector_instance: String::new(),
            partition: String::new(),
            recorded_at: norn_ir::provenance::UtcTimestamp(String::new()),
        };
        match self.mapping_plan(&input, active) {
            Ok(plan) => Ok(Some(std::sync::Arc::clone(&plan.resolution))),
            // The mapping is no longer in the active set.
            Err(CellError::MappingBundleMismatch) => Ok(None),
            Err(error) => Err(error),
        }
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
        let mut reports = self.latest_reports(object_type, &record.key, None)?;
        let incoming = report_of(bundle, object_type, &record.key, None)
            .ok_or(CellError::UnsupportedMappingProfile)?;
        reports.insert(incoming.mapping.clone(), incoming);
        let resolution = decide(&plan.resolution, object_type, &reports);
        let mut values = RuntimeObject::new();
        for property in resolution.properties {
            let value = match property.decision {
                PropertyDecision::Single { value }
                | PropertyDecision::Decided { value, .. }
                | PropertyDecision::Agreed { value } => value,
                PropertyDecision::Conflict => RuntimeValue::Null,
                PropertyDecision::Undecided => {
                    if current.is_none() {
                        return Ok(Applied::Withheld);
                    }
                    continue;
                }
            };
            if current.and_then(|c| c.get(&property.property)) != Some(&value) {
                values.insert(property.property, value);
            }
        }
        Ok(Applied::Values(values))
    }
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
                values: BTreeMap::from([("q".to_owned(), RuntimeValue::from("one"))]),
                lineage: BTreeMap::new(),
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
