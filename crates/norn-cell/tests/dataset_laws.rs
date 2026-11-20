//! MANIFEST-LAWS-1 (#1381): a dataset's laws run set-based on the engine and
//! hold back exactly what the per-row path holds back. The same rows go
//! through both paths. Per row, they are mapped and committed one at a time
//! as observations, and each observation's findings and quarantine are read
//! back. As a set, they are landed on the engine with every law checked in
//! one pass and the residue settled by the per-row evaluator. The two paths
//! must agree on the held-back rows, on every (row, rule) finding and its
//! hold flag, and on the rows of both landed files (LAW-02, LAW-05).
#![cfg(all(feature = "duckdb", feature = "native"))]
#![allow(
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    missing_docs,
    reason = "set-based versus per-row correspondence"
)]

#[path = "support/dataset_cell.rs"]
mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;

use norn_cell::analytics::engine::{Engine, Settings};
use norn_cell::analytics::{
    FileFormat, FileSource, LandRequest, LandResponse, LoadRequest, ResidueRow, SettleRequest,
    StoreRequest, StoredObject, TenantId,
};
use norn_cell::dataset_laws::rule_key;
use norn_ir::RuntimeValue as Value;
use norn_kernel::{LogicalTime, ObservationId, Observed, Provenance};
use proptest::strategy::Strategy as _;
use serde_json::json;
use support::{Cell, Field};

const TYPE: &str = "bench.RetailLine";
const FIELDS: [&str; 8] = [
    "id",
    "invoice",
    "code",
    "quantity",
    "price",
    "customer",
    "country",
    "occurred_at",
];

/// Laws every lowered path covers: comparison, arithmetic, Boolean
/// connectives, text equality and a nullable read.
const LOWERED: [(&str, &str, &str); 4] = [
    ("price_nonnegative", "error", "self.price >= 0"),
    (
        "customer_named",
        "warning",
        "(self.customer ?? 'nobody') != 'void'",
    ),
    (
        "line_total_bounded",
        "error",
        "self.price * self.quantity <= 12 or self.invoice == 'C1'",
    ),
    ("country_known", "error", "not (self.country == 'XX')"),
];
/// A law outside the lowered subset (a function call): every row is residue.
const FUNCTION: (&str, &str, &str) = (
    "retail_sign",
    "warning",
    "self.quantity > 0 or startsWith(self.invoice, 'C')",
);

/// The retail line: its identity first, `customer` optional.
fn fields() -> Vec<Field> {
    let text = |id: &'static str, required| Field {
        id,
        kind: json!({"kind":"text"}),
        required,
    };
    let decimal = |id: &'static str, scale: u32, additivity: &str| Field {
        id,
        kind: json!({"kind":"decimal","scale":scale,"unit":{"system":"ucum","code":"1"},
            "additivity":{"kind":additivity}}),
        required: true,
    };
    vec![
        text("id", true),
        text("invoice", true),
        text("code", true),
        decimal("quantity", 0, "additive"),
        decimal("price", 3, "non_additive"),
        text("customer", false),
        text("country", true),
        Field {
            id: "occurred_at",
            kind: json!({"kind":"timestamp","timezone":"utc"}),
            required: true,
        },
    ]
}

fn cell(laws: &[(&str, &str, &str)]) -> (tempfile::TempDir, Cell) {
    support::cell(TYPE, &fields(), laws)
}

/// LAW-05: nested Boolean connectives, a `null` test, and byte-distinct
/// Unicode text (a composed `é` is not its decomposed spelling).
const WIDENED: [(&str, &str, &str); 3] = [
    (
        "nested_connectives",
        "error",
        "not (self.country == 'XX' and (self.invoice == 'C1' or not (self.quantity > 2)))",
    ),
    (
        "customer_null_or_named",
        "warning",
        "self.customer == null or self.customer != 'void'",
    ),
    (
        "customer_not_composed",
        "warning",
        "self.customer != '\u{e9}'",
    ),
];

#[derive(Debug, Clone)]
struct Line {
    invoice: &'static str,
    quantity: i64,
    /// The price's exact decimal text (scale 3 at most).
    price: String,
    customer: Option<&'static str>,
    country: &'static str,
    occurred_at: &'static str,
}

/// LAW-05: instants with a zone offset and with nanoseconds.
const INSTANTS: [&str; 4] = [
    "2026-10-01T09:30:00Z",
    "2026-10-01T11:30:00+02:00",
    "2026-10-01T09:30:00.123456789Z",
    "2026-10-01T04:30:00.000000001-05:00",
];

/// A finding as both paths name it: the row, the rule, and whether it
/// holds the row back.
type Finding = (u64, String, bool);

/// The per-row path: each line observed through the mapping and committed
/// on its own. Returns each line's findings, the held-back lines, and the
/// lines that landed and are not held (the trusted rows).
fn per_row(cell: &mut Cell, lines: &[Line]) -> (BTreeSet<Finding>, BTreeSet<u64>, BTreeSet<u64>) {
    let input = norn_cell::mapping_bundle::MappingInput {
        mapping: "bench.dataset.mapping".into(),
        dataset: "Rows".into(),
        connector_instance: "public".into(),
        partition: "sample".into(),
        recorded_at: norn_ir::provenance::UtcTimestamp("2026-11-04T00:00:00Z".into()),
    };
    let mut findings = BTreeSet::new();
    let mut held = BTreeSet::new();
    let mut trusted = BTreeSet::new();
    for (index, line) in lines.iter().enumerate() {
        let row = u64::try_from(index + 1).expect("row");
        let id = format!("line-{row}");
        let mut payload = json!({"id":id,"invoice":line.invoice,"code":"c","quantity":line.quantity,
            "price":"__PRICE__","country":line.country,"occurred_at":line.occurred_at});
        if let Some(customer) = line.customer {
            payload["customer"] = json!(customer);
        }
        // The price stays an exact number token, never a float.
        let text = payload.to_string().replace("\"__PRICE__\"", &line.price);
        let observed = Observed {
            id: ObservationId(format!("row-{row}")),
            provenance: Provenance {
                source: "Bench".into(),
                source_id: id.clone(),
                mapping_version: "pipeline/1".into(),
                observed_at: LogicalTime::from_millis(1_793_750_400_000),
            },
            input: norn_ir::runtime_object! {"contract"=>"norn.raw_observation/v1", "source"=>"Bench",
            "source_id"=>id.clone(), "event_id"=>format!("row-{row}"), "source_version"=>"2",
            "pipeline_version"=>"pipeline/1", "kind"=>"upsert", "extracted_at"=>1_793_750_400_000u64,
            "payload"=>Value::from_json_text(&text).expect("payload"),
            "payload_digest"=>norn_ir::canonical::HashProfile::Norn1.hash_json(&text).expect("digest")},
        };
        let (bundle, _) = cell
            .inject_mapping_bundle(&observed, &input)
            .expect("captured");
        // A row the mapping refuses stays in the inbox: it is held back,
        // as a row the dataset path cannot type is.
        let _processed = cell.process_mapping_bundle(&bundle);
        let model = cell.store().model().clone();
        let catalog = norn_kernel::TypeCatalog::new(model.object_types())
            .with_interfaces(model.interfaces())
            .with_property_types(model.property_types())
            .with_governing_laws(model.law_artifacts());
        let reader = cell.store().read();
        let Some(record) = reader.object("main", TYPE, &id).expect("read") else {
            held.insert(row);
            continue;
        };
        for finding in reader
            .observation_findings("main", TYPE, &id)
            .expect("findings")
        {
            let holds = norn_kernel::snapshot_observe::finding_quarantines(
                &catalog,
                &finding.rule_ref,
                finding.severity,
            )
            .expect("hold class");
            findings.insert((row, rule_key(&finding.rule_ref).expect("rule"), holds));
        }
        if reader
            .observation_is_quarantined("main", &record)
            .expect("quarantine")
        {
            held.insert(row);
        } else {
            trusted.insert(row);
        }
    }
    (findings, held, trusted)
}

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

/// The CSV of `lines`.
fn csv(lines: &[Line]) -> String {
    let mut out = format!("{}\n", FIELDS.join(","));
    for (index, line) in lines.iter().enumerate() {
        out.push_str(&format!(
            "line-{},{},c,{},{},{},{},{}\n",
            index + 1,
            line.invoice,
            line.quantity,
            line.price,
            line.customer.unwrap_or_default(),
            line.country,
            line.occurred_at,
        ));
    }
    out
}

/// A landing of `bytes` under `attempt`, checked by every rule of the cell.
fn begin(
    engine: &Engine,
    cell: &Cell,
    bytes: &[u8],
    format: FileFormat,
    attempt: &str,
) -> LandRequest {
    let (_path, mut file) = engine.begin_attempt(&tenant(), attempt).expect("begun");
    file.write_all(bytes).expect("source");
    drop(file);
    let store = cell.store();
    let table = store.analytics_table(TYPE).expect("table");
    let checks = store.dataset_checks(TYPE, &table).expect("checks");
    LandRequest {
        load: LoadRequest {
            tenant: tenant(),
            table,
            load: "land-1".into(),
            file: FileSource {
                url: String::new(),
                uri: "s3://synthetic/uploads/lines".into(),
                sha256: norn_cell::analytics::hex_digest(bytes),
                format,
            },
            headers: BTreeMap::new(),
        },
        pins: store
            .dataset_pins(TYPE, &BTreeMap::new(), "bench.dataset.mapping@test")
            .expect("pins"),
        landing: "landing-1".into(),
        attempt: attempt.into(),
        checks,
        residue_page: 7,
    }
}

/// Settle every residue page of a landing by the per-row evaluator.
fn settle_all(
    engine: &Engine,
    cell: &Cell,
    request: &LandRequest,
    mut landed: LandResponse,
) -> (LandResponse, u64, Vec<ResidueRow>) {
    let store = cell.store();
    let mut residue_rows = 0;
    let mut seen = Vec::new();
    while landed.residue_total > 0 {
        let page = landed.residue.clone();
        residue_rows += u64::try_from(page.len()).expect("page");
        let findings = store
            .residue_findings(TYPE, &request.load.table, &page)
            .expect("per-row residue");
        // LAW-04: the evaluator decides only the rules left to each row.
        for finding in &findings {
            let row = page
                .iter()
                .find(|r| r.row == finding.row)
                .expect("a residue row");
            assert!(row.rules.contains(&finding.rule), "{finding:?}");
        }
        let through = page.last().expect("a residue page").row;
        seen.extend(page);
        landed = engine
            .settle(&SettleRequest {
                land: request.clone(),
                through,
                findings,
            })
            .expect("settled");
    }
    (landed, residue_rows, seen)
}

/// The rows of a landed Parquet file: its `:row` offsets, and for the held
/// set each `(row, rule, holds)` and the rows held back.
fn parquet_rows(path: &std::path::Path, column: &str) -> Vec<(u64, Option<String>, bool)> {
    let conn = duckdb::Connection::open_in_memory().expect("reader");
    let mut statement = conn
        .prepare(&format!(
            "SELECT \":row\", {column} FROM read_parquet('{}') ORDER BY 1",
            path.display()
        ))
        .expect("rows");
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, u64>(0)?,
                row.get::<_, Option<String>>(1).unwrap_or(None),
                row.get::<_, Option<bool>>(2)
                    .unwrap_or(None)
                    .unwrap_or(false),
            ))
        })
        .expect("rows");
    rows.map(|row| row.expect("row")).collect()
}

/// What the set-based path decided.
struct SetBased {
    findings: BTreeSet<Finding>,
    held: BTreeSet<u64>,
    data: BTreeSet<u64>,
    residue: u64,
    rows: Vec<ResidueRow>,
}

/// The set-based path: the same lines landed on the engine, every law
/// checked in one pass, the residue settled by the per-row evaluator; read
/// back from the two Parquet files the landing wrote.
fn set_based(cell: &Cell, lines: &[Line]) -> SetBased {
    let (_dir, engine) = engine();
    let request = begin(&engine, cell, csv(lines).as_bytes(), FileFormat::Csv, "a1");
    let landed = engine.land(&request).expect("landed");
    let (landed, residue, rows) = settle_all(&engine, cell, &request, landed);
    let (data, held_path) = engine.landed_files(&tenant(), "a1").expect("files");
    let mut findings = BTreeSet::new();
    let mut held = BTreeSet::new();
    for (row, rule, holds) in parquet_rows(&held_path, "\":rule\", \":holds\"") {
        if let Some(rule) = rule {
            findings.insert((row, rule, holds));
        }
        if holds {
            held.insert(row);
        }
    }
    let data: BTreeSet<u64> = parquet_rows(&data, "NULL, NULL")
        .into_iter()
        .map(|(row, _, _)| row)
        .collect();
    // LAW-02: the data file is the typed rows no rule holds back, and the
    // counts say so.
    let held_by_rules = findings
        .iter()
        .filter(|(_, _, holds)| *holds)
        .map(|(row, _, _)| *row)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        landed.held_by_rules,
        u64::try_from(held_by_rules.len()).expect("count")
    );
    assert_eq!(
        landed.data.as_ref().expect("data").rows,
        u64::try_from(data.len()).expect("count")
    );
    assert_eq!(
        landed.load.rows,
        u64::try_from(data.len() + held_by_rules.len()).expect("count")
    );
    assert!(
        data.is_disjoint(&held),
        "a held row is not in the data file"
    );
    SetBased {
        findings,
        held,
        data,
        residue,
        rows,
    }
}

fn line(
    (invoice, quantity, price, customer, country, occurred_at): (
        &'static str,
        i64,
        String,
        Option<&'static str>,
        &'static str,
        &'static str,
    ),
) -> Line {
    Line {
        invoice,
        quantity,
        price,
        customer,
        country,
        occurred_at,
    }
}

fn lines() -> impl proptest::strategy::Strategy<Value = Vec<Line>> {
    let ordinary = (
        proptest::sample::select(vec!["A1", "A2", "C1", "C9"]),
        -3i64..6,
        (-2_000i64..5_000).prop_map(|milli| {
            norn_ir::Decimal::parse(&format!("{milli}e-3"))
                .expect("price")
                .canonical()
        }),
        proptest::option::of(proptest::sample::select(vec![
            "ann", "void", "bob", "\u{e9}", "e\u{301}",
        ])),
        proptest::sample::select(vec!["GB", "FR", "XX"]),
        proptest::sample::select(INSTANTS.to_vec()),
    )
        .prop_map(line);
    // LAW-05: prices at the edge of the exactly computed range and of the
    // column's scale, with quantities that keep a product in range.
    let boundary = (
        proptest::sample::select(vec!["A1", "C1"]),
        -1i64..2,
        proptest::sample::select(vec![
            "79228162514264337593543950.335".to_owned(),
            "-79228162514264337593543950.335".to_owned(),
            "0.001".to_owned(),
            "-0.001".to_owned(),
        ]),
        proptest::option::of(proptest::sample::select(vec!["ann", "\u{e9}"])),
        proptest::sample::select(vec!["GB", "XX"]),
        proptest::sample::select(INSTANTS.to_vec()),
    )
        .prop_map(line);
    proptest::collection::vec(proptest::prop_oneof![4 => ordinary, 1 => boundary], 1..24)
}

fn laws(
    extra: &[(&'static str, &'static str, &'static str)],
) -> Vec<(&'static str, &'static str, &'static str)> {
    LOWERED
        .iter()
        .chain(&WIDENED)
        .chain(extra)
        .copied()
        .collect()
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(16))]
    /// MANIFEST-LAWS-1 / LAW-05: lowered laws decide every row whose reads
    /// are present on the engine, and give each (row, rule) the per-row
    /// path's outcome and hold flag; both Parquet row sets are the per-row
    /// path's trusted and held rows (LAW-02).
    #[test]
    fn lowered_laws_hold_back_what_the_per_row_path_holds_back(lines in lines()) {
        let (_dir, mut cell) = cell(&laws(&[]));
        let set = set_based(&cell, &lines);
        let (row_findings, row_held, row_trusted) = per_row(&mut cell, &lines);
        proptest::prop_assert_eq!(&set.findings, &row_findings);
        proptest::prop_assert_eq!(&set.held, &row_held);
        proptest::prop_assert_eq!(&set.data, &row_trusted);
        // Only rows with an absent read are left to the per-row evaluator.
        let absent = u64::try_from(lines.iter().filter(|line| line.customer.is_none()).count()).expect("count");
        proptest::prop_assert_eq!(set.residue, absent);
    }

    /// MANIFEST-LAWS-1 / LAW-04: a law outside the lowered subset leaves
    /// every row to the per-row evaluator for that law alone; the engine's
    /// decisions of the row's other laws stand, with the same outcome.
    #[test]
    fn a_law_outside_the_subset_is_decided_per_row(lines in lines()) {
        let (_dir, mut cell) = cell(&laws(&[FUNCTION]));
        let set = set_based(&cell, &lines);
        let (row_findings, row_held, row_trusted) = per_row(&mut cell, &lines);
        proptest::prop_assert_eq!(&set.findings, &row_findings);
        proptest::prop_assert_eq!(&set.held, &row_held);
        proptest::prop_assert_eq!(&set.data, &row_trusted);
        proptest::prop_assert_eq!(set.residue, u64::try_from(lines.len()).expect("rows"));
        let function = cell
            .store()
            .dataset_checks(TYPE, &cell.store().analytics_table(TYPE).expect("table"))
            .expect("checks")
            .into_iter()
            .find(|check| check.label == "law:retail_sign")
            .expect("the function law")
            .rule;
        for row in &set.rows {
            let line = &lines[usize::try_from(row.row - 1).expect("row")];
            if line.customer.is_some() {
                proptest::prop_assert_eq!(&row.rules, &vec![function.clone()]);
            }
        }
    }
}

/// LAW-01: a landing is staged by its attempt and seen by no reader until
/// its files are stored and read at their cache identity. A reader of the
/// current dataset while another landing's residue is settled, and after a
/// settle that fails its check, reads the current dataset unchanged; a
/// retry is its own attempt and leaves the first one's findings alone.
#[test]
fn a_reader_sees_no_row_of_a_landing_until_it_is_stored() {
    let (_dir, cell) = cell(&laws(&[FUNCTION]));
    let (_engine_dir, engine) = engine();
    let first: Vec<Line> = (0..3)
        .map(|i| Line {
            invoice: "A1",
            quantity: 1,
            price: "1.5".into(),
            customer: Some("ann"),
            country: if i == 0 { "XX" } else { "GB" },
            occurred_at: INSTANTS[0],
        })
        .collect();
    let request = begin(
        &engine,
        &cell,
        csv(&first).as_bytes(),
        FileFormat::Csv,
        "a1",
    );
    let landed = engine.land(&request).expect("landed");
    let (landed, _, _) = settle_all(&engine, &cell, &request, landed);
    let (data, held) = (
        landed.data.clone().expect("data"),
        landed.held.clone().expect("held"),
    );
    let object = |sha: &str| StoredObject {
        uri: format!("s3://synthetic/{sha}.parquet"),
        url: String::new(),
    };
    let stored = engine
        .store(&StoreRequest {
            land: request.clone(),
            landed: landed.clone(),
            data: object(&data.sha256),
            held: object(&held.sha256),
        })
        .expect("stored")
        .stored
        .expect("a cache identity");
    let read = |load: &str| {
        engine.profile(&norn_cell::analytics::ProfileRequest {
            tenant: tenant(),
            source: norn_cell::analytics::Source::Dataset {
                table: TYPE.into(),
                load: load.into(),
            },
            columns: vec!["country".into()],
        })
    };
    let current = read(&stored).expect("the stored dataset");
    // A second landing with residue: nothing of it is read while it settles.
    let second: Vec<Line> = (0..9)
        .map(|_| Line {
            invoice: "C9",
            quantity: 2,
            price: "3".into(),
            customer: Some("bob"),
            country: "FR",
            occurred_at: INSTANTS[2],
        })
        .collect();
    let again = begin(
        &engine,
        &cell,
        csv(&second).as_bytes(),
        FileFormat::Csv,
        "a2",
    );
    let pending = engine.land(&again).expect("landed with residue");
    assert!(pending.residue_total > 0 && pending.data.is_none());
    assert_eq!(read(&stored).expect("unchanged"), current);
    // A retry is its own attempt; the first attempt's residue is untouched.
    let retry = begin(
        &engine,
        &cell,
        csv(&second).as_bytes(),
        FileFormat::Csv,
        "a3",
    );
    let retried = engine.land(&retry).expect("retried");
    assert_eq!(retried.residue_total, pending.residue_total);
    // A settle whose findings fail their check refuses and drops the attempt.
    let forged = SettleRequest {
        land: again.clone(),
        through: pending.residue.last().expect("residue").row,
        findings: vec![norn_cell::analytics::RowFinding {
            row: pending.residue[0].row,
            rule: "not a rule left to the row".into(),
            label: "law:forged".into(),
            holds: true,
        }],
    };
    assert_eq!(
        engine.settle(&forged).expect_err("a forged finding").code,
        "bulk.invalid_finding"
    );
    assert_eq!(
        engine
            .settle(&SettleRequest {
                findings: Vec::new(),
                ..forged
            })
            .expect_err("the attempt is gone")
            .code,
        "core.compute.stale_input"
    );
    assert_eq!(read(&stored).expect("unchanged"), current);
    // The retry settles on its own and lands what a clean run lands.
    let (retried, _, _) = settle_all(&engine, &cell, &retry, retried);
    let clean = begin(
        &engine,
        &cell,
        csv(&second).as_bytes(),
        FileFormat::Csv,
        "a4",
    );
    let clean_landed = engine.land(&clean).expect("landed");
    let (clean_landed, _, _) = settle_all(&engine, &cell, &clean, clean_landed);
    assert_eq!(
        (retried.data, retried.held, retried.findings),
        (clean_landed.data, clean_landed.held, clean_landed.findings)
    );
}

/// LAW-03: Parquet instants import exactly: a `TIMESTAMP_NS` keeps every
/// nanosecond (before the epoch too), a `TIMESTAMPTZ` its instant, so a rule
/// reads the value the file holds.
#[test]
fn parquet_instants_import_with_every_nanosecond_and_their_instant() {
    let (_dir, cell) = cell(&[]);
    let (_engine_dir, engine) = engine();
    let scratch = tempfile::tempdir().expect("scratch");
    let conn = duckdb::Connection::open_in_memory().expect("writer");
    let mut landed_instants = Vec::new();
    for (attempt, instants) in [
        (
            "a1",
            "TIMESTAMP_NS '2026-10-01 09:30:00.123456789', TIMESTAMP_NS '1969-12-31 23:59:59.999999999'",
        ),
        (
            "a2",
            "TIMESTAMPTZ '2026-10-01 11:30:00.5+02:00', TIMESTAMPTZ '2026-10-01 04:30:00-05:00'",
        ),
    ] {
        let path = scratch.path().join(format!("{attempt}.parquet"));
        conn.execute_batch(&format!(
            "COPY (SELECT 'line-' || CAST(i AS VARCHAR) AS id, 'A1' AS invoice, 'c' AS code, 1 AS quantity, \
             CAST('1.5' AS DECIMAL(18,3)) AS price, CAST(NULL AS VARCHAR) AS customer, 'GB' AS country, \
             [{instants}][i] AS occurred_at FROM range(1, 3) AS r(i)) TO '{}' (FORMAT parquet)",
            path.display()
        ))
        .expect("parquet written");
        let bytes = std::fs::read(&path).expect("parquet");
        let request = begin(&engine, &cell, &bytes, FileFormat::Parquet, attempt);
        let landed = engine.land(&request).expect("landed");
        assert_eq!(landed.residue_total, 0);
        let (data, _) = engine.landed_files(&tenant(), attempt).expect("files");
        landed_instants.extend(
            parquet_rows(&data, "occurred_at, NULL")
                .into_iter()
                .map(|(_, text, _)| text.expect("an instant")),
        );
    }
    assert_eq!(
        landed_instants,
        vec![
            "2026-10-01T09:30:00.123456789Z",
            "1969-12-31T23:59:59.999999999Z",
            "2026-10-01T09:30:00.500000Z",
            "2026-10-01T09:30:00.000000Z",
        ]
    );
}
