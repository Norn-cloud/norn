//! Read hash-checked ephemeral protocol/stock inputs generated before testing.

use std::path::{Path, PathBuf};

use norn_ir::ArtifactEnvelope;
use serde_json::Value;
use sha2::{Digest as _, Sha256};

/// Upstream schemas and generated stock catalogs must remain outside the repo.
pub(super) fn runtime_root() -> PathBuf {
    let root = std::env::var_os("NORN_UPSTREAM_FIXTURE_DIR")
        .map(PathBuf::from)
        .expect("generate pinned runtime inputs on Boat with scripts/importers/erpnext_runtime.py --output EXTERNAL_SCRATCH --stock; set NORN_UPSTREAM_FIXTURE_DIR")
        .canonicalize()
        .expect("runtime input directory exists");
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root");
    assert!(
        !root.starts_with(repo),
        "vendor test inputs cannot be bundled"
    );
    root
}

/// Load the real core package or the separately generated runtime test pack.
pub(super) fn package_artifacts(directory: &Path) -> Vec<ArtifactEnvelope> {
    let directory = if directory.ends_with("packs/erpnext-norn-core") {
        runtime_root().join("pack")
    } else {
        directory.to_path_buf()
    };
    let manifest: Value = serde_json::from_slice(
        &std::fs::read(directory.join("norn-package.json")).expect("package manifest"),
    )
    .expect("package JSON");
    let artifacts: Vec<ArtifactEnvelope> = manifest
        .get("artifacts")
        .and_then(Value::as_array)
        .expect("artifact inventory")
        .iter()
        .map(|entry| {
            let relative = entry
                .get("path")
                .and_then(Value::as_str)
                .expect("artifact path");
            assert!(Path::new(relative).is_relative());
            assert!(
                !Path::new(relative)
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir))
            );
            let bytes = std::fs::read(directory.join(relative)).expect("runtime artifact");
            assert_eq!(
                entry
                    .get("sha256")
                    .and_then(Value::as_str)
                    .expect("artifact digest"),
                format!("{:x}", Sha256::digest(&bytes)),
                "runtime artifact digest"
            );
            serde_json::from_slice(&bytes).expect("runtime artifact decodes")
        })
        .collect();
    artifacts
}

/// Permission tests consume a small protocol closure, independently of stock.
pub(super) fn protocol_artifacts() -> Vec<ArtifactEnvelope> {
    package_artifacts(&PathBuf::from("packs/erpnext-norn-core"))
        .into_iter()
        .filter(|artifact| {
            matches!(
                artifact.artifact_id.as_str(),
                "erpnext.source_schema"
                    | "erpnext.norn_core"
                    | "erpnext.commerce.SalesOrder.owner.extension"
                    | "erpnext.commerce.SalesInvoice.owner.extension"
                    | "erpnext.connector.norn_core"
            )
        })
        .collect()
}
