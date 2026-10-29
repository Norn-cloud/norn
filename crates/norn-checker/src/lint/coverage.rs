//! Mapping coverage: the one shared description of what a `MappingIR`
//! mapping fills on its target object type and which source fields it reads
//! (T8-LINT-1 #2593, T8-MAP-1b #2601).
//!
//! A [`MappingCoverage`] is computed **from** a mapping (the `MappingIR`
//! compile step, T8-MAP-1b, fills it) and **consumed** by the ontology lint
//! (the mapping-coverage rules of T8-LINT-1 and the source-mirroring smell of
//! T8-LINT-2). It is a plain value with no IO, so both sides agree on one
//! shape and neither re-derives coverage on its own.
//!
//! One coverage value describes one mapping into one target object type:
//!
//! - every member of the target the mapping accounts for, either
//!   [`MemberStatus::Mapped`] from zero or more source fields (zero for a
//!   constant or a pure transform) or an explicit [`MemberStatus::Gap`] with
//!   a reason ("every required property/link has a map or an explicit gap",
//!   T8-MAP-1b);
//! - the source it reads ([`SourceRef`]): the source schema, the source
//!   entity (table, sheet, `DocType`), and every field that entity has;
//! - the source fields it deliberately does not read
//!   ([`IgnoredSourceField`]), so an unread field is either ignored with a
//!   reason or reported as unmapped.
//!
//! Members are named by their typed ids ([`MemberRef`]); member and field
//! names are compared exactly, with no normalization.

use std::collections::BTreeSet;

use norn_ir::{LinkId, ObjectTypeName, PropertyId};

/// The coverage of one mapping into one target object type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappingCoverage {
    /// The `artifactId` of the mapping artifact; lint findings about this
    /// coverage are reported against it.
    pub mapping_artifact_id: String,
    /// The version of the mapping artifact.
    pub mapping_version: String,
    /// The source the mapping reads.
    pub source: SourceRef,
    /// The object type the mapping writes.
    pub target: ObjectTypeName,
    /// Every target member the mapping accounts for, in mapping order.
    pub members: Vec<MemberCoverage>,
    /// Source fields the mapping deliberately does not read.
    pub ignored_source_fields: Vec<IgnoredSourceField>,
}

/// The source side of a mapping: one entity of one discovered source schema
/// (`SourceSchemaIR`, T8-SRC-1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRef {
    /// The id of the source schema (`SourceSchemaIR` artifact id).
    pub schema_id: String,
    /// The source entity the mapping iterates: a table, a sheet, or a
    /// `DocType`.
    pub entity: String,
    /// Every field of that source entity, in source order.
    pub fields: Vec<String>,
}

/// A member of the target object type: a property or a link.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MemberRef {
    /// A property of the target (own or inherited).
    Property(PropertyId),
    /// A link of the target (own or inherited).
    Link(LinkId),
}

impl MemberRef {
    /// The member id as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Property(id) => id.as_str(),
            Self::Link(id) => id.as_str(),
        }
    }
}

/// How the mapping accounts for one target member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberCoverage {
    /// The member.
    pub member: MemberRef,
    /// Whether it is mapped or an explicit gap.
    pub status: MemberStatus,
}

/// Whether a target member is filled by the mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberStatus {
    /// Filled from these source fields (empty for a constant or a transform
    /// that reads no field).
    Mapped {
        /// The source fields the member's map reads.
        source_fields: Vec<String>,
    },
    /// Deliberately not filled by this mapping.
    Gap {
        /// Why the member is not mapped.
        reason: String,
    },
}

/// A source field the mapping deliberately does not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IgnoredSourceField {
    /// The source field.
    pub field: String,
    /// Why it is not read.
    pub reason: String,
}

impl MappingCoverage {
    /// The coverage entry of `member`, if the mapping accounts for it.
    #[must_use]
    pub fn member(&self, member: &MemberRef) -> Option<&MemberCoverage> {
        self.members.iter().find(|entry| &entry.member == member)
    }

    /// Whether the mapping accounts for `member` (mapped or an explicit gap).
    #[must_use]
    pub fn accounts_for(&self, member: &MemberRef) -> bool {
        self.member(member).is_some()
    }

    /// Every source field some member's map reads.
    #[must_use]
    pub fn read_source_fields(&self) -> BTreeSet<&str> {
        self.members
            .iter()
            .filter_map(|entry| match &entry.status {
                MemberStatus::Mapped { source_fields } => Some(source_fields),
                MemberStatus::Gap { .. } => None,
            })
            .flatten()
            .map(String::as_str)
            .collect()
    }

    /// The source fields of [`SourceRef::fields`] the mapping neither reads
    /// nor ignores, in source order.
    #[must_use]
    pub fn unaccounted_source_fields(&self) -> Vec<&str> {
        let read = self.read_source_fields();
        let ignored: BTreeSet<&str> = self
            .ignored_source_fields
            .iter()
            .map(|ignored| ignored.field.as_str())
            .collect();
        self.source
            .fields
            .iter()
            .map(String::as_str)
            .filter(|field| !read.contains(field) && !ignored.contains(field))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coverage() -> MappingCoverage {
        MappingCoverage {
            mapping_artifact_id: "erpnext.customer.mapping".to_owned(),
            mapping_version: "0.1.0".to_owned(),
            source: SourceRef {
                schema_id: "erpnext.schema".to_owned(),
                entity: "Customer".to_owned(),
                fields: ["name", "customer_name", "territory", "modified"]
                    .map(str::to_owned)
                    .to_vec(),
            },
            target: ObjectTypeName("crm.Customer".to_owned()),
            members: vec![
                MemberCoverage {
                    member: MemberRef::Property(PropertyId("display_name".to_owned())),
                    status: MemberStatus::Mapped {
                        source_fields: vec!["customer_name".to_owned()],
                    },
                },
                MemberCoverage {
                    member: MemberRef::Link(LinkId("region".to_owned())),
                    status: MemberStatus::Gap {
                        reason: "territories are not modelled yet".to_owned(),
                    },
                },
            ],
            ignored_source_fields: vec![IgnoredSourceField {
                field: "modified".to_owned(),
                reason: "source bookkeeping".to_owned(),
            }],
        }
    }

    #[test]
    fn members_are_accounted_for_when_mapped_or_gapped() {
        let coverage = coverage();
        assert!(coverage.accounts_for(&MemberRef::Property(PropertyId("display_name".to_owned()))));
        assert!(coverage.accounts_for(&MemberRef::Link(LinkId("region".to_owned()))));
        assert!(!coverage.accounts_for(&MemberRef::Property(PropertyId("region".to_owned()))));
    }

    #[test]
    fn unaccounted_fields_are_neither_read_nor_ignored() {
        let coverage = coverage();
        assert_eq!(
            coverage
                .read_source_fields()
                .into_iter()
                .collect::<Vec<_>>(),
            vec!["customer_name"]
        );
        assert_eq!(
            coverage.unaccounted_source_fields(),
            vec!["name", "territory"]
        );
    }
}
