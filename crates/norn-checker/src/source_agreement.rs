//! SOURCE-AGREE-1: two sources report different values of a measure's
//! partition source (its unit, currency or basis) for one subject.
//!
//! The rule is pure. The cell reads a subject's verified versions and their
//! provenance; [`changes`] names the consecutive versions whose partition
//! values differ, the cell keeps the pairs two different source systems
//! wrote, and [`finding`] builds the warning. A disagreement never excludes
//! its subject: both reports are true facts about what each source says.

use std::collections::BTreeMap;

use norn_ir::canonical::CanonicalJsonError;
use norn_ir::provenance::{InstanceProvenance, UtcTimestamp};
use norn_ir::{
    DataQualityFinding, DataQualityFindingStatus, DataQualityLineage, DataQualityOutcome,
    DataQualityRuleRef, DataQualitySeverity, DataQualitySubject, ObjectTypeDef, PropertyId,
    PropertyType, SourceClaim,
};

/// SOURCE-AGREE-1's registered code; a finding carries its message.
pub const SOURCE_DISAGREEMENT: &str = "core.observation.source_disagreement";

/// HOST-READ-4b's bound: the most disputed subjects one read examines and
/// counts. Past it the count is a lower bound.
pub const BOUND: u64 = 500;

/// The sibling properties that hold the unit, currency or basis of a numeric
/// property `object` declares (`unitSource`, a direct `currencySource`), in
/// declaration order, each once.
#[must_use]
pub fn partition_sources(object: &ObjectTypeDef) -> Vec<PropertyId> {
    let mut sources: Vec<PropertyId> = Vec::new();
    for property in &object.properties {
        let declared: Vec<PropertyId> = match &property.property_type {
            PropertyType::Decimal {
                unit_source,
                currency_source,
                ..
            } => unit_source
                .iter()
                .cloned()
                .chain(
                    currency_source
                        .iter()
                        .filter(|path| path.links.is_empty())
                        .map(|path| path.property.clone()),
                )
                .collect(),
            PropertyType::Ref { unit_source, .. } => unit_source.clone(),
            PropertyType::Text { .. }
            | PropertyType::Integer { .. }
            | PropertyType::Boolean { .. }
            | PropertyType::Timestamp { .. }
            | PropertyType::Date { .. }
            | PropertyType::Duration { .. }
            | PropertyType::Enum { .. }
            | PropertyType::Record { .. }
            | PropertyType::JsonExt { .. }
            | PropertyType::DecimalBinding { .. } => Vec::new(),
        };
        for source in declared {
            if !sources.contains(&source) {
                sources.push(source);
            }
        }
    }
    sources
}

/// One change of a partition source between consecutive versions: the
/// property and the indices of the earlier and later version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// The partition source whose value changed.
    pub property: PropertyId,
    /// Index of the earlier version.
    pub earlier: usize,
    /// Index of the later version (`earlier + 1`).
    pub later: usize,
}

/// The changes of `partition` across `versions` (each a version's partition
/// values, oldest first; `None` is null or absent) from one reported value to
/// another. A source that reports no value (a mapping gap) disagrees with no
/// one, so a change to or from null is not one.
#[must_use]
pub fn changes(
    versions: &[BTreeMap<PropertyId, Option<String>>],
    partition: &[PropertyId],
) -> Vec<Change> {
    let mut found = Vec::new();
    for (later, pair) in versions.windows(2).enumerate() {
        let [before, after] = pair else { continue };
        for property in partition {
            let (Some(Some(was)), Some(Some(now))) = (before.get(property), after.get(property))
            else {
                continue;
            };
            if was != now {
                found.push(Change {
                    property: property.clone(),
                    earlier: later,
                    later: later.saturating_add(1),
                });
            }
        }
    }
    found
}

/// The finding for two sources' reports of `property`. It is open while the
/// value at the read's frontier (`current`) differs from the earlier report:
/// it resolves when the subject returns to it, and a later change by another
/// source is a finding of its own.
///
/// # Errors
/// The finding identity cannot be serialized.
pub fn finding(
    subject: DataQualitySubject,
    property: PropertyId,
    (earlier, earlier_provenance): (SourceClaim, Option<InstanceProvenance>),
    (later, later_provenance): (SourceClaim, Option<InstanceProvenance>),
    current: Option<&str>,
    seen: UtcTimestamp,
) -> Result<DataQualityFinding, CanonicalJsonError> {
    let status = if current == earlier.value.as_deref() {
        DataQualityFindingStatus::Resolved
    } else {
        DataQualityFindingStatus::Open
    };
    let message = crate::codes::lookup(SOURCE_DISAGREEMENT)
        .map_or(SOURCE_DISAGREEMENT, |entry| entry.message);
    let provenance = [earlier_provenance, later_provenance]
        .into_iter()
        .flatten()
        .map(|provenance| DataQualityLineage::new(provenance, Vec::new()))
        .collect();
    DataQualityFinding::v2(
        DataQualityRuleRef::Builtin {
            id: SOURCE_DISAGREEMENT.to_owned(),
            version: "1".to_owned(),
        },
        vec![subject],
        DataQualitySeverity::Warning,
        message,
        DataQualityOutcome::SourceDisagreement {
            property,
            earlier,
            later,
        },
        provenance,
        seen.clone(),
        seen,
        status,
    )
}
