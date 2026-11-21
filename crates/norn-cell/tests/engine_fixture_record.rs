//! Records `tests/fixtures/engine-storage-v<N>.json`, the committed cell the
//! upgrade test opens (ENGINE-UPGRADE-1, #1494). Run it on the release whose
//! storage version `N` it records, before that release's successor lands:
//!
//! `NORN_RECORD_ENGINE_FIXTURE=$PWD/crates/norn-cell/tests/fixtures/engine-storage-vN.json \
//!   cargo test -p norn-cell --lib suites::engine_fixture_record -- --ignored`
//!
//! It uses only APIs every storage version has, so it builds unchanged
//! against an older checkout. `engine-storage-v1.json` was recorded this way
//! on `main` at 962938d9d8, the last storage-version-1 engine.
#![cfg(feature = "native")]
#![allow(clippy::expect_used, missing_docs, reason = "fixture recorder")]

#[path = "support/engine_fixture.rs"]
mod engine_fixture;

#[test]
#[ignore = "records a fixture for a later engine's upgrade test; run on the release it records"]
fn record_engine_storage_fixture() {
    let out = std::env::var_os("NORN_RECORD_ENGINE_FIXTURE").expect("fixture output path");
    let dir = tempfile::tempdir().expect("directory");
    let cell = dir.path().join("cell.sqlite");
    engine_fixture::write_cell(&cell);
    let image = engine_fixture::image_of(&cell);
    let text = serde_json::to_string_pretty(&image).expect("image") + "\n";
    std::fs::write(out, text).expect("write fixture");
}
