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
use std::fmt::Write as _;
use std::io::Write as _;

use super::backend::{Engine, Settings};
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
        "self.customer == null or (self.customer ?? '') != 'void'",
    ),
    (
        "customer_not_composed",
        "warning",
        "(self.customer ?? '') != '\u{e9}'",
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
        // S4: a number the per-row decoder cannot hold exactly is refused
        // before it is observed; the row is held back.
        let Ok(payload) = Value::from_json_text(&text) else {
            held.insert(row);
            continue;
        };
        let observed = Observed {
            id: ObservationId(format!("row-{row}")),
            provenance: Provenance {
                source: "Bench".into(),
                source_id: id.clone(),
                mapping_version: "pipeline/1".into(),
                observed_at: LogicalTime::from_millis(1_793_750_400_000),
            },
            input: norn_ir::runtime_object! {"contract"=>"norn.raw_observation/v1", "source"=>"Bench",
            "source_id"=>id, "event_id"=>format!("row-{row}"), "source_version"=>"2",
            "pipeline_version"=>"pipeline/1", "kind"=>"upsert", "extracted_at"=>1_793_750_400_000u64,
            "payload"=>payload,
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
    engine_with("256MB")
}

/// An engine under the `DuckDB` memory limit `memory`.
fn engine_with(memory: &str) -> (tempfile::TempDir, Engine) {
    let dir = tempfile::tempdir().expect("engine dir");
    let engine = Engine::new(Settings {
        root: dir.path().to_path_buf(),
        memory_limit: memory.into(),
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
        writeln!(
            out,
            "line-{},{},c,{},{},{},{},{}",
            index + 1,
            line.invoice,
            line.quantity,
            line.price,
            line.customer.unwrap_or_default(),
            line.country,
            line.occurred_at,
        )
        .expect("a String takes the line");
    }
    out
}

/// Land `bytes`, checked by every rule of the cell, in an attempt the
/// engine mints: the request (naming that attempt, for its settles and
/// store) and the landing's first answer.
fn land(
    engine: &Engine,
    cell: &Cell,
    bytes: &[u8],
    format: FileFormat,
) -> (LandRequest, LandResponse) {
    let mut request = land_request(cell, bytes, format);
    let landed = engine
        .land(&request, |mut file| {
            file.write_all(bytes).expect("source");
            Ok(())
        })
        .expect("landed");
    request.attempt.clone_from(&landed.attempt);
    (request, landed)
}

/// The files an attempt wrote under the engine directory `root`.
fn files(root: &std::path::Path, attempt: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = root
        .join("t")
        .join(tenant().as_str())
        .join("scratch")
        .join(attempt);
    (dir.join("data.parquet"), dir.join("held.parquet"))
}

/// A landing of `bytes`, checked by every rule of the cell. It names no
/// attempt: the engine mints one.
fn land_request(cell: &Cell, bytes: &[u8], format: FileFormat) -> LandRequest {
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
        attempt: String::new(),
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

/// The Parquet file of `lines`, every field text: an absent customer is a
/// null, a present one its text, empty or blank as it is.
fn parquet(lines: &[Line]) -> Vec<u8> {
    let quote = |text: &str| format!("'{}'", text.replace('\'', "''"));
    let rows: Vec<String> = lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            format!(
                "({}, {}, 'c', {}, {}, {}, {}, {})",
                quote(&format!("line-{}", index + 1)),
                quote(line.invoice),
                quote(&line.quantity.to_string()),
                quote(&line.price),
                line.customer
                    .map_or_else(|| "CAST(NULL AS VARCHAR)".to_owned(), &quote),
                quote(line.country),
                quote(line.occurred_at),
            )
        })
        .collect();
    let scratch = tempfile::tempdir().expect("scratch");
    let path = scratch.path().join("lines.parquet");
    duckdb::Connection::open_in_memory()
        .expect("writer")
        .execute_batch(&format!(
            "COPY (SELECT * FROM (VALUES {}) AS t({})) TO '{}' (FORMAT parquet)",
            rows.join(", "),
            FIELDS.join(", "),
            path.display()
        ))
        .expect("parquet written");
    std::fs::read(&path).expect("parquet")
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
    set_based_file(cell, csv(lines).as_bytes(), FileFormat::Csv, lines)
}

/// S3: every finding in the held-set file carries its row's values, as the
/// per-row path holds them (a number canonical).
fn assert_held_values(held: &std::path::Path, lines: &[Line]) {
    let conn = duckdb::Connection::open_in_memory().expect("reader");
    let mut statement = conn
        .prepare(&format!(
            "SELECT \":row\", {} FROM read_parquet('{}') WHERE \":rule\" IS NOT NULL ORDER BY 1",
            FIELDS.join(", "),
            held.display()
        ))
        .expect("held values");
    let rows = statement
        .query_map([], |row| {
            let mut values = Vec::new();
            for i in 1..=FIELDS.len() {
                values.push(row.get::<_, Option<String>>(i)?);
            }
            Ok((row.get::<_, u64>(0)?, values))
        })
        .expect("held values");
    for row in rows {
        let (row, values) = row.expect("held row");
        let line = &lines[usize::try_from(row - 1).expect("row")];
        let price = norn_ir::Decimal::parse(&line.price)
            .expect("a typed price")
            .canonical();
        let expected = vec![
            Some(format!("line-{row}")),
            Some(line.invoice.to_owned()),
            Some("c".to_owned()),
            Some(line.quantity.to_string()),
            Some(price),
            line.customer.map(str::to_owned),
            Some(line.country.to_owned()),
            Some(line.occurred_at.to_owned()),
        ];
        assert_eq!(values, expected, "the held values of row {row}");
    }
}

/// [`set_based`], of a file of `lines` in `format`.
fn set_based_file(cell: &Cell, bytes: &[u8], format: FileFormat, lines: &[Line]) -> SetBased {
    let (dir, engine) = engine();
    let (request, landed) = land(&engine, cell, bytes, format);
    let (landed, residue, rows) = settle_all(&engine, cell, &request, landed);
    let (data, held_path) = files(dir.path(), &landed.attempt);
    assert_held_values(&held_path, lines);
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
    let data: BTreeSet<u64> = parquet_rows(&data, "CAST(NULL AS VARCHAR), CAST(NULL AS BOOLEAN)")
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
            // S4: past the per-row decoder's exact range: never typed.
            "80000000000000000000000000000".to_owned(),
            "-79228162514264337593543950.336".to_owned(),
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

/// Whether the engine decides `line`'s laws itself: every read present and
/// every text in NFC (the decomposed `e\u{301}` is not).
fn decided(line: &Line) -> bool {
    line.customer.is_some_and(|customer| customer != "e\u{301}")
}

/// S4: whether `line` is typed at all: its price is in the per-row
/// decoder's exact range. An untyped row is held back, never residue.
fn typed(line: &Line) -> bool {
    norn_ir::Decimal::parse(&line.price).is_ok()
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
        // Only rows with an absent read, or text not in NFC, are left to the
        // per-row evaluator.
        let undecided = u64::try_from(lines.iter().filter(|line| typed(line) && !decided(line)).count()).expect("count");
        proptest::prop_assert_eq!(set.residue, undecided);
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
        proptest::prop_assert_eq!(set.residue, u64::try_from(lines.iter().filter(|line| typed(line)).count()).expect("rows"));
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
            if decided(line) {
                proptest::prop_assert_eq!(&row.rules, &vec![function.clone()]);
            }
        }
    }
}

/// LAW-02 / LAW-05: present Parquet text is the value the per-row path
/// reads: an empty or blank customer is that text, never a null, so a law
/// that defaults a null holds back exactly the rows the per-row path holds
/// back.
#[test]
fn present_parquet_text_is_decided_as_the_per_row_path_decides_it() {
    const BLANK: [(&str, &str, &str); 2] = [
        (
            "customer_not_empty",
            "error",
            "(self.customer ?? 'guest') != ''",
        ),
        (
            "customer_not_blank",
            "warning",
            "(self.customer ?? 'guest') != ' '",
        ),
    ];
    let (_dir, mut cell) = cell(&laws(&BLANK));
    let lines: Vec<Line> = [Some(""), Some(" "), Some("  "), None, Some("ann")]
        .into_iter()
        .map(|customer| Line {
            invoice: "A1",
            quantity: 1,
            price: "1.5".into(),
            customer,
            country: "GB",
            occurred_at: "2026-10-01T09:30:00Z",
        })
        .collect();
    let set = set_based_file(&cell, &parquet(&lines), FileFormat::Parquet, &lines);
    let (row_findings, row_held, row_trusted) = per_row(&mut cell, &lines);
    assert_eq!(set.findings, row_findings);
    assert_eq!(set.held, row_held);
    assert_eq!(set.data, row_trusted);
    assert!(set.held.contains(&1), "an empty customer is held back");
    assert!(
        set.findings.iter().any(|(row, _, _)| *row == 2),
        "a blank customer is found"
    );
    assert!(set.data.contains(&4), "an absent customer is a guest");
}

/// LAW-01: a landing is staged by its attempt and seen by no reader until
/// its files are stored and read at their cache identity. A reader of the
/// current dataset while another landing's residue is settled, and after a
/// settle that fails its check, reads the current dataset unchanged; a
/// retry is its own attempt and leaves the first one's findings alone.
#[test]
fn a_reader_sees_no_row_of_a_landing_until_it_is_stored() {
    let (_dir, cell) = cell(&laws(&[FUNCTION]));
    let (engine_dir, engine) = engine();
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
    let (request, landed) = land(&engine, &cell, csv(&first).as_bytes(), FileFormat::Csv);
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
        .store(
            &StoreRequest {
                land: request,
                landed,
                data: object(&data.sha256),
                held: object(&held.sha256),
            },
            |_data, _held| Ok(()),
        )
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
            model_pin: String::new(),
            cell_revision: String::new(),
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
    let (again, pending) = land(&engine, &cell, csv(&second).as_bytes(), FileFormat::Csv);
    assert!(pending.residue_total > 0 && pending.data.is_none());
    assert_eq!(read(&stored).expect("unchanged"), current);
    // A retry is its own attempt; the first attempt's residue is untouched.
    let (retry, retried) = land(&engine, &cell, csv(&second).as_bytes(), FileFormat::Csv);
    assert_eq!(retried.residue_total, pending.residue_total);
    // A settle whose findings fail their check refuses and drops the attempt.
    let forged = SettleRequest {
        land: again,
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
        super::backend::ATTEMPT_UNKNOWN
    );
    assert_eq!(read(&stored).expect("unchanged"), current);
    // The retry settles on its own and lands what a clean run lands.
    let (retried, _, _) = settle_all(&engine, &cell, &retry, retried);
    // OLAP-02: a settle replayed once the files are written refuses and
    // removes nothing; the attempt is still the store's to take.
    assert_eq!(
        engine
            .settle(&SettleRequest {
                land: retry,
                through: 0,
                findings: Vec::new(),
            })
            .expect_err("a replayed settle")
            .code,
        "core.compute.stale_input"
    );
    let (retried_data, retried_held) = files(engine_dir.path(), &retried.attempt);
    assert!(retried_data.exists() && retried_held.exists());
    let (clean, clean_landed) = land(&engine, &cell, csv(&second).as_bytes(), FileFormat::Csv);
    let (clean_landed, _, _) = settle_all(&engine, &cell, &clean, clean_landed);
    assert_eq!(
        (retried.data, retried.held, retried.findings),
        (clean_landed.data, clean_landed.held, clean_landed.findings)
    );
}

/// OLAP-02 (Sol r4 S2): a settle is bound to the landing contract the
/// engine recorded at land time. A settle that echoes the landing identity
/// but names another load (or other pins, or another file) is refused
/// before the attempt is taken: the attempt, its registry entry and its
/// staged residue stay intact, and the correct settle then lands exactly
/// what an undisturbed landing lands.
#[test]
fn a_settle_of_another_contract_is_refused_before_it_takes_the_attempt() {
    let (_dir, cell) = cell(&laws(&[FUNCTION]));
    let (engine_dir, engine) = engine();
    let lines: Vec<Line> = (0..9)
        .map(|_| Line {
            invoice: "C9",
            quantity: 2,
            price: "3".into(),
            customer: Some("bob"),
            country: "FR",
            occurred_at: INSTANTS[2],
        })
        .collect();
    let bytes = csv(&lines);
    let (request, pending) = land(&engine, &cell, bytes.as_bytes(), FileFormat::Csv);
    assert!(pending.residue_total > 0 && pending.data.is_none());
    let attempt_dir = engine_dir
        .path()
        .join("t")
        .join(tenant().as_str())
        .join("scratch")
        .join(&pending.attempt);
    let through = pending.residue.last().expect("residue").row;
    let mut other_load = request.clone();
    other_load.load.load = "land-2".into();
    let mut other_pins = request.clone();
    other_pins.pins.laws = "other-laws".into();
    let mut other_file = request.clone();
    other_file.load.file.uri = "s3://synthetic/uploads/other".into();
    for forged in [other_load, other_pins, other_file] {
        assert_eq!(
            engine
                .settle(&SettleRequest {
                    land: forged,
                    through,
                    findings: Vec::new(),
                })
                .expect_err("another landing contract")
                .code,
            "core.compute.stale_input"
        );
        assert!(attempt_dir.is_dir(), "the attempt's directory is intact");
    }
    // The registry entry and the staged residue survive: the correct
    // settle takes the attempt and lands what an undisturbed landing lands.
    let (settled, _, _) = settle_all(&engine, &cell, &request, pending);
    let (clean, clean_pending) = land(&engine, &cell, bytes.as_bytes(), FileFormat::Csv);
    let (clean_landed, _, _) = settle_all(&engine, &cell, &clean, clean_pending);
    assert_eq!(
        (settled.data, settled.held, settled.findings),
        (clean_landed.data, clean_landed.held, clean_landed.findings)
    );
}

/// LAW-03: Parquet instants import exactly: a `TIMESTAMP_NS` keeps every
/// nanosecond (before the epoch too), a `TIMESTAMPTZ` its instant, so a rule
/// reads the value the file holds.
#[test]
fn parquet_instants_import_with_every_nanosecond_and_their_instant() {
    let (_dir, cell) = cell(&[]);
    let (engine_dir, engine) = engine();
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
        // The earliest second TIMESTAMP_NS holds: its start is below BIGINT's
        // nanoseconds, and the instant is still imported exactly.
        (
            "a3",
            "TIMESTAMP_NS '1677-09-21 00:12:43.9', TIMESTAMP_NS '1677-09-21 00:12:43.5'",
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
        let (_request, landed) = land(&engine, &cell, &bytes, FileFormat::Parquet);
        assert_eq!(landed.residue_total, 0);
        let (data, _) = files(engine_dir.path(), &landed.attempt);
        landed_instants.extend(
            parquet_rows(&data, "occurred_at, CAST(NULL AS BOOLEAN)")
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
            "1677-09-21T00:12:43.900000000Z",
            "1677-09-21T00:12:43.500000000Z",
        ]
    );
}

/// A plain line, `customer` as given.
fn plain(customer: Option<&'static str>) -> Line {
    Line {
        invoice: "A1",
        quantity: 1,
        price: "1.5".into(),
        customer,
        country: "GB",
        occurred_at: INSTANTS[0],
    }
}

/// The attempt directories left under the engine directory `root`.
fn attempts_left(root: &std::path::Path) -> usize {
    let scratch = root.join("t").join(tenant().as_str()).join("scratch");
    std::fs::read_dir(scratch).map_or(0, Iterator::count)
}

/// MANIFEST-LAWS-1 (Sol S1): a landing whose residue takes more pages than
/// every host can settle within its engine steps is refused at land time
/// with a stable code, and its staging and attempt are dropped, so nothing
/// is left staged for a retry to strand. One page fewer lands.
#[test]
fn a_residue_past_the_page_bound_is_refused_and_leaves_nothing_staged() {
    use norn_cell::analytics::{MAX_ENGINE_STEPS, MAX_RESIDUE_PAGES, RESIDUE_EXCEEDED};
    // A landing is a land call, a settle per page and a store.
    assert_eq!(MAX_RESIDUE_PAGES + 2, MAX_ENGINE_STEPS);
    let (_dir, cell) = cell(&laws(&[FUNCTION]));
    let (engine_dir, engine) = engine();
    let pages = usize::try_from(MAX_RESIDUE_PAGES).expect("pages");
    let fits: Vec<Line> = (0..pages).map(|_| plain(Some("ann"))).collect();
    let over: Vec<Line> = (0..=pages).map(|_| plain(Some("ann"))).collect();
    let land_paged = |lines: &[Line]| {
        let bytes = csv(lines);
        let mut request = land_request(&cell, bytes.as_bytes(), FileFormat::Csv);
        request.residue_page = 1;
        let landed = engine.land(&request, |mut file| {
            file.write_all(bytes.as_bytes()).expect("source");
            Ok(())
        });
        (request, landed)
    };
    let (_, refused) = land_paged(&over);
    assert_eq!(refused.expect_err("too many pages").code, RESIDUE_EXCEEDED);
    assert_eq!(attempts_left(engine_dir.path()), 0, "nothing is staged");
    let (mut request, landed) = land_paged(&fits);
    let landed = landed.expect("the bound itself lands");
    request.attempt.clone_from(&landed.attempt);
    let (landed, residue, _) = settle_all(&engine, &cell, &request, landed);
    assert_eq!(residue, u64::try_from(pages).expect("rows"));
    assert!(landed.data.is_some());
}

/// MANIFEST-LAWS-1 (Sol S2): a row whose rules may cost the per-row
/// evaluator more fuel than it gives a row is never decided in SQL. Two
/// identical 600 KB NFC texts are equal in SQL, but ZEN charges an equality
/// its operands' bytes, so the per-row path refuses the row; the engine
/// leaves it to that path, which refuses the dataset as it would.
#[test]
fn a_row_past_the_fuel_budget_is_left_to_the_per_row_path() {
    let law = (
        "customer_is_invoice",
        "warning",
        "(self.customer ?? '') == self.invoice",
    );
    let (_dir, cell) = cell(&[law]);
    let (_engine_dir, engine) = engine();
    let wide: &'static str = Box::leak("a".repeat(600_000).into_boxed_str());
    let lines = vec![
        Line {
            invoice: wide,
            ..plain(Some(wide))
        },
        plain(Some("A1")),
    ];
    let (request, landed) = land(&engine, &cell, csv(&lines).as_bytes(), FileFormat::Csv);
    assert_eq!(landed.residue_total, 1, "only the wide row is residue");
    assert_eq!(landed.residue[0].row, 1);
    cell.store()
        .residue_findings(TYPE, &request.load.table, &landed.residue)
        .expect_err("the per-row path refuses the wide row");
}

/// MANIFEST-LAWS-1 (Sol S4): a number past the per-row decoder's exact
/// range is never typed, even where no law reads it: the per-row path
/// refuses it, so the dataset path holds it back too.
#[test]
fn a_number_past_the_exact_range_is_never_trusted() {
    let (_dir, mut cell) = cell(&[LOWERED[3]]);
    let mut lines = vec![plain(Some("ann")), plain(Some("ann")), plain(Some("ann"))];
    lines[0].price = "80000000000000000000000000000".into();
    lines[1].price = "-79228162514264337593543950.336".into();
    let (_engine_dir, engine) = engine();
    let (_, landed) = land(&engine, &cell, csv(&lines).as_bytes(), FileFormat::Csv);
    assert_eq!(landed.load.reasons.get("bulk.out_of_range"), Some(&2));
    let set = set_based(&cell, &lines);
    let (row_findings, row_held, row_trusted) = per_row(&mut cell, &lines);
    assert_eq!(set.held, BTreeSet::from([1, 2]));
    assert_eq!(set.data, BTreeSet::from([3]));
    assert_eq!(
        (set.findings, set.held, set.data),
        (row_findings, row_held, row_trusted)
    );
}

/// MANIFEST-LAWS-1 (Sol S5): a settle covers exactly the residue page the
/// engine answered. One whose `through` passes the page's last row would
/// drop rows no evaluator saw; it is refused before the attempt is taken,
/// and the attempt then settles as an undisturbed one does.
#[test]
fn a_settle_past_the_answered_page_is_refused() {
    let (_dir, cell) = cell(&laws(&[FUNCTION]));
    let (_engine_dir, engine) = engine();
    let lines: Vec<Line> = (0..9).map(|_| plain(Some("ann"))).collect();
    let bytes = csv(&lines);
    let (request, pending) = land(&engine, &cell, bytes.as_bytes(), FileFormat::Csv);
    assert_eq!(pending.residue.last().expect("a page").row, 7);
    assert_eq!(pending.residue_total, 9);
    let findings = cell
        .store()
        .residue_findings(TYPE, &request.load.table, &pending.residue)
        .expect("per-row residue");
    for through in [9, 6] {
        assert_eq!(
            engine
                .settle(&SettleRequest {
                    land: request.clone(),
                    through,
                    findings: findings.clone(),
                })
                .expect_err("not the answered page")
                .code,
            "core.compute.stale_input"
        );
    }
    let (settled, residue, _) = settle_all(&engine, &cell, &request, pending);
    assert_eq!(residue, 9, "every residue row was evaluated");
    let (clean, clean_pending) = land(&engine, &cell, bytes.as_bytes(), FileFormat::Csv);
    let (clean, _, _) = settle_all(&engine, &cell, &clean, clean_pending);
    assert_eq!(
        (settled.data, settled.held, settled.findings),
        (clean.data, clean.held, clean.findings)
    );
}

#[test]
fn a_settle_cannot_change_a_rules_label_or_hold() {
    let (_dir, cell) = cell(&laws(&[FUNCTION]));
    let (engine_dir, engine) = engine();
    let mut row = plain(Some("ann"));
    row.quantity = 0;
    let bytes = csv(&[row]);
    for forge_hold in [false, true] {
        let (request, pending) = land(&engine, &cell, bytes.as_bytes(), FileFormat::Csv);
        let mut findings = cell
            .store()
            .residue_findings(TYPE, &request.load.table, &pending.residue)
            .expect("per-row residue");
        assert_eq!(findings.len(), 1);
        assert!(findings[0].holds);
        if forge_hold {
            findings[0].holds = false;
        } else {
            findings[0].label = "another rule".into();
        }
        let through = pending.residue.last().expect("a residue row").row;
        assert_eq!(
            engine
                .settle(&SettleRequest {
                    land: request,
                    through,
                    findings,
                })
                .expect_err("forged finding metadata")
                .code,
            "bulk.invalid_finding"
        );
        assert_eq!(attempts_left(engine_dir.path()), 0);
    }
}

/// MANIFEST-LAWS-1 (Sol S6): a residue page is bounded in bytes as well as
/// rows: wide rows end a page early, and every row is still settled once.
#[test]
fn a_residue_page_of_wide_rows_is_bounded_in_bytes() {
    use norn_cell::analytics::RESIDUE_PAGE_BYTES;
    let (_dir, cell) = cell(&[FUNCTION]);
    let (_engine_dir, engine) = engine();
    let wide: &'static str = Box::leak("w".repeat(1 << 20).into_boxed_str());
    let lines: Vec<Line> = (0..7).map(|_| plain(Some(wide))).collect();
    let (request, mut landed) = land(&engine, &cell, csv(&lines).as_bytes(), FileFormat::Csv);
    let mut rows = Vec::new();
    let mut pages = 0;
    while landed.residue_total > 0 {
        let page = serde_json::to_string(&landed.residue).expect("page");
        assert!(
            page.len() <= RESIDUE_PAGE_BYTES,
            "a page of {} bytes",
            page.len()
        );
        pages += 1;
        let findings = cell
            .store()
            .residue_findings(TYPE, &request.load.table, &landed.residue)
            .expect("per-row residue");
        rows.extend(landed.residue.iter().map(|row| row.row));
        landed = engine
            .settle(&SettleRequest {
                land: request.clone(),
                through: *rows.last().expect("a row"),
                findings,
            })
            .expect("settled");
    }
    assert!(pages > 1, "wide rows end a page early");
    assert_eq!(rows, (1..=7).collect::<Vec<u64>>());
}

/// Typed properties of [`wide_typed_cell`]: Sol's S2 case.
const TYPED: usize = 120;

/// A type with two required texts `a` and `b`, [`TYPED`] decimals and the
/// invariant `self.a == self.b`: the kernel charges the typed properties to
/// a row's invariant budget.
fn wide_typed_cell() -> (tempfile::TempDir, Cell) {
    let text = |id: &'static str| Field {
        id,
        kind: json!({"kind":"text"}),
        required: true,
    };
    let mut fields = vec![text("id"), text("a"), text("b")];
    fields.extend((0..TYPED).map(|i| Field {
        id: Box::leak(format!("d{i}").into_boxed_str()),
        kind: json!({"kind":"decimal","scale":0,"unit":{"system":"ucum","code":"1"},
            "additivity":{"kind":"additive"}}),
        required: true,
    }));
    support::cell_with(TYPE, &fields, &[], &[("a_is_b", "self.a == self.b")])
}

/// The CSV of [`wide_typed_cell`] rows whose `a` and `b` are both `width`
/// bytes of `x`.
fn wide_typed_csv(widths: &[usize]) -> String {
    let decimals: Vec<String> = (0..TYPED).map(|i| format!("d{i}")).collect();
    let mut out = format!("id,a,b,{}\n", decimals.join(","));
    for (index, width) in widths.iter().enumerate() {
        let text = "x".repeat(*width);
        writeln!(
            out,
            "line-{},{text},{text},{}",
            index + 1,
            vec!["1"; TYPED].join(",")
        )
        .expect("a String takes the line");
    }
    out
}

/// MANIFEST-LAWS-1 (Sol r2 S2): the SQL fuel bound pays the kernel's
/// typed-property charge (one unit per typed property, once per row,
/// before any rule). With [`TYPED`] decimals, a text equality whose
/// operands are each one byte past the bound is left to the per-row path,
/// one at the bound is decided in SQL and the per-row path decides it too,
/// and Sol's 499,944-byte operands, which SQL decided before, are residue
/// that the per-row path refuses.
#[test]
fn the_fuel_bound_pays_the_typed_property_charge() {
    let (_dir, cell) = wide_typed_cell();
    let store = cell.store();
    let table = store.analytics_table(TYPE).expect("table");
    // Sol's row: SQL must leave it to the per-row path, which refuses it.
    let sol = 499_944;
    let (sol_dir, sol_engine) = engine_with("2GB");
    let bytes = wide_typed_csv(&[sol]);
    let (request, landed) = land(&sol_engine, &cell, bytes.as_bytes(), FileFormat::Csv);
    assert_eq!(
        landed.residue_total, 1,
        "Sol's row is left to the per-row path"
    );
    store
        .residue_findings(TYPE, &request.load.table, &landed.residue)
        .expect_err("the per-row path exhausts its fuel");
    // Sol r3 S8: the upload is refused before its store, so it left no
    // lineage record behind.
    drop(sol_engine);
    assert_eq!(lineage_records(sol_dir.path()), Vec::<String>::new());
    // The bound itself: `a == b` is charged both operands' bytes beside the
    // checks' static fuel, which carries the typed-property charge.
    let checks = store.dataset_checks(TYPE, &table).expect("checks");
    assert_eq!(checks.len(), 1);
    let budget = norn_cell::analytics::row_fuel();
    let at = usize::try_from((budget - checks[0].fuel) / 2).expect("width");
    assert!(sol > at + 1, "Sol's case is past the bound");
    let (_engine_dir, engine) = engine_with("2GB");
    let bytes = wide_typed_csv(&[at, at + 1]);
    let (request, landed) = land(&engine, &cell, bytes.as_bytes(), FileFormat::Csv);
    let residue: Vec<u64> = landed.residue.iter().map(|row| row.row).collect();
    assert_eq!(residue, vec![2], "only the row past the bound is residue");
    // The row at the bound fits the per-row budget, so SQL may decide it.
    let mut at_bound = landed.residue[0].clone();
    at_bound.row = 1;
    for column in ["a", "b"] {
        at_bound.values.insert(
            column.into(),
            norn_cell::analytics::Value::Text("x".repeat(at)),
        );
    }
    assert_eq!(
        store
            .residue_findings(TYPE, &request.load.table, &[at_bound])
            .expect("the per-row path decides the row at the bound"),
        Vec::new()
    );
}

/// MANIFEST-LAWS-1 (Sol r2 S4): an Integer column is typed in the signed
/// 64-bit range the per-row property codec holds, not the decimal
/// mantissa's. Both endpoints land; the values just past them are refused
/// as out of range, as the codec refuses them, even with no rule reading
/// the column.
#[test]
fn an_integer_is_typed_in_the_signed_64_bit_range() {
    let fields = vec![
        Field {
            id: "id",
            kind: json!({"kind":"text"}),
            required: true,
        },
        Field {
            id: "count",
            kind: json!({"kind":"integer","additivity":{"kind":"additive"}}),
            required: true,
        },
    ];
    let (_dir, cell) = support::cell(TYPE, &fields, &[]);
    let values = [
        i128::from(i64::MIN),
        i128::from(i64::MAX),
        i128::from(i64::MIN) - 1,
        i128::from(i64::MAX) + 1,
    ];
    let mut bytes = "id,count\n".to_owned();
    for (index, value) in values.iter().enumerate() {
        writeln!(bytes, "line-{},{value}", index + 1).expect("a String takes the line");
        // The per-row property codec is the reference.
        let number = Value::from_json_text(value.to_string()).expect("a JSON number");
        assert_eq!(
            norn_checker::property_codec::encode(norn_ir::PropertyKind::Integer, &number).is_ok(),
            index < 2,
            "{value}"
        );
    }
    let (engine_dir, engine) = engine();
    let (request, landed) = land(&engine, &cell, bytes.as_bytes(), FileFormat::Csv);
    assert_eq!(landed.load.reasons.get("bulk.out_of_range"), Some(&2));
    assert_eq!(landed.load.rejected, 2);
    let (landed, _, _) = settle_all(&engine, &cell, &request, landed);
    let (data, _) = files(engine_dir.path(), &landed.attempt);
    let rows: Vec<(u64, Option<String>, bool)> =
        parquet_rows(&data, "CAST(\"count\" AS VARCHAR), CAST(NULL AS BOOLEAN)");
    assert_eq!(
        rows,
        vec![
            (1, Some(i64::MIN.to_string()), false),
            (2, Some(i64::MAX.to_string()), false),
        ]
    );
}

/// The serialized size of the residue row a one-line landing of `plain`
/// with a customer of `width` bytes answers.
fn residue_row_bytes(cell: &Cell, width: usize) -> usize {
    let (_engine_dir, engine) = engine();
    let customer: &'static str = Box::leak("w".repeat(width).into_boxed_str());
    let (_, landed) = land(
        &engine,
        cell,
        &parquet(&[plain(Some(customer))]),
        FileFormat::Parquet,
    );
    serde_json::to_string(&landed.residue[0])
        .expect("row")
        .len()
}

/// MANIFEST-LAWS-1 (Sol r2 S7): a page's bound counts its brackets. A row
/// whose page (`[row]`) is exactly [`RESIDUE_PAGE_BYTES`] is answered, and
/// one a byte wider, which alone passes a page, is refused.
#[test]
fn a_residue_page_counts_its_brackets() {
    use norn_cell::analytics::{RESIDUE_EXCEEDED, RESIDUE_PAGE_BYTES};
    let (_dir, cell) = cell(&[FUNCTION]);
    // A customer's bytes are its row's bytes one for one.
    let base = 1 << 10;
    let overhead = residue_row_bytes(&cell, base) - base;
    let fits = RESIDUE_PAGE_BYTES - 2 - overhead;
    assert_eq!(residue_row_bytes(&cell, fits) + 2, RESIDUE_PAGE_BYTES);
    let (_engine_dir, engine) = engine();
    let at: &'static str = Box::leak("w".repeat(fits).into_boxed_str());
    // A row past CSV's 2 MB line comes as Parquet.
    let (_, landed) = land(
        &engine,
        &cell,
        &parquet(&[plain(Some(at))]),
        FileFormat::Parquet,
    );
    assert_eq!(
        serde_json::to_string(&landed.residue).expect("page").len(),
        RESIDUE_PAGE_BYTES
    );
    let over: &'static str = Box::leak("w".repeat(fits + 1).into_boxed_str());
    let bytes = parquet(&[plain(Some(over))]);
    let request = land_request(&cell, &bytes, FileFormat::Parquet);
    let refused = engine
        .land(&request, |mut file| {
            file.write_all(&bytes).expect("source");
            Ok(())
        })
        .expect_err("a page one byte past the bound");
    assert_eq!(refused.code, RESIDUE_EXCEEDED);
}

/// The `lineage:` records of the engine directory `root`, read once the
/// engine that wrote them is gone.
fn lineage_records(root: &std::path::Path) -> Vec<String> {
    super::backend::lineage_keys(root, &tenant())
}

/// MANIFEST-LAWS-1 (Sol r2 S8): a landing refused at its preflight (a
/// residue past the page bound, or a row wider than a page) leaves no
/// `lineage:<load>` record; one that is stored records its lineage (Sol r3
/// S8: in its store transaction).
#[test]
fn a_landing_refused_at_preflight_leaves_no_lineage() {
    use norn_cell::analytics::{MAX_RESIDUE_PAGES, RESIDUE_EXCEEDED, RESIDUE_PAGE_BYTES};
    let (_dir, cell) = cell(&laws(&[FUNCTION]));
    let pages = usize::try_from(MAX_RESIDUE_PAGES).expect("pages");
    let too_wide: &'static str = Box::leak("w".repeat(RESIDUE_PAGE_BYTES).into_boxed_str());
    let refusals = [
        (
            (0..=pages).map(|_| plain(Some("ann"))).collect::<Vec<_>>(),
            1,
        ),
        (vec![plain(Some(too_wide))], 7),
    ];
    for (lines, page) in refusals {
        let (engine_dir, engine) = engine();
        // Parquet carries a row past CSV's 2 MB line.
        let bytes = parquet(&lines);
        let mut request = land_request(&cell, &bytes, FileFormat::Parquet);
        request.residue_page = page;
        let refused = engine
            .land(&request, |mut file| {
                file.write_all(&bytes).expect("source");
                Ok(())
            })
            .expect_err("refused at preflight");
        assert_eq!(refused.code, RESIDUE_EXCEEDED);
        drop(engine);
        assert_eq!(lineage_records(engine_dir.path()), Vec::<String>::new());
    }
    let (engine_dir, engine) = engine();
    let (request, landed) = land(
        &engine,
        &cell,
        csv(&[plain(Some("ann"))]).as_bytes(),
        FileFormat::Csv,
    );
    let (landed, _, _) = settle_all(&engine, &cell, &request, landed);
    let (data, held) = (
        landed.data.clone().expect("data"),
        landed.held.clone().expect("held"),
    );
    let object = |sha: &str| StoredObject {
        uri: format!("s3://synthetic/{sha}.parquet"),
        url: String::new(),
    };
    engine
        .store(
            &StoreRequest {
                land: request,
                landed,
                data: object(&data.sha256),
                held: object(&held.sha256),
            },
            |_data, _held| Ok(()),
        )
        .expect("stored");
    drop(engine);
    assert_eq!(
        lineage_records(engine_dir.path()),
        vec!["lineage:land-1".to_owned()]
    );
}
