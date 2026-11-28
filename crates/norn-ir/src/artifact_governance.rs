//! Pure direct-write lifecycle admission guard for canonical IR artifacts.
//!
//! Authority-bearing definitions may be drafted or proposed directly, but the
//! `Approved` and `Active` states are reserved for the governed validation,
//! review, and apply state machine. `SourceSchema` is evidence-only and remains
//! directly writable in every lifecycle state.
//!
//! Verus verifies the exhaustive 17-kind x 7-lifecycle truth table directly on
//! this file (`scripts/verify-kernel-inline-verus.sh`).

#![deny(clippy::default_numeric_fallback)]

#[cfg(not(verus_keep_ghost))]
use schemars::JsonSchema;
#[cfg(not(verus_keep_ghost))]
use serde::{Deserialize, Serialize};
#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

#[cfg_attr(
    not(verus_keep_ghost),
    derive(
        Debug,
        Clone,
        PartialEq,
        Eq,
        PartialOrd,
        Ord,
        Hash,
        Serialize,
        Deserialize,
        JsonSchema,
    )
)]
#[cfg_attr(not(verus_keep_ghost), serde(rename_all = "snake_case"))]
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub enum ArtifactKind {
    Entity,
    Relation,
    Action,
    Rule,
    Connector,
    Widget,
    Customization,
    Setting,
    BusinessPack,
    Policy,
    /// ViewIR v0 — pure, deterministic, recompute-on-read computed view.
    /// Declared over Source-bearing entity/relation facts and other Derived views.
    /// No IO, no nondeterminism, no side effects.
    Derived,
    /// SourceSchemaIR (T8-SRC-1): what discovery found in a source system.
    /// Evidence, never mapped directly to the ontology.
    SourceSchema,
    /// MappingIR (T8-MAP-1a): an interface-scoped mapping from one source
    /// schema to EntityIR objects.
    Mapping,
    /// Scoped temporal laws over ZEN predicates.
    Law,
    /// Pure classification over existing base identities.
    DefinedConcept,
    /// Localized labels for definitions; never part of a definition's
    /// identity ([`crate::labels`]).
    Labels,
    /// `ModelIR`: a typed predictive model with opaque internals
    /// ([`crate::model`]).
    Model,
}

#[cfg_attr(
    not(verus_keep_ghost),
    derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)
)]
#[cfg_attr(not(verus_keep_ghost), serde(rename_all = "snake_case"))]
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub enum ArtifactLifecycle {
    Draft,
    Proposed,
    Approved,
    Active,
    Deprecated,
    Superseded,
    Rejected,
}

#[cfg(verus_keep_ghost)]
verus! {
    /// Closed classification of all 17 `ArtifactKind` variants. `SourceSchema`
    /// is evidence-only and is not authority-bearing; every other variant
    /// (including `Mapping`, `DefinedConcept`, `Labels`, `Model`, and `Law`) is
    /// authority-bearing.
    pub open spec fn authority_bearing(kind: ArtifactKind) -> bool {
        match kind {
            ArtifactKind::Entity
            | ArtifactKind::DefinedConcept
            | ArtifactKind::Labels
            | ArtifactKind::Model
            | ArtifactKind::Relation
            | ArtifactKind::Action
            | ArtifactKind::Rule
            | ArtifactKind::Law
            | ArtifactKind::Connector
            | ArtifactKind::Widget
            | ArtifactKind::Customization
            | ArtifactKind::Setting
            | ArtifactKind::BusinessPack
            | ArtifactKind::Policy
            | ArtifactKind::Derived
            | ArtifactKind::Mapping => true,
            ArtifactKind::SourceSchema => false,
        }
    }

    /// Closed classification of all 7 `ArtifactLifecycle` variants. Only
    /// `Approved` and `Active` grant runtime authority.
    pub open spec fn grants_runtime_authority(lifecycle: ArtifactLifecycle) -> bool {
        match lifecycle {
            ArtifactLifecycle::Approved | ArtifactLifecycle::Active => true,
            ArtifactLifecycle::Draft
            | ArtifactLifecycle::Proposed
            | ArtifactLifecycle::Deprecated
            | ArtifactLifecycle::Superseded
            | ArtifactLifecycle::Rejected => false,
        }
    }

    /// Exact total specification for direct catalog-write admission over all
    /// 17 `ArtifactKind` x 7 `ArtifactLifecycle` combinations (119 cases).
    pub open spec fn expected_direct_write(
        kind: ArtifactKind,
        lifecycle: ArtifactLifecycle,
    ) -> bool {
        !(authority_bearing(kind) && grants_runtime_authority(lifecycle))
    }

    proof fn active_authority_bearing_rejected(kind: ArtifactKind)
        requires authority_bearing(kind),
        ensures !expected_direct_write(kind, ArtifactLifecycle::Active),
    {}

    proof fn approved_authority_bearing_rejected(kind: ArtifactKind)
        requires authority_bearing(kind),
        ensures !expected_direct_write(kind, ArtifactLifecycle::Approved),
    {}

    proof fn source_schema_always_directly_writable(lifecycle: ArtifactLifecycle)
        ensures expected_direct_write(ArtifactKind::SourceSchema, lifecycle),
    {}

    proof fn non_authoritative_lifecycle_directly_writable(
        kind: ArtifactKind,
        lifecycle: ArtifactLifecycle,
    )
        requires !grants_runtime_authority(lifecycle),
        ensures expected_direct_write(kind, lifecycle),
    {}

    proof fn direct_artifact_write_truth_table(
        kind: ArtifactKind,
        lifecycle: ArtifactLifecycle,
    )
        ensures
            expected_direct_write(kind, lifecycle)
                == !(authority_bearing(kind) && grants_runtime_authority(lifecycle)),
            authority_bearing(kind) && grants_runtime_authority(lifecycle)
                ==> !expected_direct_write(kind, lifecycle),
            !authority_bearing(kind) ==> expected_direct_write(kind, lifecycle),
            !grants_runtime_authority(lifecycle) ==> expected_direct_write(kind, lifecycle),
    {}
}

/// Returns whether the direct authoring/catalog path may persist this lifecycle.
///
/// Authority-bearing definitions may be drafted or proposed directly, but the
/// Approved/Active states are reserved for the governed validation, review, and
/// apply state machine. This function is deliberately pure so Verus verifies
/// the exact production guard directly on this source file.
#[must_use]
#[allow(
    clippy::match_like_matches_macro,
    reason = "explicit exhaustive match for Verus verification and compiler exhaustiveness on new variants"
)]
#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures
    #![verifier::proof_note("direct_artifact_write_truth_table")]
    r == expected_direct_write(*kind, *lifecycle),
))]
pub const fn direct_artifact_write_allowed(
    kind: &ArtifactKind,
    lifecycle: &ArtifactLifecycle,
) -> bool {
    let authority_bearing = match *kind {
        ArtifactKind::Entity
        | ArtifactKind::Relation
        | ArtifactKind::Action
        | ArtifactKind::Rule
        | ArtifactKind::Connector
        | ArtifactKind::Customization
        | ArtifactKind::Setting
        | ArtifactKind::BusinessPack
        | ArtifactKind::Policy
        | ArtifactKind::Widget
        | ArtifactKind::Derived
        | ArtifactKind::Mapping
        | ArtifactKind::Law
        | ArtifactKind::DefinedConcept
        | ArtifactKind::Labels
        | ArtifactKind::Model => true,
        ArtifactKind::SourceSchema => false,
    };
    let grants_runtime_authority = match *lifecycle {
        ArtifactLifecycle::Approved | ArtifactLifecycle::Active => true,
        ArtifactLifecycle::Draft
        | ArtifactLifecycle::Proposed
        | ArtifactLifecycle::Deprecated
        | ArtifactLifecycle::Superseded
        | ArtifactLifecycle::Rejected => false,
    };

    !(authority_bearing && grants_runtime_authority)
}
