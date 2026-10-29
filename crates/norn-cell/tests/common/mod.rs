//! Shared fixtures for the `norn-cell` integration tests.

#![allow(
    clippy::expect_used,
    clippy::panic,
    dead_code,
    unreachable_pub,
    reason = "test support shared by several integration-test crates"
)]

use std::path::{Path, PathBuf};

use norn_cell::Model;
use norn_ir::{ObjectTypeDef, PropertyTypeDef};
use norn_projections::SqliteDdlInput;

pub fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

fn json_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("fixture directory")
        .map(|entry| entry.expect("entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();
    files
}

fn decode<T: serde::de::DeserializeOwned>(path: &Path) -> T {
    serde_json::from_str(&std::fs::read_to_string(path).expect("reads"))
        .unwrap_or_else(|error| panic!("{} decodes: {error}", path.display()))
}

/// Every object type of `fixtures/ir/entity-v2/object-types/`.
pub fn object_types() -> Vec<ObjectTypeDef> {
    json_files(&fixtures().join("ir/entity-v2/object-types"))
        .iter()
        .map(|path| decode(path))
        .collect()
}

/// Every shared property type of `fixtures/ir/entity-v2/property-types/`,
/// plus the registry value type they reference (`iso4217.currency_code`, a
/// `text` type until the registry lands).
pub fn property_types() -> Vec<PropertyTypeDef> {
    let mut defs: Vec<PropertyTypeDef> =
        json_files(&fixtures().join("ir/entity-v2/property-types"))
            .iter()
            .map(|path| decode(path))
            .collect();
    defs.push(
        serde_json::from_value(serde_json::json!({
            "name": "iso4217.currency_code",
            "type": { "kind": "text" }
        }))
        .expect("registry stand-in decodes"),
    );
    defs
}

/// The model of the whole fixture corpus.
pub fn corpus_model() -> Model {
    let object_types = object_types();
    let property_types = property_types();
    Model::new(&SqliteDdlInput {
        object_types: &object_types,
        property_types: &property_types,
    })
    .expect("the fixture corpus projects")
}

/// A model of `object_types` alone.
pub fn model_of(object_types: &[ObjectTypeDef]) -> Model {
    Model::new(&SqliteDdlInput {
        object_types,
        property_types: &[],
    })
    .expect("projects")
}

/// Copy the store file at `from` (with its `-wal` and `-journal` siblings,
/// if present) to `to`: the on-disk image a crash at this instant leaves.
pub fn crash_image(from: &Path, to: &Path) {
    for suffix in ["", "-wal", "-journal"] {
        let source = PathBuf::from(format!("{}{suffix}", from.display()));
        if source.exists() {
            std::fs::copy(&source, format!("{}{suffix}", to.display())).expect("copy");
        }
    }
}
