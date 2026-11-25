//! #1765: mirror compaction streams the table's history within a memory
//! bound, answering as resolving it all at once does; it refuses (leaving
//! the mirror exact) rather than pass its bound; and no object outlives its
//! records: a reset removes the old segments and anything a crash left, and
//! a refused sync installs nothing that stays.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::super::{FrontierRequest, MirrorRow, SyncRequest, TableSchema, TenantId, Value};
use super::compact::{COMPACT_AFTER, merge};
use super::mirror::{self, Need, SegmentRow, Version};
use super::values::Cell;
use super::{EXHAUSTED, Engine, Settings};

const WORLD: &str = "w";

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
    TenantId::of_scope("compaction")
}

fn table() -> TableSchema {
    serde_json::from_value(serde_json::json!({
        "name": "t.Mirror",
        "columns": [{"id": "note", "kind": "text", "nullable": true}],
        "pin": "p1"
    }))
    .expect("table")
}

/// A staged (or tombstone) row of identity `id` at version `v`, committed at `from`.
fn row(id: &str, v: u64, from: u64, ord: u64, note: Option<String>) -> SegmentRow {
    SegmentRow {
        ord,
        tomb: note.is_none(),
        resolved: false,
        version: Version {
            id: id.to_owned(),
            kind: "t.Mirror".to_owned(),
            v,
            from,
            to: None,
            scope: "s".to_owned(),
            bad: false,
            model: "m".to_owned(),
            cells: vec![note.map_or(Cell::Null, Cell::Text)],
        },
    }
}

/// Every row of the segment files at `paths`, with its segment number.
fn read_all(paths: &[PathBuf], table: &TableSchema) -> Vec<(u64, SegmentRow)> {
    let mut rows = Vec::new();
    for path in paths {
        for batch in super::merge::reader(path, 1 << 20).expect("reader") {
            rows.extend(mirror::read_rows(&batch.expect("batch"), table).expect("rows"));
        }
    }
    rows
}

/// Versions as comparable text, in identity order.
fn resolved_text(versions: &[Version]) -> Vec<String> {
    let mut text: Vec<String> = versions.iter().map(|v| format!("{v:?}")).collect();
    text.sort();
    text
}

/// `count` sync-like segments of a growing history: 120 identities, each
/// batch writing a new version of some of them (and deleting a few), with a
/// 1 KiB note per version.
fn history(engine: &Engine, count: u64) -> Vec<PathBuf> {
    let table = table();
    engine
        .with(&tenant(), |tenant| {
            let mut paths = Vec::new();
            for seq in 1..=count {
                let rows: Vec<SegmentRow> = (0..120_u64)
                    .filter(|id| id.wrapping_add(seq).rem_euclid(3) != 0)
                    .enumerate()
                    .map(|(ord, id)| {
                        let note = (id.wrapping_mul(seq).rem_euclid(17) != 0)
                            .then(|| format!("{seq}:{id}:{}", "n".repeat(1024)));
                        row(
                            &format!("id{id:03}"),
                            seq,
                            seq.wrapping_mul(10),
                            u64::try_from(ord).unwrap_or(0),
                            note,
                        )
                    })
                    .collect();
                let staged = mirror::write_segment(
                    tenant,
                    &table,
                    seq,
                    rows,
                    &format!("seg-{seq}.parquet"),
                )?;
                paths.push(staged);
            }
            Ok(paths)
        })
        .expect("segments")
}

/// The streaming merge resolves exactly what resolving every row at once
/// does, and holds a small, history-independent share of its memory bound.
#[test]
fn a_streaming_merge_resolves_as_the_whole_history_does_within_its_bound() {
    let (dir, engine) = engine("64MB");
    let table = table();
    let paths = history(&engine, 12);
    let everything = read_all(&paths, &table);
    let total = everything.len();
    let reference = resolved_text(&mirror::intervals(everything));
    let out = dir.path().join("compacted.parquet");
    let memory = 2 << 20;
    let merged = merge(memory, &table, 12, &paths, &out).expect("merge");
    let compacted = read_all(std::slice::from_ref(&out), &table);
    assert!(
        compacted
            .iter()
            .all(|(seg, row)| *seg == 12 && row.resolved)
    );
    assert_eq!(usize::try_from(merged.rows).expect("rows"), compacted.len());
    assert_eq!(resolved_text(&mirror::intervals(compacted)), reference);
    // The history is about 1 MiB of notes; the merge held far less, and
    // within its share of the bound, however long the history grows.
    assert!(total > 900, "{total}");
    assert!(merged.peak < memory.div_euclid(2), "{merged:?}");
    let longer = history(&engine, 36);
    let again = merge(
        memory,
        &table,
        36,
        &longer,
        &dir.path().join("longer.parquet"),
    )
    .expect("a longer history");
    assert!(again.peak < memory.div_euclid(2), "{again:?}");
}

/// An identity whose history alone passes the merge's bound refuses the
/// merge; a segment out of identity order refuses it too.
#[test]
fn a_merge_past_its_bound_or_out_of_order_refuses() {
    let (dir, engine) = engine("64MB");
    let table = table();
    let paths = engine
        .with(&tenant(), |tenant| {
            (1..=9_u64)
                .map(|seq| {
                    let rows = (0..8_u64)
                        .map(|at| {
                            let v = seq.wrapping_mul(8).wrapping_add(at);
                            row("one", v, v, at, Some("w".repeat(8 << 10)))
                        })
                        .collect();
                    mirror::write_segment(tenant, &table, seq, rows, &format!("big-{seq}.parquet"))
                })
                .collect::<super::Outcome<Vec<_>>>()
        })
        .expect("segments");
    let refused = merge(256 << 10, &table, 9, &paths, &dir.path().join("x.parquet"))
        .expect_err("one identity of 576 KiB under a 64 KiB share");
    assert_eq!(refused.code, EXHAUSTED);
    // A segment written out of identity order (as before #1765) refuses.
    let unordered = dir.path().join("unordered.parquet");
    let rows = [
        row("b", 1, 1, 0, Some("x".to_owned())),
        row("a", 1, 1, 1, Some("y".to_owned())),
    ];
    let schema = mirror::segment_schema(&table);
    let batch = mirror::segment_batch(&schema, &table, 1, &rows).expect("batch");
    super::columnar::write_parquet(&unordered, &schema, &[batch]).expect("write");
    let refused = merge(
        64 << 20,
        &table,
        1,
        &[unordered],
        &dir.path().join("y.parquet"),
    )
    .expect_err("out of identity order");
    assert!(
        refused.detail.contains("identity order"),
        "{}",
        refused.detail
    );
}

fn frontier(model: &str) -> FrontierRequest {
    FrontierRequest {
        tenant: tenant(),
        world: WORLD.to_owned(),
        tables: vec![table()],
        links: Vec::new(),
        model_pin: model.to_owned(),
    }
}

/// One sync batch `(after, through]` writing a version of `ids` identities.
fn batch(after: u64, through: u64, ids: u64, width: usize, model: &str) -> SyncRequest {
    let rows = (0..ids)
        .map(|id| MirrorRow {
            table: "t.Mirror".to_owned(),
            object_type: "t.Mirror".to_owned(),
            id: format!("id{id:03}"),
            version: through,
            commit_seq: through,
            scope: "s".to_owned(),
            values: Some(vec![Value::Text(format!(
                "{through}:{}",
                "v".repeat(width)
            ))]),
            model: model.to_owned(),
            bad: false,
        })
        .collect();
    SyncRequest {
        tenant: tenant(),
        world: WORLD.to_owned(),
        after,
        through,
        rows,
        model_pin: model.to_owned(),
        schema_pins: BTreeMap::from([("t.Mirror".to_owned(), "p1".to_owned())]),
        links: Vec::new(),
    }
}

fn objects(root: &Path) -> Vec<PathBuf> {
    let dir = root.join("t").join(tenant().as_str()).join("objects");
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_or_default(|entries| entries.map(|entry| entry.expect("entry").path()).collect());
    found.sort();
    found
}

/// Under a memory limit far below the table's history, every sync still
/// compacts: the table keeps at most `COMPACT_AFTER + 1` segments, each
/// named object is a live segment, and the mirror answers every identity.
#[test]
fn compaction_keeps_up_under_a_memory_limit_below_the_history() {
    let (dir, engine) = engine("4MB");
    engine.frontier(&frontier("m")).expect("frontier");
    for through in 1..=30_u64 {
        engine
            .sync(&batch(
                through.saturating_sub(1),
                through,
                100,
                2 << 10,
                "m",
            ))
            .expect("sync");
    }
    let (segments, current) = engine
        .with(&tenant(), |tenant| {
            let segments = tenant.catalog.segments("t.Mirror", WORLD)?;
            let current =
                mirror::versions(&engine, tenant, &table(), &segments, Need::Current(30))?;
            Ok((segments, current))
        })
        .expect("mirror");
    assert!(
        segments.len() <= COMPACT_AFTER.saturating_add(1),
        "{}",
        segments.len()
    );
    assert_eq!(current.len(), 100);
    assert!(current.iter().all(|version| version.v == 30));
    let mut named: Vec<PathBuf> = segments
        .iter()
        .map(|segment| segment.path.clone())
        .collect();
    named.sort();
    assert_eq!(objects(dir.path()), named, "replaced segments are removed");
}

/// A refused sync installs nothing that stays; a reset removes the old
/// segments and any object a crash left behind, after it is committed, and
/// keeps every object another record names; the mirror then syncs afresh.
#[test]
fn a_reset_sweeps_unnamed_objects_and_a_refused_sync_leaves_none() {
    let (dir, engine) = engine("64MB");
    engine.frontier(&frontier("m1")).expect("frontier");
    for through in 1..=3_u64 {
        engine
            .sync(&batch(through.saturating_sub(1), through, 5, 16, "m1"))
            .expect("sync");
    }
    let before = objects(dir.path());
    assert_eq!(before.len(), 3);
    // A batch that does not continue the frontier refuses and leaves nothing.
    assert!(engine.sync(&batch(7, 8, 5, 16, "m1")).is_err());
    assert_eq!(objects(dir.path()), before);
    // What a crash between install and publish would leave behind.
    let orphan = before
        .first()
        .and_then(|path| path.parent())
        .expect("objects dir")
        .join(format!("{}.parquet", "0".repeat(64)));
    std::fs::write(&orphan, b"PAR1").expect("orphan");
    // An object another record names (as a dataset's files are) is kept.
    let kept = orphan.with_file_name(format!("{}.parquet", "1".repeat(64)));
    std::fs::write(&kept, b"PAR1").expect("kept");
    engine
        .with(&tenant(), |tenant| {
            tenant.catalog.write(|tx| {
                super::catalog::set(tx, "files:t.Kept", &format!("{{\"data\":{kept:?}}}"))
            })
        })
        .expect("record");
    engine.frontier(&frontier("m2")).expect("a reset");
    assert_eq!(objects(dir.path()), vec![kept.clone()]);
    engine
        .sync(&batch(0, 1, 5, 16, "m2"))
        .expect("a fresh sync");
    assert_eq!(objects(dir.path()).len(), 2);
    assert!(kept.exists());
}
