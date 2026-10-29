#![allow(
    clippy::pedantic,
    missing_docs,
    reason = "exhaustive reference table for a security-critical pure guard"
)]
#![deny(
    clippy::match_wildcard_for_single_variants,
    clippy::format_collect,
    clippy::format_push_string,
    clippy::inefficient_to_string,
    clippy::large_types_passed_by_value,
    clippy::needless_pass_by_value,
    clippy::large_futures
)]

use norn_ir::{ArtifactKind, ArtifactLifecycle, direct_artifact_write_allowed};

#[test]
fn every_authority_bearing_kind_requires_governed_approval_and_activation() {
    let authority_bearing = [
        ArtifactKind::Entity,
        ArtifactKind::Relation,
        ArtifactKind::Action,
        ArtifactKind::Rule,
        ArtifactKind::Connector,
        ArtifactKind::Customization,
        ArtifactKind::Setting,
        ArtifactKind::BusinessPack,
        ArtifactKind::Policy,
        ArtifactKind::Widget,
        ArtifactKind::Derived,
        ArtifactKind::Mapping,
        ArtifactKind::Law,
    ];

    for kind in authority_bearing {
        assert!(direct_artifact_write_allowed(
            &kind,
            &ArtifactLifecycle::Draft
        ));
        assert!(direct_artifact_write_allowed(
            &kind,
            &ArtifactLifecycle::Proposed
        ));
        assert!(!direct_artifact_write_allowed(
            &kind,
            &ArtifactLifecycle::Approved
        ));
        assert!(!direct_artifact_write_allowed(
            &kind,
            &ArtifactLifecycle::Active
        ));
    }
}

#[test]
fn non_authoritative_lifecycles_remain_directly_writable() {
    for kind in [
        ArtifactKind::Entity,
        ArtifactKind::Relation,
        ArtifactKind::Action,
        ArtifactKind::Rule,
        ArtifactKind::Connector,
        ArtifactKind::Customization,
        ArtifactKind::Setting,
        ArtifactKind::BusinessPack,
        ArtifactKind::Policy,
        ArtifactKind::Widget,
        ArtifactKind::Derived,
        ArtifactKind::SourceSchema,
        ArtifactKind::Mapping,
        ArtifactKind::Law,
    ] {
        assert!(direct_artifact_write_allowed(
            &kind,
            &ArtifactLifecycle::Deprecated
        ));
        assert!(direct_artifact_write_allowed(
            &kind,
            &ArtifactLifecycle::Superseded
        ));
        assert!(direct_artifact_write_allowed(
            &kind,
            &ArtifactLifecycle::Rejected
        ));
    }
}
