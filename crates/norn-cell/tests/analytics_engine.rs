//! D102 / QUERY-ENGINE-1 / QUERY-THREE: the tenant analytics engine answers
//! exactly as the cell's exact engine at the same frontier, through the
//! mirror the cell syncs (ANALYTICS-FRONTIER-1); its large answers commit to
//! their members (MEMBERSHIP-DIGEST-1); and a loaded dataset is typed, its
//! refused rows quarantined with reasons (BULK-LOAD-1).
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "QUERY-ENGINE-1 engine correspondence"
)]
mod query_support;

use std::collections::BTreeMap;
use std::io::Write as _;

use proptest::strategy::Strategy as _;

use norn_cell::analytics::engine::{Engine, Settings};
use norn_cell::analytics::{
    EvaluateResponse, FileFormat, FileSource, GroupKey, LoadRequest, MembersRequest,
    MembershipDigest, Source, SyncRequest, TenantId, bucket_value, lower, table_schema,
};
use norn_cell::compute::{QueryContext, QueryResult};
use norn_cell::{CellError, CellStore, CommitBatch, FactWriteRow, Model};
use norn_checker::EffectiveCatalog;
use norn_checker::metric::compute::{QuestionPlan, admit};
use norn_ir::{
    ArtifactEnvelope, ArtifactKind, ArtifactPayload, Decimal, EntityIrVersion, EntityRootV2,
    RuntimeValue,
};
use query_support::{CLOCK, TYPE, WORLD, fixture, load, put};

fn engine() -> (tempfile::TempDir, Engine) {
    let dir = tempfile::tempdir().expect("engine dir");
    let engine = Engine::new(Settings {
        root: dir.path().to_path_buf(),
        memory_limit: "256MB".into(),
        threads: 1,
    })
    .expect("engine");
    (dir, engine)
}

fn tenant() -> TenantId {
    TenantId::of_scope("synthetic")
}

/// ANALYTICS-FRONTIER-1: bring the mirror to the cell's verified head, in small batches.
fn sync(store: &CellStore, engine: &Engine, world: &str) {
    let mut frontier = engine
        .frontier(&store.analytics_frontier(&tenant(), world).expect("tables"))
        .expect("frontier")
        .frontier;
    loop {
        let batch = store
            .analytics_delta(&tenant(), world, frontier, 3)
            .expect("delta");
        frontier = engine.sync(&batch.request).expect("sync").frontier;
        if batch.complete {
            break;
        }
    }
}

fn ask(
    store: &CellStore,
    engine: &Engine,
    plan: &QuestionPlan,
    context: QueryContext,
) -> Result<QueryResult, String> {
    sync(store, engine, &context.world);
    let prepared = store
        .analytics_prepare(plan, context, &tenant())
        .map_err(|e| code(&e))?;
    // An engine fault keeps its detail: it never equals a Core 03 refusal.
    let response = engine.evaluate(&prepared.request).map_err(|r| {
        if r.code == "analytics.engine" {
            format!("{}: {}", r.code, r.detail)
        } else {
            r.code
        }
    })?;
    store
        .analytics_answer(prepared, response)
        .map_err(|e| code(&e))
}

fn code(error: &CellError) -> String {
    if let CellError::ComputeRefused(code) = error {
        (*code).to_owned()
    } else {
        format!("{error:?}")
    }
}

/// QUERY-ENGINE-1: equal frontier, equal envelope (only the engine label differs),
/// or the same refusal.
fn compare(
    store: &CellStore,
    engine: &Engine,
    plan: &QuestionPlan,
    context: &QueryContext,
) -> Option<QueryResult> {
    let exact = store
        .evaluate_question(plan, context.clone(), |_| Ok(true))
        .map_err(|e| code(&e));
    let answered = ask(store, engine, plan, context.clone());
    match (exact, answered) {
        (Ok(exact), Ok(mut answered)) => {
            assert_eq!(answered.engine, norn_cell::compute::Engine::Duckdb);
            answered.engine = norn_cell::compute::Engine::Cell;
            assert_eq!(answered, exact);
            Some(exact)
        }
        (Err(exact), Err(answered)) => {
            assert_eq!(answered, exact);
            None
        }
        (exact, answered) => panic!("engines disagree: exact {exact:?}, engine {answered:?}"),
    }
}

fn commit(store: &mut CellStore, writes: Vec<FactWriteRow>) {
    store
        .commit(&CommitBatch {
            world: WORLD.into(),
            scope: "synthetic".into(),
            logical_time: CLOCK,
            writes,
            ..CommitBatch::default()
        })
        .expect("native changes");
}

/// The fixture's type with `recovered` nullable, asked as a sum of `recovered`
/// (or the yield ratio) grouped by `metal`.
fn by_metal(sum: bool, max_items: u32) -> (CellStore, QuestionPlan, QueryContext) {
    shaped(
        sum,
        max_items,
        &serde_json::json!([{"path": {"property": "metal"}}]),
    )
}

/// As [`by_metal`], grouped by `dimensions` (a `QuestionIR` dimension list).
fn shaped(
    sum: bool,
    max_items: u32,
    dimensions: &serde_json::Value,
) -> (CellStore, QuestionPlan, QueryContext) {
    declared(sum, max_items, dimensions, [3, 3])
}

/// As [`shaped`], with `recovered` declared at `scales[1]` decimal places and
/// the other decimals at `scales[0]`.
fn declared(
    sum: bool,
    max_items: u32,
    dimensions: &serde_json::Value,
    scales: [u64; 2],
) -> (CellStore, QuestionPlan, QueryContext) {
    let (store, plan, context) = fixture();
    let mut object = store.model().object_types().next().expect("type").clone();
    for property in &mut object.properties {
        let recovered = property.id.as_str() == "recovered";
        if recovered {
            property.constraints.nullable = true;
        }
        if let norn_ir::PropertyType::Decimal { scale, .. } = &mut property.property_type {
            *scale = norn_ir::WholeNumber(scales[usize::from(recovered)]);
        }
    }
    let mut artifacts: Vec<ArtifactEnvelope> =
        serde_json::from_str(include_str!("../../../fixtures/ir/metrics/ontology.json"))
            .expect("template");
    let artifact = &mut artifacts[0];
    artifact.artifact_id = "demo.Conversion.entity.v2".into();
    artifact.name = TYPE.into();
    artifact.namespace = "demo".into();
    artifact.payload = ArtifactPayload::from_kind_json(
        &ArtifactKind::Entity,
        norn_ir::json_tree_from_value(
            serde_json::to_value(EntityRootV2 {
                entity_version: EntityIrVersion::V2_0,
                object_type: object.clone(),
                mutation_authority: None,
                extensions: None,
            })
            .expect("typed root"),
        )
        .expect("tree"),
    )
    .expect("payload");
    let catalog = EffectiveCatalog::from_artifacts(&artifacts[..1]);
    let mut question = plan.question().clone();
    question.filters.clear();
    question.dimensions = serde_json::from_value(dimensions.clone()).expect("dimensions");
    if sum {
        question.metrics[0].metric = "demo.recovered".into();
    }
    let plan = admit(plan.definitions(), &question, &catalog, max_items).expect("admitted");
    let model = Model::from_definitions(&[object], &[]).expect("model");
    (
        CellStore::open_in_memory(model).expect("cell"),
        plan,
        context,
    )
}

fn row(id: &str, input: i64, recovered: Option<i64>, at: &str, metal: &str) -> FactWriteRow {
    let exact =
        |value: i64| RuntimeValue::from(Decimal::parse(&format!("{value}e-3")).expect("scale 3"));
    FactWriteRow::Put {
        object_type: TYPE.into(),
        key: id.into(),
        values: BTreeMap::from([
            ("input".into(), exact(input)),
            (
                "recovered".into(),
                recovered.map_or(RuntimeValue::Null, exact),
            ),
            ("loss".into(), exact(0)),
            ("occurred_at".into(), RuntimeValue::from(at)),
            ("metal".into(), RuntimeValue::from(metal)),
        ]),
    }
}

#[test]
fn query_three_scrap_yield_engine_matches_exact_after_correction_and_retraction() {
    let (_dir, engine) = engine();
    let (mut store, plan, context) = fixture();
    load(&mut store);
    compare(&store, &engine, &plan, &context).expect("answered");
    for write in [
        put(
            "SYN-MELT-15-1",
            75_000,
            72_000,
            "2026-10-23T12:00:00Z",
            "gold",
        ),
        FactWriteRow::Delete {
            object_type: TYPE.into(),
            key: "SYN-MELT-15-1".into(),
        },
    ] {
        commit(&mut store, vec![write]);
        compare(&store, &engine, &plan, &context).expect("answered");
    }
}

const TIMES: [&str; 5] = [
    "2026-10-23T12:00:00Z",
    "2026-10-19T00:00:00Z",
    "2026-10-18T23:59:59.999999999Z",
    "2026-10-20T01:00:00+02:00",
    "2026-11-02T00:00:00Z",
];
const METALS: [&str; 4] = ["gold", "silver", "e\u{301}", "\u{e9}"];

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(36))]
    /// QUERY-THREE: generated populations, then corrections and retractions,
    /// agree exactly after every commit: nulls, NFC buckets, window edges,
    /// filters, sums and ratios, by none, one or two dimensions and every
    /// calendar grain the shapes name.
    #[test]
    fn generated_histories_match_exact_envelopes(
        rows in proptest::collection::vec((0u8..12, -5000i64..5000, proptest::option::of(-5000i64..5000), 0usize..5, 0usize..4), 1..20),
        edits in proptest::collection::vec((0u8..12, proptest::option::of((-5000i64..5000, 0usize..4))), 0..8),
        shape in 0u8..6,
    ) {
        let (_dir, engine) = engine();
        let (mut store, plan, context) = match shape {
            0 => fixture(),
            1 => by_metal(true, 100),
            2 => by_metal(false, 100),
            3 => shaped(true, 100, &serde_json::json!([{"path": {"property": "occurred_at"}, "grain": "month"}, {"path": {"property": "metal"}}])),
            4 => shaped(false, 100, &serde_json::json!([{"path": {"property": "occurred_at"}, "grain": "day"}])),
            _ => shaped(true, 100, &serde_json::json!([])),
        };
        let nullable = shape != 0;
        let writes: BTreeMap<u8, FactWriteRow> = rows
            .iter()
            .map(|(id, input, recovered, at, metal)| {
                let recovered = if nullable { *recovered } else { Some(recovered.unwrap_or(0)) };
                (*id, row(&format!("G{id}"), *input, recovered, TIMES[*at], METALS[*metal]))
            })
            .collect();
        commit(&mut store, writes.into_values().collect());
        compare(&store, &engine, &plan, &context);
        for (id, edit) in edits {
            let write = match edit {
                Some((input, metal)) => row(&format!("G{id}"), input, Some(input / 2), TIMES[0], METALS[metal]),
                None => FactWriteRow::Delete { object_type: TYPE.into(), key: format!("G{id}") },
            };
            if store.commit(&CommitBatch {
                world: WORLD.into(),
                scope: "synthetic".into(),
                logical_time: CLOCK,
                writes: vec![write],
                ..CommitBatch::default()
            }).is_ok() {
                compare(&store, &engine, &plan, &context);
            }
        }
    }
}

/// ANALYTICS-FRONTIER-1: a mirror synced past a frontier still answers that
/// frontier exactly.
#[test]
fn an_answer_at_an_earlier_frontier_reads_only_versions_current_then() {
    let (_dir, engine) = engine();
    let (mut store, plan, context) = by_metal(true, 100);
    commit(
        &mut store,
        vec![row("A", 1000, Some(400), TIMES[0], "gold")],
    );
    let exact = store
        .evaluate_question(&plan, context.clone(), |_| Ok(true))
        .expect("exact");
    sync(&store, &engine, WORLD);
    let earlier = store
        .analytics_prepare(&plan, context, &tenant())
        .expect("prepared")
        .request;
    commit(
        &mut store,
        vec![
            row("A", 1000, Some(900), TIMES[0], "gold"),
            row("B", 1, Some(1), TIMES[0], "gold"),
        ],
    );
    sync(&store, &engine, WORLD);
    let response = engine.evaluate(&earlier).expect("earlier frontier");
    assert_eq!(totals(&response), exact_totals(&exact));
}

fn totals(response: &EvaluateResponse) -> BTreeMap<String, Vec<String>> {
    response
        .buckets
        .iter()
        .map(|b| {
            let key = String::from_utf8(
                bucket_value(&b.group)
                    .expect("bucket")
                    .canonical_json()
                    .expect("key"),
            )
            .expect("utf8");
            (key, b.totals.clone())
        })
        .collect()
}

fn exact_totals(result: &QueryResult) -> BTreeMap<String, Vec<String>> {
    use norn_checker::metric::compute::contribution::coefficient;
    result
        .rows
        .iter()
        .map(|(key, row)| {
            (
                key.clone(),
                row.totals
                    .iter()
                    .map(|t| coefficient(t).expect("bounded").to_string())
                    .collect(),
            )
        })
        .collect()
}

/// D102 ruling (b), MEMBERSHIP-DIGEST-1: past the plan's bound the exact
/// engine refuses, the engine answers with counts and digests, and paging
/// the members recomputes each digest.
#[test]
fn a_population_past_the_bound_answers_with_digests_that_paging_recomputes() {
    let (_dir, engine) = engine();
    let (mut store, plan, context) = by_metal(true, 3);
    let writes = (0..9)
        .map(|i| {
            row(
                &format!("M{i}"),
                100 * i,
                (i % 3 != 0).then_some(10 * i),
                TIMES[0],
                METALS[usize::try_from(i % 2).expect("index")],
            )
        })
        .collect();
    commit(&mut store, writes);
    assert!(matches!(
        store.evaluate_question(&plan, context.clone(), |_| Ok(true)),
        Err(CellError::ComputeRefused("core.compute.bound_exceeded"))
    ));
    let answered = ask(&store, &engine, &plan, context.clone()).expect("engine answers");
    let (_wide_store, wide_plan, _) = by_metal(true, 100);
    let exact = store
        .evaluate_question(&wide_plan, context.clone(), |_| Ok(true))
        .expect("wide exact");
    let prepared = store
        .analytics_prepare(&plan, context, &tenant())
        .expect("prepared");
    for (key, row) in &answered.rows {
        assert!(row.members.is_empty());
        let exact_row = &exact.rows[key];
        assert_eq!(row.totals, exact_row.totals);
        assert_eq!(row.null_counts, exact_row.null_counts);
        assert_eq!(
            row.member_count,
            Some(u64::try_from(exact_row.members.len()).expect("count"))
        );
        let RuntimeValue::String(text) = &row.bucket else {
            panic!("a text bucket: {:?}", row.bucket);
        };
        let group = GroupKey::Text(text.clone());
        let mut digest = MembershipDigest::default();
        let mut after = None;
        loop {
            let page = engine
                .members(&MembersRequest {
                    tenant: tenant(),
                    source: prepared.request.source.clone(),
                    query: prepared.request.query.clone(),
                    group: group.clone(),
                    after: after.clone(),
                    limit: 2,
                })
                .expect("page")
                .members;
            let Some(last) = page.last() else { break };
            after = Some(last.id.clone());
            for member in &page {
                digest.push(member);
            }
        }
        assert_eq!(Some(digest.finish()), row.membership_digest);
    }
}

/// The scope a row was written in must be the cell's.
#[test]
fn a_row_of_another_scope_refuses_as_stale() {
    let (_dir, engine) = engine();
    let (mut store, plan, context) = by_metal(true, 100);
    commit(
        &mut store,
        vec![row("A", 1000, Some(400), TIMES[0], "gold")],
    );
    sync(&store, &engine, WORLD);
    let mut request = store
        .analytics_prepare(&plan, context, &tenant())
        .expect("prepared")
        .request;
    if let Source::Mirror { scope, .. } = &mut request.source {
        *scope = "another".into();
    }
    assert_eq!(
        engine.evaluate(&request).expect_err("refused").code,
        "core.compute.stale_input"
    );
}

/// A batch that does not continue the mirror's frontier is refused whole.
#[test]
fn a_sync_that_skips_commits_is_refused() {
    let (_dir, engine) = engine();
    let (mut store, _plan, _context) = by_metal(true, 100);
    commit(
        &mut store,
        vec![row("A", 1000, Some(400), TIMES[0], "gold")],
    );
    engine
        .frontier(&store.analytics_frontier(&tenant(), WORLD).expect("tables"))
        .expect("frontier");
    let skipping = SyncRequest {
        tenant: tenant(),
        world: WORLD.into(),
        after: 7,
        through: 9,
        rows: Vec::new(),
        links: Vec::new(),
    };
    assert_eq!(
        engine.sync(&skipping).expect_err("refused").code,
        "analytics.frontier_moved"
    );
}

/// A CSV in the engine's scratch directory, the only place a load reads.
fn csv(engine: &Engine, lines: &[&str]) -> (tempfile::NamedTempFile, String) {
    let mut file = tempfile::NamedTempFile::new_in(engine.scratch()).expect("file");
    for line in lines {
        writeln!(file, "{line}").expect("write");
    }
    file.flush().expect("flush");
    let hex = norn_cell::analytics::hex_digest(&std::fs::read(file.path()).expect("read"));
    (file, hex)
}

fn load_request(plan: &QuestionPlan, load: &str, sha256: String) -> LoadRequest {
    LoadRequest {
        tenant: tenant(),
        table: table_schema(TYPE, plan.properties()).expect("schema"),
        load: load.into(),
        file: FileSource {
            url: "file:".into(),
            uri: "s3://synthetic/uploads/conversions.csv".into(),
            sha256,
            format: FileFormat::Csv,
        },
        headers: BTreeMap::new(),
    }
}

/// BULK-LOAD-1: rows are typed by their column; a row that is not, or that
/// misses a required value, is quarantined with its first reason; a blank
/// field is null; a changed file refuses by digest.
#[test]
fn a_load_types_rows_and_quarantines_refused_ones_with_reasons() {
    let (_dir, engine) = engine();
    let (_store, plan, _context) = by_metal(true, 100);
    let (file, sha256) = csv(
        &engine,
        &[
            "metal,occurred_at,input,recovered,loss",
            "gold,2026-10-23T12:00:00Z,1.5,1.25,0",
            "gold,2026-10-23T12:00:00Z,2,,0",
            "silver,2026-10-23T12:00:00+02:00,1.2345,1,0",
            "gold,not-a-time,1,1,0",
            "gold,2026-10-23T12:00:00Z,abc,1,0",
            "gold,2026-10-23T12:00:00Z,   ,1,0",
            "\"go,ld\",2026-02-30T00:00:00Z,1,1,0",
        ],
    );
    let response = engine
        .load(&load_request(&plan, "load-1", sha256.clone()), file.path())
        .expect("loaded");
    assert_eq!(response.rows, 2);
    assert_eq!(response.rejected, 5);
    assert_eq!(
        response.reasons,
        BTreeMap::from([
            ("bulk.not_decimal".into(), 1),
            ("bulk.not_timestamp".into(), 2),
            ("bulk.required".into(), 1),
            ("bulk.scale_exceeded".into(), 1),
        ])
    );
    // Declared pointwise laws quarantine the rows that break them.
    let mut lawful = load_request(&plan, "load-laws", sha256.clone());
    for column in &mut lawful.table.columns {
        if column.id == "metal" {
            column.laws = vec![norn_cell::analytics::PointLaw::OneOf(vec!["gold".into()])];
        } else if column.id == "input" {
            column.laws = vec![norn_cell::analytics::PointLaw::Min("1.6".into())];
        }
    }
    let laws = engine
        .load(&lawful, file.path())
        .expect("loaded under laws");
    // The first reason in column order: `input` is checked before `occurred_at`.
    assert_eq!(laws.rows, 1, "{laws:?}");
    assert_eq!(
        laws.reasons,
        BTreeMap::from([
            ("bulk.below_min".into(), 3),
            ("bulk.not_decimal".into(), 1),
            ("bulk.required".into(), 1),
            ("bulk.scale_exceeded".into(), 1),
        ])
    );
    let mut unchecked = load_request(&plan, "load-unchecked", sha256.clone());
    unchecked.table.columns[0].laws = vec![norn_cell::analytics::PointLaw::Unchecked];
    assert_eq!(
        engine
            .load(&unchecked, file.path())
            .expect_err("an unchecked law")
            .code,
        "bulk.unsupported_law"
    );
    let mut changed = load_request(&plan, "load-2", sha256);
    changed.file.sha256 = "0".repeat(64);
    assert_eq!(
        engine.load(&changed, file.path()).expect_err("digest").code,
        "bulk.digest_mismatch"
    );
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(12))]
    /// QUERY-ENGINE-1 over a loaded dataset: the same rows committed to a cell
    /// (keyed by their file row) give the same totals, nulls and members.
    #[test]
    fn a_loaded_dataset_answers_as_the_same_rows_in_a_cell(
        rows in proptest::collection::vec((-5000i64..5000, proptest::option::of(-5000i64..5000), 0usize..5, 0usize..2), 1..25),
    ) {
        let (_dir, engine) = engine();
        let (mut store, plan, context) = by_metal(true, 100);
        let mut lines = vec!["metal,occurred_at,input,recovered,loss".to_owned()];
        let mut writes = Vec::new();
        for (i, (input, recovered, at, metal)) in rows.iter().enumerate() {
            let text = |v: i64| Decimal::parse(&format!("{v}e-3")).expect("scale").canonical();
            lines.push(format!("{},{},{},{},0", METALS[*metal], TIMES[*at], text(*input), recovered.map_or(String::new(), text)));
            writes.push(row(&i.saturating_add(1).to_string(), *input, *recovered, TIMES[*at], METALS[*metal]));
        }
        let (file, sha256) = csv(&engine, &lines.iter().map(String::as_str).collect::<Vec<_>>());
        let loaded = engine.load(&load_request(&plan, "load-1", sha256), file.path()).expect("loaded");
        proptest::prop_assert_eq!(loaded.rows, u64::try_from(rows.len()).expect("rows"));
        commit(&mut store, writes);
        let exact = store.evaluate_question(&plan, context, |_| Ok(true)).expect("exact");
        let response = engine
            .evaluate(&norn_cell::analytics::EvaluateRequest {
                tenant: tenant(),
                source: Source::Dataset { table: TYPE.into(), load: "load-1".into() },
                query: lower(&plan).expect("lowered"),
                inline_members: 100,
            })
            .expect("dataset answer");
        proptest::prop_assert_eq!(totals(&response), exact_totals(&exact));
        for bucket in &response.buckets {
            let key = String::from_utf8(bucket_value(&bucket.group).expect("bucket").canonical_json().expect("key")).expect("utf8");
            let exact_row = &exact.rows[&key];
            let members = bucket.members.as_ref().expect("inline");
            proptest::prop_assert_eq!(members.len(), exact_row.members.len());
            for member in members {
                let exact_member = exact_row.members.get(&member.id).unwrap_or_else(|| panic!("member {:?} is not among {:?}", member.id, exact_row.members.keys()));
                let amounts: Vec<Option<Decimal>> = member.amounts.iter().map(|a| a.map(|v| Decimal::parse(&format!("{v}e-6")).expect("coefficient"))).collect();
                proptest::prop_assert_eq!(Some(amounts), exact_member.amounts.clone());
            }
        }
    }
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(48))]
    /// BULK-LOAD-1: a loaded timestamp is admitted exactly when the cell reads
    /// it as one, with the cell's own UTC ordering key; any other is
    /// quarantined as not a timestamp.
    #[test]
    fn loaded_timestamps_order_exactly_as_the_cell_reads_them(
        (year, month, day) in (1900u32..2100, 1u32..13, 1u32..32),
        (hour, minute, second) in (0u32..25, 0u32..60, 0u32..61),
        fraction in proptest::option::of("[0-9]{1,10}"),
        zone in proptest::prop_oneof![
            proptest::strategy::Just("Z".to_owned()),
            (0u32..25, 0u32..61, proptest::bool::ANY).prop_map(|(h, m, west)| format!("{}{h:02}:{m:02}", if west { '-' } else { '+' })),
        ],
    ) {
        let text = format!(
            "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}{}{zone}",
            fraction.map_or(String::new(), |f| format!(".{f}"))
        );
        let expected = norn_checker::metric::compute::contribution::instant_key(&text, false);
        let (_dir, engine) = engine();
        let (file, sha256) = csv(&engine, &["at", &text]);
        let mut request = load_request(&by_metal(true, 100).1, "t", sha256);
        request.table = norn_cell::analytics::TableSchema {
            name: "probe.Times".into(),
            columns: vec![norn_cell::analytics::Column {
                id: "at".into(),
                kind: norn_cell::analytics::ColumnKind::Timestamp,
                nullable: true,
                laws: Vec::new(),
            }],
            pin: "probe".into(),
        };
        let loaded = engine.load(&request, file.path()).expect("loaded");
        match expected {
            None => proptest::prop_assert_eq!(loaded.reasons, BTreeMap::from([("bulk.not_timestamp".to_owned(), 1)])),
            Some((secs, nanos)) => {
                proptest::prop_assert_eq!(loaded.rows, 1);
                let profile = engine
                    .profile(&norn_cell::analytics::ProfileRequest {
                        tenant: tenant(),
                        source: Source::Dataset { table: "probe.Times".into(), load: "t".into() },
                        columns: vec!["at".into()],
                    })
                    .expect("profile");
                proptest::prop_assert_eq!(
                    profile.columns[0].ranks.get(&1).cloned(),
                    Some(norn_cell::analytics::Value::Instant { text, secs, nanos })
                );
            }
        }
    }
}

/// The bulk path's numbers, on the machine that runs it: a 1M-row CSV load,
/// the same rows as Parquet, the weekly yield question over them, a profile,
/// and a mirror sync of 100k governed versions. Run on Boat in release:
/// `cargo test --release -p norn-cell --features duckdb --test analytics_engine -- --ignored --nocapture`.
#[test]
#[ignore = "a 1M-row measurement, run on Boat in release"]
#[allow(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    clippy::print_stdout,
    clippy::too_many_lines,
    reason = "a measurement prints rates of whole counts"
)]
fn bulk_path_measurements() {
    use std::time::Instant;
    let threads: u32 = std::env::var("NORN_BENCH_THREADS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let dir = tempfile::tempdir().expect("engine dir");
    let engine = Engine::new(Settings {
        root: dir.path().to_path_buf(),
        memory_limit: "512MB".into(),
        threads,
    })
    .expect("engine");
    println!("threads {threads}, memory_limit 512MB");
    let (_store, plan, _context) = by_metal(false, 100);
    let rows: usize = std::env::var("NORN_BENCH_ROWS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1_000_000);
    let mut lines = Vec::with_capacity(rows + 1);
    lines.push("metal,occurred_at,input,recovered,loss".to_owned());
    for i in 0..rows {
        let day = 1 + i % 28;
        let input = 1000 + (i * 7919) % 90_000;
        let recovered = (i % 10 != 0).then(|| input - (i * 31) % 900);
        lines.push(format!(
            "{},2026-{:02}-{day:02}T{:02}:00:00Z,{}.{:03},{},0",
            METALS[i % 3],
            7 + (i / 28) % 4,
            i % 24,
            input / 1000,
            input % 1000,
            recovered.map_or(String::new(), |r| format!("{}.{:03}", r / 1000, r % 1000)),
        ));
    }
    let (file, sha256) = csv(
        &engine,
        &lines.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    let bytes = std::fs::metadata(file.path()).expect("size").len();
    let started = Instant::now();
    let loaded = engine
        .load(&load_request(&plan, "csv", sha256), file.path())
        .expect("csv load");
    let csv_secs = started.elapsed().as_secs_f64();
    println!(
        "csv: {rows} rows, {bytes} bytes, {:.3}s, {:.0} rows/s, rejected {}",
        csv_secs,
        rows as f64 / csv_secs,
        loaded.rejected
    );
    let ask = |load: &str| norn_cell::analytics::EvaluateRequest {
        tenant: tenant(),
        source: Source::Dataset {
            table: TYPE.into(),
            load: load.into(),
        },
        query: lower(&plan).expect("lowered"),
        inline_members: 100,
    };
    let mut times = Vec::new();
    for _ in 0..11 {
        let started = Instant::now();
        let answer = engine.evaluate(&ask("csv")).expect("question");
        times.push(started.elapsed().as_secs_f64());
        assert!(!answer.buckets.is_empty());
    }
    times.sort_by(f64::total_cmp);
    println!(
        "question over {rows} rows (ratio by metal, digests): p50 {:.3}s, max {:.3}s",
        times[5], times[10]
    );
    let started = Instant::now();
    let profile = engine
        .profile(&norn_cell::analytics::ProfileRequest {
            tenant: tenant(),
            source: Source::Dataset {
                table: TYPE.into(),
                load: "csv".into(),
            },
            columns: vec![
                "input".into(),
                "recovered".into(),
                "occurred_at".into(),
                "metal".into(),
            ],
        })
        .expect("profile");
    println!(
        "profile of 4 columns over {} rows: {:.3}s",
        profile.rows,
        started.elapsed().as_secs_f64()
    );
    let started = Instant::now();
    let parquet = engine
        .export(&norn_cell::analytics::ExportRequest {
            tenant: tenant(),
            source: Source::Dataset {
                table: TYPE.into(),
                load: "csv".into(),
            },
            columns: vec![
                "metal".into(),
                "occurred_at".into(),
                "input".into(),
                "recovered".into(),
                "loss".into(),
            ],
            format: norn_cell::analytics::ExportFormat::Parquet,
        })
        .expect("export");
    let parquet_bytes = std::fs::metadata(&parquet).expect("size").len();
    println!(
        "parquet export: {parquet_bytes} bytes, {:.3}s",
        started.elapsed().as_secs_f64()
    );
    let mut request = load_request(
        &plan,
        "parquet",
        norn_cell::analytics::hex_digest(&std::fs::read(&parquet).expect("read")),
    );
    request.file.format = FileFormat::Parquet;
    let started = Instant::now();
    let loaded = engine.load(&request, &parquet).expect("parquet load");
    let parquet_secs = started.elapsed().as_secs_f64();
    println!(
        "parquet: {} rows, {:.3}s, {:.0} rows/s",
        loaded.rows,
        parquet_secs,
        loaded.rows as f64 / parquet_secs
    );
    let started = Instant::now();
    engine
        .evaluate(&ask("parquet"))
        .expect("question after parquet");
    println!(
        "question after parquet load: {:.3}s",
        started.elapsed().as_secs_f64()
    );
    // The governed mirror: 100k versions in batches of 10k.
    let (store, _plan, _context) = by_metal(true, 100);
    engine
        .frontier(&store.analytics_frontier(&tenant(), WORLD).expect("tables"))
        .expect("frontier");
    let table = table_schema(TYPE, plan.properties()).expect("schema");
    let started = Instant::now();
    let mut seq = 0u64;
    for batch in 0..10u64 {
        let rows = (0..10_000u64)
            .map(|i| {
                seq += 1;
                norn_cell::analytics::MirrorRow {
                    table: TYPE.into(),
                    object_type: TYPE.into(),
                    id: format!("M{}", batch * 10_000 + i),
                    version: 1,
                    commit_seq: batch + 1,
                    scope: "synthetic".into(),
                    values: Some(
                        table
                            .columns
                            .iter()
                            .map(|c| match c.id.as_str() {
                                "metal" => norn_cell::analytics::Value::Text("gold".into()),
                                "occurred_at" => norn_cell::analytics::Value::Instant {
                                    text: TIMES[0].into(),
                                    secs: 1_792_756_800,
                                    nanos: 0,
                                },
                                _ => norn_cell::analytics::Value::Number(format!("{}.5", i % 1000)),
                            })
                            .collect(),
                    ),
                    bad: false,
                }
            })
            .collect();
        engine
            .sync(&SyncRequest {
                tenant: tenant(),
                world: WORLD.into(),
                after: batch,
                through: batch + 1,
                rows,
                links: Vec::new(),
            })
            .expect("sync");
    }
    let sync_secs = started.elapsed().as_secs_f64();
    println!(
        "mirror sync: {seq} versions, {sync_secs:.3}s, {:.0} rows/s",
        seq as f64 / sync_secs
    );
}

/// METRIC-SNAPSHOT-ASOF-1 on the engine: a semi-additive stock read as of
/// each period's end equals the exact engine's snapshot answer, by week,
/// by week and location, by location alone (as of the window's end), and
/// ungrouped.
mod snapshot {
    use super::{Engine, compare, engine};
    use norn_cell::compute::QueryContext;
    use norn_cell::{CellStore, CommitBatch, FactWriteRow, Model};
    use norn_checker::metric::compute::QuestionPlan;
    use norn_ir::{
        ArtifactEnvelope, Decimal, LawRoot, ObjectTypeDef, QuestionIR, RuntimeValue as V,
    };
    use serde_json::json;
    use std::collections::{BTreeMap, BTreeSet};

    const TYPE: &str = "demo.Stock";
    const WORLD: &str = "synthetic-stock";
    const CLOCK: u64 = 1_792_972_800_000;
    const INSTANTS: [&str; 10] = [
        "2026-06-01T00:00:00Z",
        "2026-07-05T23:59:59Z",
        "2026-07-06T00:00:00Z",
        "2026-07-09T12:00:00Z",
        "2026-07-13T00:00:00Z",
        "2026-07-19T23:59:59Z",
        "2026-07-24T08:00:00Z",
        "2026-07-27T00:00:00Z",
        "2026-08-05T18:30:00Z",
        "2026-08-10T00:00:00Z",
    ];

    fn object_type() -> ObjectTypeDef {
        serde_json::from_value(json!({"name": TYPE, "properties": [
            {"id": "quantity", "type": {"kind": "decimal", "scale": 3, "additivity": {"kind": "semi_additive"}}, "constraints": {"nullable": true}},
            {"id": "observed_at", "type": {"kind": "timestamp", "timezone": "utc"}},
            {"id": "location", "type": {"kind": "text"}},
        ]}))
        .expect("typed model")
    }

    fn context() -> QueryContext {
        QueryContext {
            world: WORLD.into(),
            scope: "synthetic".into(),
            population_pin: "all-stock".into(),
            authorization_epoch: 1,
            mapping_pin: "fixture:stock".into(),
            now_ms: CLOCK + 1_000_000,
            source_cursors: BTreeMap::new(),
            watermarks: BTreeMap::new(),
        }
    }

    fn plan(north_only: bool, group_by: &serde_json::Value) -> (CellStore, QuestionPlan) {
        let artifact: ArtifactEnvelope = serde_json::from_value(json!({
            "irVersion": "0.1", "artifactId": "demo.Stock.entity", "kind": "entity", "namespace": "demo",
            "name": TYPE, "version": "0.1.0", "lifecycle": "active", "scope": {}, "dependencies": [],
            "provenance": {"source": "system"}, "payload": {"entityVersion": "2.0", "objectType": object_type()},
        }))
        .expect("artifact");
        let space = norn_checker::metric::derive::derive_metric_space(
            &[artifact],
            &BTreeSet::new(),
            &LawRoot::Entity(TYPE.to_owned()),
        )
        .expect("derived space");
        let metric = space
            .candidates()
            .iter()
            .find(|c| c.definition.name.ends_with(".quantity.as_of_observed_at"))
            .expect("the snapshot candidate");
        let filters = if north_only {
            json!([{"path": {"property": "location"}, "operator": "eq", "value": "north"}])
        } else {
            json!([])
        };
        let question: QuestionIR = serde_json::from_value(json!({
            "irVersion": "0.1", "name": "demo.stock_week", "objectSet": TYPE,
            "aggregate": [{"metric": metric.definition.name}],
            "groupBy": group_by, "filter": filters,
            "window": {"start": "2026-07-06T00:00:00Z", "end": "2026-08-10T00:00:00Z"},
            "freshness": {"kind": "live", "maxAgeMs": 5000},
        }))
        .expect("question");
        let plan = space
            .admit(&question, 100)
            .expect("admitted snapshot question");
        assert!(plan.as_of());
        let model = Model::from_definitions(&[object_type()], &[]).expect("model");
        (CellStore::open_in_memory(model).expect("cell"), plan)
    }

    fn put(
        store: &mut CellStore,
        tick: u64,
        key: &str,
        quantity: Option<i64>,
        at: &str,
        location: &str,
    ) {
        let quantity = quantity.map_or(V::Null, |q| {
            V::from(Decimal::parse(&q.to_string()).expect("exact"))
        });
        store
            .commit(&CommitBatch {
                world: WORLD.into(),
                scope: "synthetic".into(),
                logical_time: CLOCK.checked_add(tick).expect("clock"),
                writes: vec![FactWriteRow::Put {
                    object_type: TYPE.into(),
                    key: key.into(),
                    values: BTreeMap::from([
                        ("quantity".into(), quantity),
                        ("observed_at".into(), V::from(at)),
                        ("location".into(), V::from(location)),
                    ]),
                }],
                ..CommitBatch::default()
            })
            .expect("version commits");
    }

    fn shapes() -> [serde_json::Value; 4] {
        [
            json!([{"path": {"property": "observed_at"}, "grain": "week"}]),
            json!([{"path": {"property": "observed_at"}, "grain": "week"}, {"path": {"property": "location"}}]),
            json!([{"path": {"property": "location"}}]),
            json!([]),
        ]
    }

    fn check(
        engine: &Engine,
        history: &[(usize, Option<i64>, usize, bool)],
        north_only: bool,
        shape: &serde_json::Value,
    ) {
        let (mut store, plan) = plan(north_only, shape);
        for (tick, (subject, quantity, at, north)) in (1u64..).zip(history) {
            let location = if *north { "north" } else { "south" };
            put(
                &mut store,
                tick,
                &format!("s{subject}"),
                *quantity,
                INSTANTS[*at],
                location,
            );
        }
        compare(&store, engine, &plan, &context());
    }

    #[test]
    fn the_scenario_history_matches_the_exact_snapshot_in_every_shape() {
        let history = [
            (1, Some(10), 3, true),
            (1, Some(5), 4, true),
            (1, Some(7), 5, true),
            (2, Some(100), 6, true),
            (1, Some(3), 7, false),
            (2, Some(50), 5, true),
            (3, Some(1), 0, true),
            (3, Some(2), 0, true),
            (4, None, 3, true),
        ];
        for shape in shapes() {
            for north_only in [false, true] {
                let (_dir, engine) = engine();
                check(&engine, &history, north_only, &shape);
            }
        }
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(24))]
        /// Generated histories with valid-time ties, nulls, location flips and
        /// versions before the window and on period boundaries.
        #[test]
        fn generated_snapshot_histories_match_the_exact_engine(
            history in proptest::collection::vec((0usize..3, proptest::option::weighted(0.85, 0i64..1_000), 0usize..10, proptest::bool::ANY), 1..14),
            north_only in proptest::bool::ANY,
            shape in 0usize..4,
        ) {
            let (_dir, engine) = engine();
            check(&engine, &history, north_only, &shapes()[shape]);
        }
    }
}

/// The cases the retired in-process SQL path carried, held by the engine:
/// declared scales 0–7 per operand, signed midpoint rounding, a coefficient
/// near the bound, zero inputs, a hostile identity and a non-NFC bucket, each
/// through correction and retraction; and the calendar labels.
mod scales {
    use super::{
        CellStore, Decimal, FactWriteRow, QueryContext, QuestionPlan, RuntimeValue, TYPE, commit,
        compare, declared, engine,
    };
    use std::collections::BTreeMap;

    fn by_metal(scales: [u64; 2], sum: bool) -> (CellStore, QuestionPlan, QueryContext) {
        declared(
            sum,
            100,
            &serde_json::json!([{"path": {"property": "metal"}}]),
            scales,
        )
    }

    fn row(id: &str, input: i64, recovered: i64, scales: [u64; 2]) -> FactWriteRow {
        let exact = |value: i64, scale: u64| {
            RuntimeValue::from(Decimal::parse(&format!("{value}e-{scale}")).expect("declared"))
        };
        FactWriteRow::Put {
            object_type: TYPE.into(),
            key: id.into(),
            values: BTreeMap::from([
                ("input".into(), exact(input, scales[0])),
                ("recovered".into(), exact(recovered, scales[1])),
                ("loss".into(), exact(0, scales[0])),
                (
                    "occurred_at".into(),
                    RuntimeValue::from("2026-10-23T12:00:00Z"),
                ),
                ("metal".into(), RuntimeValue::from("e\u{301}")),
            ]),
        }
    }

    #[test]
    fn rounding_scales_and_hostile_ids_match_exact_after_correction_and_retraction() {
        let id = "quoted'); DROP TABLE input;--";
        for (scales, input, recovered) in [
            ([3, 3], 128, 1i64),
            ([3, 3], 128, -1),
            ([3, 3], -128, 1),
            ([3, 3], -128, -1),
            ([3, 3], 0, 0),
            ([3, 3], 1, 9_000_000_000_000_000),
            ([0, 3], 128, 1),
            ([6, 0], 3, 1),
            ([0, 6], 1, -1),
            ([6, 0], 0, 1),
        ] {
            let (_dir, engine) = engine();
            let (mut store, plan, context) = by_metal(scales, false);
            for recovered in [recovered, recovered.checked_neg().expect("bounded sign")] {
                commit(&mut store, vec![row(id, input, recovered, scales)]);
                let answer = compare(&store, &engine, &plan, &context).expect("answered");
                assert_eq!(answer.rows.len(), 1, "{scales:?} {input} {recovered}");
            }
            commit(
                &mut store,
                vec![FactWriteRow::Delete {
                    object_type: TYPE.into(),
                    key: id.into(),
                }],
            );
            let answer = compare(&store, &engine, &plan, &context).expect("answered");
            assert!(answer.rows.is_empty());
        }
    }

    /// A declared scale past six: the engine answers exactly as the cell
    /// does, or refuses as it does.
    #[test]
    fn a_scale_past_six_answers_as_the_cell_does() {
        for sum in [true, false] {
            let (_dir, engine) = engine();
            let (mut store, plan, context) = by_metal([7, 7], sum);
            compare(&store, &engine, &plan, &context);
            commit(
                &mut store,
                vec![
                    row("G1", 1_234_567, 7_654_321, [7, 7]),
                    row("G2", -5, 3, [7, 7]),
                ],
            );
            compare(&store, &engine, &plan, &context);
        }
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(16))]
        #[test]
        fn generated_declared_scales_match_exact_envelopes(
            scales in (0u64..7, 0u64..7),
            population in proptest::collection::vec((-1000i64..1000, -1000i64..1000), 1..8),
            sum in proptest::bool::ANY,
        ) {
            let (_dir, engine) = engine();
            let scales = [scales.0, scales.1];
            let (mut store, plan, context) = by_metal(scales, sum);
            let writes = population
                .iter()
                .enumerate()
                .map(|(i, (input, recovered))| row(&format!("G{i}"), *input, *recovered, scales))
                .collect();
            commit(&mut store, writes);
            compare(&store, &engine, &plan, &context);
        }
    }

    /// The grain labels the exact engine buckets by equal `DuckDB`'s
    /// `date_trunc` in UTC, cast to a date: at every month, quarter and year
    /// boundary of 2024 (one second either side), around the epoch, on a leap
    /// day, across an ISO week that spans a year end, and at instants swept
    /// through 2024.
    #[test]
    #[allow(clippy::arithmetic_side_effects, reason = "instant sweep fixture")]
    fn grain_labels_equal_duckdb_date_trunc() {
        use norn_checker::metric::compute::contribution::period_start;
        use norn_ir::metric::TimeGrain;
        let conn = duckdb::Connection::open_in_memory().expect("duckdb");
        let edges = [
            1_704_067_199,
            1_704_067_200,
            1_706_745_599,
            1_706_745_600,
            1_709_251_199,
            1_709_251_200,
            1_711_929_599,
            1_711_929_600,
            1_714_521_599,
            1_714_521_600,
            1_717_199_999,
            1_717_200_000,
            1_719_791_999,
            1_719_792_000,
            1_722_470_399,
            1_722_470_400,
            1_725_148_799,
            1_725_148_800,
            1_727_740_799,
            1_727_740_800,
            1_730_419_199,
            1_730_419_200,
            1_733_011_199,
            1_733_011_200,
            1_735_689_599,
            1_735_689_600,
            -86401,
            -1,
            0,
            951_782_400,
            951_868_799,
            4_102_444_799,
            1_735_516_800,
            1_735_516_799,
        ];
        let sweep = (0..1_500_i64).map(|step| 1_703_462_400 + step * 21_599);
        for (grain, name) in [
            (TimeGrain::Day, "day"),
            (TimeGrain::Week, "week"),
            (TimeGrain::Month, "month"),
            (TimeGrain::Quarter, "quarter"),
            (TimeGrain::Year, "year"),
        ] {
            let sql = format!(
                "SELECT CAST(CAST(date_trunc('{name}', make_timestamp(CAST(? AS BIGINT) * 1000000)) AS DATE) AS VARCHAR)"
            );
            for seconds in edges.into_iter().chain(sweep.clone()) {
                let expected: String = conn
                    .query_row(&sql, [seconds], |row| row.get(0))
                    .expect("date_trunc");
                assert_eq!(
                    period_start(seconds, grain).expect("label"),
                    expected,
                    "{name} at {seconds}"
                );
            }
        }
    }
}

/// METRIC-GRAIN-LINK-1 on the engine: an entry's time is its event's,
/// through the to-one link the mirror keeps; an entry without an event is
/// skipped and counted (`unlinked`); a corrected event time moves its
/// entries; an unlinked or retargeted entry moves too. Every answer equals
/// the exact engine's (`linked_time_question.rs` is the oracle scenario).
mod linked_time {
    use super::{compare, engine};
    use norn_cell::compute::QueryContext;
    use norn_cell::{CellStore, CommitBatch, FactWriteRow, LinkChange, Model};
    use norn_checker::metric::compute::QuestionPlan;
    use norn_checker::metric::derive::derive_metric_space;
    use norn_ir::{
        ArtifactEnvelope, Decimal, LawRoot, ObjectTypeDef, QuestionIR, RuntimeValue as V,
    };
    use serde_json::json;
    use std::collections::{BTreeMap, BTreeSet};

    const WORLD: &str = "synthetic-ledger";
    const CLOCK: u64 = 1_792_972_800_000;

    fn types() -> [ObjectTypeDef; 2] {
        [
            serde_json::from_value(json!({"name": "demo.Event", "properties": [
                {"id": "occurred_at", "type": {"kind": "timestamp", "timezone": "utc"}},
            ]}))
            .expect("event type"),
            serde_json::from_value(json!({"name": "demo.Entry", "properties": [
                {"id": "amount", "type": {"kind": "decimal", "scale": 2, "additivity": {"kind": "additive"}}},
                {"id": "side", "type": {"kind": "text"}},
            ], "links": [
                {"id": "event", "target": "demo.Event", "cardinality": {"to": "one", "from": "many"}},
            ]}))
            .expect("entry type"),
        ]
    }

    fn envelope(object: &ObjectTypeDef) -> ArtifactEnvelope {
        let name = object.name.as_str();
        serde_json::from_value(json!({
            "irVersion": "0.1", "artifactId": format!("{name}.entity"), "kind": "entity",
            "namespace": "demo", "name": name, "version": "0.1.0", "lifecycle": "active",
            "scope": {}, "dependencies": [], "provenance": {"source": "system"},
            "payload": {"entityVersion": "2.0", "objectType": object},
        }))
        .expect("artifact")
    }

    /// "amount by `event.occurred_at`", grouped as `group_by`, optionally
    /// only the `debit` side.
    fn fixture(group_by: &serde_json::Value, debit_only: bool) -> (CellStore, QuestionPlan) {
        let [event, entry] = types();
        let space = derive_metric_space(
            &[envelope(&event), envelope(&entry)],
            &BTreeSet::new(),
            &LawRoot::Entity("demo.Entry".to_owned()),
        )
        .expect("derived space");
        let metric = space
            .candidates()
            .iter()
            .find(|candidate| {
                candidate.definition.type_params.expr.as_deref() == Some("amount")
                    && candidate
                        .definition
                        .norn
                        .time_dimension
                        .as_ref()
                        .is_some_and(|time| time.links.len() == 1)
            })
            .expect("the entry's linked-time sum");
        let filter = if debit_only {
            json!([{"path": {"property": "side"}, "operator": "eq", "value": "debit"}])
        } else {
            json!([])
        };
        let question: QuestionIR = serde_json::from_value(json!({
            "irVersion": "0.1", "name": "demo.entries", "objectSet": "demo.Entry",
            "aggregate": [{"metric": metric.definition.name}],
            "groupBy": group_by,
            "filter": filter,
            "window": {"start": "2026-07-06T00:00:00Z", "end": "2026-08-10T00:00:00Z"},
            "freshness": {"kind": "live", "maxAgeMs": 5000},
        }))
        .expect("question");
        let plan = space
            .admit(&question, 100)
            .expect("admitted linked question");
        assert!(plan.time_link().is_some());
        let model = Model::from_definitions(&types(), &[]).expect("model");
        (CellStore::open_in_memory(model).expect("cell"), plan)
    }

    fn weekly() -> serde_json::Value {
        json!([{"path": {"links": ["event"], "property": "occurred_at"}, "grain": "week"}])
    }

    fn context() -> QueryContext {
        QueryContext {
            world: WORLD.into(),
            scope: "synthetic".into(),
            population_pin: "all-entries".into(),
            authorization_epoch: 1,
            mapping_pin: "fixture:ledger".into(),
            now_ms: CLOCK.checked_add(1_000_000).expect("clock"),
            source_cursors: BTreeMap::new(),
            watermarks: BTreeMap::new(),
        }
    }

    fn event(key: &str, at: &str) -> FactWriteRow {
        FactWriteRow::Put {
            object_type: "demo.Event".into(),
            key: key.into(),
            values: BTreeMap::from([("occurred_at".into(), V::from(at))]),
        }
    }

    fn entry(key: &str, amount: i64, side: &str) -> FactWriteRow {
        FactWriteRow::Put {
            object_type: "demo.Entry".into(),
            key: key.into(),
            values: BTreeMap::from([
                (
                    "amount".into(),
                    V::from(Decimal::parse(&amount.to_string()).expect("exact amount")),
                ),
                ("side".into(), V::from(side)),
            ]),
        }
    }

    fn link(entry: &str, event: &str, add: bool) -> LinkChange {
        if add {
            LinkChange::Add {
                object_type: "demo.Entry".into(),
                link: "event".into(),
                source: entry.into(),
                target: event.into(),
                values: BTreeMap::new(),
                target_type: None,
            }
        } else {
            LinkChange::Remove {
                object_type: "demo.Entry".into(),
                link: "event".into(),
                source: entry.into(),
                target: event.into(),
                target_type: None,
            }
        }
    }

    fn commit(
        store: &mut CellStore,
        tick: u64,
        writes: Vec<FactWriteRow>,
        links: Vec<LinkChange>,
    ) -> bool {
        store
            .commit(&CommitBatch {
                world: WORLD.into(),
                scope: "synthetic".into(),
                logical_time: CLOCK.checked_add(tick).expect("clock"),
                writes,
                links,
                ..CommitBatch::default()
            })
            .is_ok()
    }

    #[test]
    fn an_entry_takes_its_events_time_and_one_without_an_event_is_counted_never_bucketed() {
        for (group_by, debit_only) in [
            (weekly(), false),
            (weekly(), true),
            (json!([{"path": {"property": "side"}}]), false),
            (json!([]), false),
        ] {
            let (_dir, engine) = engine();
            let (mut store, plan) = fixture(&group_by, debit_only);
            assert!(commit(
                &mut store,
                1,
                vec![
                    event("e1", "2026-07-07T10:00:00Z"),
                    event("e2", "2026-07-15T09:00:00Z"),
                    event("e3", "2026-07-22T00:00:00Z"),
                    entry("x1", 10, "debit"),
                    entry("x2", 20, "credit"),
                    entry("x3", 5, "debit"),
                    entry("x4", 7, "debit"),
                    entry("x5", 3, "credit"),
                ],
                vec![
                    link("x1", "e1", true),
                    link("x2", "e1", true),
                    link("x3", "e2", true),
                    link("x5", "e3", true),
                ],
            ));
            let answered = compare(&store, &engine, &plan, &context()).expect("answered");
            assert_eq!(answered.unlinked, 1, "{group_by}");
            // A corrected event time moves its entries; an unlinked entry is
            // counted again; a linked one joins a bucket.
            for (tick, writes, links) in [
                (2, vec![event("e2", "2026-07-08T00:00:00Z")], Vec::new()),
                (
                    3,
                    Vec::new(),
                    vec![link("x3", "e2", false), link("x4", "e3", true)],
                ),
                (4, vec![event("e3", "2026-09-01T00:00:00Z")], Vec::new()),
            ] {
                assert!(commit(&mut store, tick, writes, links));
                compare(&store, &engine, &plan, &context());
            }
        }
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(24))]
        /// Generated ledgers: events and entries written, linked, unlinked
        /// and relinked over several commits; after every commit the engine
        /// answers exactly as the cell.
        #[test]
        fn generated_ledgers_match_exact_envelopes(
            steps in proptest::collection::vec(
                (0u8..4, 0u8..5, 0u8..3, 0usize..6, -500i64..500, proptest::bool::ANY),
                1..12,
            ),
            shape in 0u8..3,
        ) {
            const TIMES: [&str; 6] = [
                "2026-07-01T00:00:00Z",
                "2026-07-06T00:00:00Z",
                "2026-07-12T23:59:59.999Z",
                "2026-07-20T08:00:00+02:00",
                "2026-08-09T23:59:59Z",
                "2026-08-10T00:00:00Z",
            ];
            let (_dir, engine) = engine();
            let group_by = match shape {
                0 => weekly(),
                1 => json!([{"path": {"property": "side"}}, {"path": {"links": ["event"], "property": "occurred_at"}, "grain": "month"}]),
                _ => json!([]),
            };
            let (mut store, plan) = fixture(&group_by, shape == 2);
            for (tick, (kind, entry_key, event_key, at, amount, debit)) in (1u64..).zip(steps) {
                let (x, e) = (format!("x{entry_key}"), format!("e{event_key}"));
                let side = if debit { "debit" } else { "credit" };
                let (writes, links) = match kind {
                    0 => (vec![event(&e, TIMES[at])], Vec::new()),
                    1 => (vec![entry(&x, amount, side)], Vec::new()),
                    2 => (vec![entry(&x, amount, side), event(&e, TIMES[at])], vec![link(&x, &e, true)]),
                    _ => (Vec::new(), vec![link(&x, &e, false)]),
                };
                if commit(&mut store, tick, writes, links) {
                    compare(&store, &engine, &plan, &context());
                }
            }
        }
    }
}
