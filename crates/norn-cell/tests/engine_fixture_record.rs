//! Records `tests/fixtures/engine-storage-v<N>.json`, the committed cell the
//! upgrade test opens (ENGINE-UPGRADE-1, #1494). Run it on the release whose
//! storage version `N` it records, before that release's successor lands:
//!
//! `NORN_RECORD_ENGINE_FIXTURE=$PWD/crates/norn-cell/tests/fixtures/engine-storage-vN.json \
//!   cargo test -p norn-cell --lib suites::engine_fixture_record::record_engine_storage_fixture -- --exact --ignored`
//!
//! For v7 (marked + declassified fixture), run on the last v7 engine (`origin/main`):
//! `NORN_RECORD_ENGINE_FIXTURE=$PWD/crates/norn-cell/tests/fixtures/engine-storage-v7.json \
//!   cargo test -p norn-cell --lib suites::engine_fixture_record::record_engine_storage_v7_fixture -- --exact --ignored`
//!
//! For v6, apply this recorder and the test-only checkpoint options in
//! `support/engine_fixture.rs` to the named v6 checkout before recording.
//!
//! `engine-storage-v1.json` was recorded on `main` at 962938d9d8, the last storage-version-1 engine, and
//! `engine-storage-v2.json` on `main` at 592458be94, the last
//! storage-version-2 engine, `engine-storage-v3.json` on `main` at
//! 3931329a9a, the last storage-version-3 engine, `engine-storage-v4.json`
//! at e09c0f2e25, the last storage-version-4 engine, and
//! `engine-storage-v5.json` on `main` at 919482b548, the last
//! storage-version-5 engine, `engine-storage-v6.json` on `main` at
//! 3a8dea5338, the OPEN-CHECKPOINT-1 storage-version-6 engine, and
//! `engine-storage-v7.json` on `main` at 39e0c3e6ce, the last
//! storage-version-7 engine.
#![cfg(feature = "native")]
#![allow(clippy::expect_used, missing_docs, reason = "fixture recorder")]

use crate::engine::SqlEngine as _;

#[path = "support/engine_fixture.rs"]
mod engine_fixture;

#[test]
#[ignore = "records a fixture for a later engine's upgrade test; run on the release it records"]
fn record_engine_storage_fixture() {
    let out = std::env::var_os("NORN_RECORD_ENGINE_FIXTURE").expect("fixture output path");
    let dir = tempfile::tempdir().expect("directory");
    let cell = dir.path().join("cell.sqlite");
    engine_fixture::write_cell(&cell);
    let store = norn_cell::CellStore::open(
        &cell,
        engine_fixture::model(),
        engine_fixture::checkpoint_fixture_options(),
    )
    .expect("open with fixture checkpoint key");
    store
        .conn
        .transaction(|| {
            let verified = store.verify_history(&std::cell::RefCell::default())?;
            store.finish_open_scrub(true, &verified)
        })
        .expect("record an authenticated checkpoint for the short fixture");
    drop(store);
    let image = engine_fixture::image_of(&cell);
    assert_eq!(
        image["tables"]["_norn_open_checkpoint"]["rows"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
    let text = serde_json::to_string_pretty(&image).expect("image") + "\n";
    std::fs::write(out, text).expect("write fixture");
}

#[test]
#[ignore = "#1533: records fixtures/engine-storage-v7.json (with marked + declassified snapshots); run on the last v7 engine (origin/main)"]
fn record_engine_storage_v7_fixture() -> Result<(), Box<dyn std::error::Error>> {
    let out = std::env::var_os("NORN_RECORD_ENGINE_FIXTURE")
        .ok_or_else(|| std::io::Error::other("NORN_RECORD_ENGINE_FIXTURE not set"))?;
    let dir = tempfile::tempdir()?;
    let cell = dir.path().join("cell.sqlite");
    engine_fixture::write_v7_marked_cell(&cell)?;
    let store = norn_cell::CellStore::open(
        &cell,
        engine_fixture::v7_marked_model()?,
        engine_fixture::checkpoint_fixture_options(),
    )?;
    store.conn.transaction(|| {
        let verified = store.verify_history(&std::cell::RefCell::default())?;
        store.finish_open_scrub(true, &verified)
    })?;
    drop(store);
    let image = engine_fixture::image_of(&cell);
    assert_eq!(
        image["tables"]["_norn_open_checkpoint"]["rows"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
    let text = format!("{}\n", serde_json::to_string_pretty(&image)?);
    std::fs::write(out, text)?;
    Ok(())
}
