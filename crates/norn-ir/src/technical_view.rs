//! The technical-view vocabulary (T12-VIEW-1, #2717; ruling 2026-09-25 §18
//! "One object definition, two projections").
//!
//! The OpenMetadata-shaped technical vocabulary is ordinary `EntityIR` object
//! types in the [`TECHNICAL_NAMESPACE`] namespace ([`TechnicalType`]):
//! `Source`, `Dataset`, `Table`, `Column`, `Pipeline`, `Mapping`,
//! `QualityCheck`, `Owner`, and `Domain`. Business types link to them through
//! the two lineage relation families ([`LineageRelation`]): `backed_by` (the
//! source tables a type's facts come from) and `derived_from` (the mapping
//! versions that produce them, and the columns a mapping reads). Lineage is
//! therefore a traversal over ordinary links, not a separate lineage store.
//!
//! This module only names the vocabulary. The definitions themselves are
//! `ObjectTypeDef`s (fixtures under `fixtures/ir/entity-v2/technical-view/`),
//! checked by `norn-checker` like any other type; projections
//! (`norn-projections::lineage`) split one artifact set into its business and
//! technical views by these names.

use crate::{LinkId, ObjectTypeName};

/// The namespace of the technical-view types.
pub const TECHNICAL_NAMESPACE: &str = "technical";

/// One technical-view type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TechnicalType {
    /// A system of record (`technical.Source`); its key is a provenance
    /// source record's `system`.
    Source,
    /// A collection of tables in one source (`technical.Dataset`).
    Dataset,
    /// One table, `DocType`, or resource (`technical.Table`).
    Table,
    /// One column or field of a table (`technical.Column`).
    Column,
    /// A job that replays mappings (`technical.Pipeline`).
    Pipeline,
    /// One mapping version (`technical.Mapping`); its key is a provenance
    /// record's `mappingVersion`.
    Mapping,
    /// A data-quality expectation on a column (`technical.QualityCheck`).
    QualityCheck,
    /// An accountable person or team (`technical.Owner`).
    Owner,
    /// A business or data domain (`technical.Domain`).
    Domain,
}

impl TechnicalType {
    /// Every technical-view type.
    pub const ALL: [Self; 9] = [
        Self::Source,
        Self::Dataset,
        Self::Table,
        Self::Column,
        Self::Pipeline,
        Self::Mapping,
        Self::QualityCheck,
        Self::Owner,
        Self::Domain,
    ];

    /// The qualified type name (`technical.Table`).
    #[must_use]
    pub const fn type_name(self) -> &'static str {
        match self {
            Self::Source => "technical.Source",
            Self::Dataset => "technical.Dataset",
            Self::Table => "technical.Table",
            Self::Column => "technical.Column",
            Self::Pipeline => "technical.Pipeline",
            Self::Mapping => "technical.Mapping",
            Self::QualityCheck => "technical.QualityCheck",
            Self::Owner => "technical.Owner",
            Self::Domain => "technical.Domain",
        }
    }

    /// The technical-view type named `name`, if any.
    #[must_use]
    pub fn of(name: &ObjectTypeName) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.type_name() == name.as_str())
    }
}

/// Whether `name` is in the technical-view namespace.
#[must_use]
pub fn is_technical_type(name: &ObjectTypeName) -> bool {
    name.domain() == Some(TECHNICAL_NAMESPACE)
}

/// The two lineage relation families.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LineageRelation {
    /// `backedBy`: the source tables (or columns) a type's facts come from.
    BackedBy,
    /// `derivedFrom`: what a type or a mapping is computed from.
    DerivedFrom,
}

impl LineageRelation {
    /// Both families.
    pub const ALL: [Self; 2] = [Self::BackedBy, Self::DerivedFrom];

    /// The link id that declares the relation (`backed_by`, `derived_from`).
    #[must_use]
    pub const fn link_id(self) -> &'static str {
        match self {
            Self::BackedBy => "backed_by",
            Self::DerivedFrom => "derived_from",
        }
    }

    /// The ruling's relation name (`backedBy`, `derivedFrom`).
    #[must_use]
    pub const fn relation_name(self) -> &'static str {
        match self {
            Self::BackedBy => "backedBy",
            Self::DerivedFrom => "derivedFrom",
        }
    }

    /// The family a link id declares, if any.
    #[must_use]
    pub fn of(link: &LinkId) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.link_id() == link.as_str())
    }
}
