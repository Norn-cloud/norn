//! B1 bounds on the adapter: a load past a cell or footer bound refuses and
//! leaves nothing behind; an engine holds a bounded number of landings and
//! evicts idle ones past their time to live, their directories with them.

use std::collections::BTreeMap;
use std::io::Write as _;

use super::super::{FileFormat, FileSource, LandRequest, LoadRequest, TableSchema, TenantId};
use super::{EXHAUSTED, Engine, Settings};

fn engine(memory: &str) -> (tempfile::TempDir, Engine) {
    let dir = tempfile::tempdir().expect("engine dir");
    let engine = Engine::new(Settings {
        root: dir.path().to_path_buf(),
        memory_limit: memory.to_owned(),
        threads: 1,
    })
    .expect("engine");
    (dir, engine)
}

fn tenant() -> TenantId {
    TenantId::of_scope("bounds")
}

fn table() -> TableSchema {
    serde_json::from_value(serde_json::json!({
        "name": "t.Wide",
        "columns": [{"id": "text", "kind": "text", "nullable": true}],
        "pin": "wide"
    }))
    .expect("table")
}

fn source(engine: &Engine, bytes: &[u8], format: FileFormat) -> (std::path::PathBuf, LoadRequest) {
    let path = engine
        .scratch(&tenant())
        .expect("scratch")
        .join(format!("source-{}", bytes.len()));
    std::fs::File::create(&path)
        .and_then(|mut file| file.write_all(bytes))
        .expect("source");
    let request = LoadRequest {
        tenant: tenant(),
        table: table(),
        load: "load-1".to_owned(),
        file: FileSource {
            url: String::new(),
            uri: "s3://synthetic/source".to_owned(),
            sha256: super::super::hex_digest(bytes),
            format,
        },
        headers: BTreeMap::new(),
    };
    (path, request)
}

fn staging_is_empty(dir: &std::path::Path) -> bool {
    let staging = dir.join("t").join(tenant().as_str()).join("staging");
    std::fs::read_dir(staging).map_or(true, |mut entries| entries.next().is_none())
}

#[test]
fn a_cell_past_its_bound_refuses_and_leaves_nothing_staged() {
    let (dir, engine) = engine("8MB");
    let mut csv = b"text\n".to_vec();
    csv.extend(std::iter::repeat_n(b'w', 3 << 20));
    csv.push(b'\n');
    let (path, request) = source(&engine, &csv, FileFormat::Csv);
    let refused = engine
        .load(&request, &path)
        .expect_err("a 3 MiB cell under 8MB");
    assert_eq!(refused.code, EXHAUSTED);
    assert!(staging_is_empty(dir.path()));
}

#[test]
fn a_footer_past_its_bound_refuses_before_it_is_parsed() {
    let (_dir, engine) = engine("8MB");
    let mut file = b"PAR1".to_vec();
    file.extend([0_u8; 64]);
    file.extend((64_u32 << 20).to_le_bytes());
    file.extend(b"PAR1");
    let (path, request) = source(&engine, &file, FileFormat::Parquet);
    let refused = engine.load(&request, &path).expect_err("a 64 MiB footer");
    assert_eq!(refused.code, EXHAUSTED);
}

/// A landing whose one row is left as residue, so the engine holds it.
fn held_landing(engine: &Engine, landing: usize) -> super::Outcome<super::super::LandResponse> {
    let csv = b"text\nann\n".to_vec();
    let (_path, load) = source(engine, &csv, FileFormat::Csv);
    let request: LandRequest = serde_json::from_value(serde_json::json!({
        "load": load,
        "pins": {"definitionsVersion": 1, "modelHash": "m", "binding": "b", "mapping": "p", "laws": "l"},
        "landing": format!("landing-{landing}"),
        "attempt": "",
        "checks": [{"rule": "r", "label": "l", "holds": true, "expect": true, "fuel": 1}],
        "residuePage": 10
    }))
    .map_err(|error| super::engine_error(error.to_string()))?;
    engine.land(&request, |mut file| {
        file.write_all(&csv)
            .map_err(|error| super::engine_error(error.to_string()))
    })
}

#[test]
fn landings_are_bounded_and_idle_ones_expire_with_their_files() {
    let (dir, mut engine) = engine("64MB");
    for landing in 0..16 {
        let held = held_landing(&engine, landing).expect("a landing left with residue");
        assert_eq!(held.residue_total, 1);
    }
    let refused = held_landing(&engine, 16).expect_err("a tenant's seventeenth landing");
    assert_eq!(refused.code, EXHAUSTED);
    let scratch = dir.path().join("t").join(tenant().as_str()).join("scratch");
    let count = || std::fs::read_dir(&scratch).map_or(0, Iterator::count);
    assert_eq!(count(), 16);
    engine.attempt_ttl = std::time::Duration::ZERO;
    held_landing(&engine, 17).expect("expired landings make room");
    assert_eq!(count(), 1, "the expired landings' directories are removed");
}

/// N6.5: an instant filter over a value without its ordering key is unknown,
/// so it holds neither negated nor not, as SQL's `NULL` does.
#[test]
fn a_keyless_instant_satisfies_no_filter_negated_or_not() {
    use super::query::{Typed, TypedFilter, filters_hold};
    use super::values::Cell;
    let keyless = [Cell::Text("2024-01-01".to_owned())];
    for negate in [false, true] {
        let filter = TypedFilter {
            index: 0,
            negate,
            test: Typed::Instant(0, 0),
        };
        assert_eq!(filters_hold(&[filter], &keyless), (false, false));
    }
}
