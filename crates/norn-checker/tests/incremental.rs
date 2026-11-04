#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "differential admission fixtures"
)]
use std::collections::BTreeSet;
use std::time::Instant;

use norn_checker::incremental::{Admission, AdmissionCache, EvidenceOrigin};
use norn_checker::{EntityArtifactSetContext, check_artifact_set, diagnostics_to_json};
use norn_ir::ArtifactEnvelope;
use proptest::prelude::*;

#[path = "../../../registry/core/src/units.rs"]
mod units;

fn registry() -> BTreeSet<norn_ir::PropertyTypeName> {
    [
        "core.code",
        "core.quantity",
        "ucum.unit_code",
        "core.display_name",
        "iso8601.timestamp",
    ]
    .map(|name| norn_ir::PropertyTypeName(name.into()))
    .into_iter()
    .collect()
}

fn fixture() -> Vec<ArtifactEnvelope> {
    let mut pool = vec![units::pack().expect("units")];
    for text in [
        include_str!("../../../registry/core/artifacts/core.Commitment.interface.json"),
        include_str!("../../../registry/core/artifacts/core.Resource.interface.json"),
        include_str!("../../../registry/core/artifacts/core.EconomicEvent.interface.json"),
        include_str!("../../../registry/core/artifacts/core.Agent.interface.json"),
    ] {
        pool.push(serde_json::from_str(text).expect("REA interface"));
    }
    pool.extend(
        serde_json::from_str::<Vec<ArtifactEnvelope>>(include_str!(
            "../../../tools/norn-ts/fixtures/definitions/jewelry-pricing.ir.json"
        ))
        .expect("jewelry"),
    );
    pool
}

fn compare(
    cache: &mut AdmissionCache,
    pool: &[ArtifactEnvelope],
    context: &EntityArtifactSetContext<'_>,
) -> Admission {
    let incremental = cache.check(pool, context);
    let full = check_artifact_set(pool, context);
    assert_eq!(
        diagnostics_to_json(&incremental.findings),
        diagnostics_to_json(&full)
    );
    // A cold cache supplies independent freshly checked evidence identities.
    let fresh = AdmissionCache::new(&[7; 32], "structural", "no solver").check(pool, context);
    assert_eq!(
        incremental
            .evidence
            .iter()
            .map(|e| (&e.artifact_id, &e.closure_digest))
            .collect::<Vec<_>>(),
        fresh
            .evidence
            .iter()
            .map(|e| (&e.artifact_id, &e.closure_digest))
            .collect::<Vec<_>>()
    );
    incremental
}

#[test]
fn dependency_edits_and_new_writers_cannot_reuse_global_evidence() {
    let registry = registry();
    let context = EntityArtifactSetContext {
        registry: &registry,
        base: &[],
        tenant: None,
    };
    let mut pool = fixture();
    let mut cache = AdmissionCache::new(&[7; 32], "structural", "no solver");
    let initial = compare(&mut cache, &pool, &context);
    assert!(
        !initial
            .findings
            .iter()
            .any(norn_checker::Diagnostic::is_error),
        "{:?}",
        initial.findings
    );
    let repeated = cache.check(&pool, &context);
    assert!(
        repeated
            .evidence
            .iter()
            .all(|e| matches!(e.origin, EvidenceOrigin::ReusedFromDigest(_)))
    );
    let last = pool.len() - 1;
    pool[last].dependencies[0].artifact_id = "missing.definition".into();
    let edited = compare(&mut cache, &pool, &context);
    assert!(
        edited
            .findings
            .iter()
            .any(norn_checker::Diagnostic::is_error)
    );
    assert_eq!(
        edited
            .evidence
            .iter()
            .filter(|e| e.origin == EvidenceOrigin::Rechecked)
            .count(),
        2
    );
    pool.push(pool[last].clone());
    compare(&mut cache, &pool, &context);
}

#[test]
fn conflicting_versions_retain_independent_local_evidence() {
    let registry = registry();
    let context = EntityArtifactSetContext {
        registry: &registry,
        base: &[],
        tenant: None,
    };
    let mut pool = fixture();
    let mut conflicting = pool.last().expect("writer").clone();
    conflicting.version = "2.0.0".into();
    pool.push(conflicting);
    let mut cache = AdmissionCache::new(&[7; 32], "structural", "no solver");
    let initial = compare(&mut cache, &pool, &context);
    let id = pool.last().expect("writer").artifact_id.clone();
    let versions: Vec<_> = initial
        .evidence
        .iter()
        .filter(|e| e.artifact_id.as_ref() == Some(&id))
        .collect();
    assert_ne!(versions[0].closure_digest, versions[1].closure_digest);
    pool[0].name = "changed catalog label".into();
    let result = compare(&mut cache, &pool, &context);
    for (job, old) in result
        .evidence
        .iter()
        .filter(|e| e.artifact_id.as_ref() == Some(&id))
        .zip(versions)
    {
        assert_eq!(
            job.origin,
            EvidenceOrigin::ReusedFromDigest(old.closure_digest.clone().expect("digest"))
        );
    }
    assert_eq!(
        result
            .evidence
            .iter()
            .filter(|e| e.origin == EvidenceOrigin::Rechecked)
            .count(),
        2
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn generated_edits_equal_full_admission(edits in prop::collection::vec((0_u8..9, any::<u8>()), 1..32)) {
        let registry = registry();
        let mut cache = AdmissionCache::new(&[7;32], "structural", "no solver");
        let original = fixture();
        let mut pool = original.clone();
        let mut base = Vec::new();
        for (op, value) in edits {
            let i = usize::from(value).checked_rem(pool.len()).expect("nonempty pool");
            match op {
                0 => pool[i].name = if value % 2 == 0 { String::new() } else { format!("edited {value}") },
                1 => pool[i].version = format!("{value}.0.0"),
                2 => pool[i].scope.tenant_id = Some(format!("tenant{value}")),
                3 => { pool[i].dependencies.clear(); },
                4 => { pool.rotate_left(1); },
                5 if pool.len() > 1 => { pool.remove(i); },
                6 => { pool.push(original[usize::from(value).checked_rem(original.len()).expect("fixture")].clone()); },
                7 => { base = if value % 2 == 0 { original.clone() } else { Vec::new() }; },
                _ => { pool = original.clone(); },
            }
            let context = EntityArtifactSetContext { registry: &registry, base: &base, tenant: None };
            compare(&mut cache, &pool, &context);
        }
    }
}

#[test]
fn registry_tenant_and_base_are_global_inputs() {
    let registry = registry();
    let mut changed_registry = registry.clone();
    changed_registry.insert(norn_ir::PropertyTypeName("external.value".into()));
    let pool = fixture();
    let base = [pool[0].clone()];
    let namespace = norn_ir::ExtensionNamespace("test".into());
    let original = EntityArtifactSetContext {
        registry: &registry,
        base: &[],
        tenant: None,
    };
    for context in [
        EntityArtifactSetContext {
            registry: &changed_registry,
            ..original
        },
        EntityArtifactSetContext {
            tenant: Some(norn_checker::entity_artifacts::TenantLayer {
                tenant_id: "customer",
                namespace: &namespace,
            }),
            ..original
        },
        EntityArtifactSetContext {
            base: &base,
            ..original
        },
    ] {
        let mut cache = AdmissionCache::new(&[7; 32], "structural", "no solver");
        compare(&mut cache, &pool, &original);
        let result = compare(&mut cache, &pool, &context);
        assert_eq!(
            result.evidence.last().expect("global").origin,
            EvidenceOrigin::Rechecked
        );
    }
}

#[test]
#[ignore = "explicit measurement; run with --ignored --nocapture"]
fn admission_change_size_measurement() {
    let registry = registry();
    let context = EntityArtifactSetContext {
        registry: &registry,
        base: &[],
        tenant: None,
    };
    let original = fixture();
    for changed in [0, 1, 2, 4, original.len()] {
        let mut cache = AdmissionCache::new(&[7; 32], "structural", "no solver");
        cache.check(&original, &context);
        let mut incremental_ns = 0;
        let mut full_ns = 0;
        let mut rechecked = 0;
        for iteration in 0..30 {
            let mut pool = original.clone();
            for artifact in pool.iter_mut().take(changed) {
                artifact.name = format!("edit {iteration}");
            }
            let start = Instant::now();
            let result = cache.check(&pool, &context);
            incremental_ns += start.elapsed().as_nanos();
            rechecked = result
                .evidence
                .iter()
                .filter(|e| e.origin == EvidenceOrigin::Rechecked)
                .count();
            let start = Instant::now();
            let full = check_artifact_set(&pool, &context);
            full_ns += start.elapsed().as_nanos();
            assert_eq!(
                diagnostics_to_json(&result.findings),
                diagnostics_to_json(&full)
            );
        }
        println!(
            "definitions={} changed={changed} rechecked={rechecked} incremental_mean_us={} full_mean_us={}",
            original.len(),
            incremental_ns / 30 / 1000,
            full_ns / 30 / 1000
        );
    }
}
