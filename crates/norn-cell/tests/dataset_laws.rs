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

use super::analytics_backend::{Engine, Settings};
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
        super::analytics_backend::ATTEMPT_UNKNOWN
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
    super::analytics_backend::lineage_keys(root, &tenant())
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

/// MANIFEST-LAWS-1: non-link predicate laws with `unless` exception trees
/// (both `always`/`error` nested overrides and `never`/`warning` exceptions)
/// run alongside a lowered law (`LAW-04` mixed settlement) and agree exactly
/// with the per-row observation path.
#[test]
fn excepted_predicate_laws_match_per_row_evaluation_across_nested_overrides() {
    let (mut artifacts, base) = support::artifacts(
        TYPE,
        &fields(),
        &[
            ("price_nonnegative", "error", "self.price >= 0"),
            ("price_capped", "error", "self.price <= 10"),
            ("no_blocked_invoice", "warning", "self.invoice == 'BLOCK'"),
        ],
    );
    let cap_law = artifacts
        .iter_mut()
        .find(|artifact| artifact.artifact_id.as_str() == "bench.price_capped.law")
        .expect("cap law");
    let mut cap_json = serde_json::to_value(&*cap_law).expect("cap json");
    cap_json["payload"]["condition"] = json!("self.country != 'XX'");
    cap_json["payload"]["exceptions"] = json!([
        {
            "id": "wholesale",
            "overrides": "price_capped",
            "condition": "self.quantity >= 10",
            "rationale": "Bulk orders are exempt from the retail cap.",
            "source": {"artifactId": "bench.price_capped.law", "pointer": "/payload/exceptions/0"}
        },
        {
            "id": "audit_block",
            "overrides": "wholesale",
            "condition": "self.invoice == 'AUDIT'",
            "rationale": "Audited bulk orders still enforce the retail cap.",
            "source": {"artifactId": "bench.price_capped.law", "pointer": "/payload/exceptions/1"}
        }
    ]);
    *cap_law = serde_json::from_value(cap_json).expect("updated cap law");

    let never_law = artifacts
        .iter_mut()
        .find(|artifact| artifact.artifact_id.as_str() == "bench.no_blocked_invoice.law")
        .expect("never law");
    let mut never_json = serde_json::to_value(&*never_law).expect("never json");
    never_json["payload"]["modality"] = json!({"kind": "never"});
    never_json["payload"]["exceptions"] = json!([
        {
            "id": "allow_uk",
            "overrides": "no_blocked_invoice",
            "condition": "self.country == 'UK'",
            "rationale": "UK blocked invoices are exempt from warning.",
            "source": {"artifactId": "bench.no_blocked_invoice.law", "pointer": "/payload/exceptions/0"}
        }
    ]);
    *never_law = serde_json::from_value(never_json).expect("updated never law");

    let (_dir, mut cell) = support::cell_from_artifacts(&artifacts, &base);

    let table = cell.store().analytics_table(TYPE).expect("table");
    let checks = cell
        .store()
        .dataset_checks(TYPE, &table)
        .expect("dataset checks");
    let lowered_check = checks
        .iter()
        .find(|check| check.label == "law:price_nonnegative")
        .expect("price_nonnegative check");
    assert!(lowered_check.predicate.is_some());
    let cap_check = checks
        .iter()
        .find(|check| check.label == "law:price_capped")
        .expect("price_capped check");
    assert!(
        cap_check.condition.is_some(),
        "row-tree unless exception tree folds into SQL condition"
    );
    assert!(cap_check.predicate.is_some());
    assert!(
        cap_check.fuel >= norn_cell::analytics::rule_fuel(0) + 2 * norn_cell::analytics::RULE_FUEL
    );
    let never_check = checks
        .iter()
        .find(|check| check.label == "law:no_blocked_invoice")
        .expect("no_blocked_invoice check");
    assert!(never_check.condition.is_some());
    assert!(never_check.predicate.is_some());

    let lines = vec![
        // 1: `when` false -> cap inactive (passes)
        Line {
            invoice: "IN1",
            quantity: 1,
            price: "50".into(),
            customer: Some("ann"),
            country: "XX",
            occurred_at: INSTANTS[0],
        },
        // 2: `when` true, base holds, `never` warning lifted by `allow_uk` -> passes
        Line {
            invoice: "BLOCK",
            quantity: 1,
            price: "5".into(),
            customer: Some("bob"),
            country: "UK",
            occurred_at: INSTANTS[0],
        },
        // 3: `when` true, base violated, no exception -> held
        Line {
            invoice: "IN3",
            quantity: 1,
            price: "50".into(),
            customer: Some("cat"),
            country: "FR",
            occurred_at: INSTANTS[0],
        },
        // 4: `when` true, base violated, `wholesale` lifts -> passes
        Line {
            invoice: "OK1",
            quantity: 12,
            price: "50".into(),
            customer: Some("dan"),
            country: "FR",
            occurred_at: INSTANTS[0],
        },
        // 5: `when` true, base violated, `audit_block` overrides `wholesale` -> held
        Line {
            invoice: "AUDIT",
            quantity: 12,
            price: "50".into(),
            customer: Some("eve"),
            country: "FR",
            occurred_at: INSTANTS[0],
        },
        // 6: `never` warning fires (`BLOCK` in `FR`) without lifting -> finding
        Line {
            invoice: "BLOCK",
            quantity: 1,
            price: "5".into(),
            customer: Some("fay"),
            country: "FR",
            occurred_at: INSTANTS[0],
        },
        // 7: co-located lowered law (`price_nonnegative`) fails -> held by SQL engine
        Line {
            invoice: "NEG1",
            quantity: 1,
            price: "-1".into(),
            customer: Some("gus"),
            country: "FR",
            occurred_at: INSTANTS[0],
        },
    ];

    let (row_findings, row_held, row_trusted) = per_row(&mut cell, &lines);
    let bulk = set_based(&cell, &lines);
    assert_eq!(
        bulk.residue, 0,
        "all row-tree excepted laws decide in SQL with zero residue"
    );
    assert_eq!(bulk.findings, row_findings);
    assert_eq!(bulk.held, row_held);
    assert_eq!(bulk.data, row_trusted);
    // Governing laws classify as Integrity regardless of display severity, so
    // row 6 (unexcepted `never`/`warning`) is held while row 2 (`allow_uk`) passes.
    assert_eq!(bulk.held, BTreeSet::from([3, 5, 6, 7]));
    assert_eq!(bulk.data, BTreeSet::from([1, 2, 4]));
}

/// An `unless` exception outside the row-tree subset (a function call) falls
/// back cleanly to per-row residue while matching per-row evaluation exactly.
#[test]
fn non_row_tree_exception_falls_back_to_per_row_residue() {
    let (mut artifacts, base) = support::artifacts(
        TYPE,
        &fields(),
        &[("price_capped", "error", "self.price <= 10")],
    );
    let cap_law = artifacts
        .iter_mut()
        .find(|artifact| artifact.artifact_id.as_str() == "bench.price_capped.law")
        .expect("cap law");
    let mut cap_json = serde_json::to_value(&*cap_law).expect("cap json");
    cap_json["payload"]["exceptions"] = json!([
        {
            "id": "vip_prefix",
            "overrides": "price_capped",
            "condition": "startsWith(self.invoice, 'VIP')",
            "rationale": "VIP invoices are exempt.",
            "source": {"artifactId": "bench.price_capped.law", "pointer": "/payload/exceptions/0"}
        }
    ]);
    *cap_law = serde_json::from_value(cap_json).expect("updated cap law");

    let (_dir, mut cell) = support::cell_from_artifacts(&artifacts, &base);
    let table = cell.store().analytics_table(TYPE).expect("table");
    let checks = cell
        .store()
        .dataset_checks(TYPE, &table)
        .expect("dataset checks");
    let cap_check = checks
        .iter()
        .find(|check| check.label == "law:price_capped")
        .expect("price_capped check");
    assert!(cap_check.condition.is_none());
    assert!(cap_check.predicate.is_none());

    let lines = vec![
        Line {
            invoice: "VIP-1",
            quantity: 1,
            price: "50".into(),
            customer: Some("ann"),
            country: "FR",
            occurred_at: INSTANTS[0],
        },
        Line {
            invoice: "STD-2",
            quantity: 1,
            price: "50".into(),
            customer: Some("bob"),
            country: "FR",
            occurred_at: INSTANTS[0],
        },
        Line {
            invoice: "STD-3",
            quantity: 1,
            price: "5".into(),
            customer: Some("cat"),
            country: "FR",
            occurred_at: INSTANTS[0],
        },
    ];
    let (row_findings, row_held, row_trusted) = per_row(&mut cell, &lines);
    let bulk = set_based(&cell, &lines);
    assert_eq!(bulk.residue, 3);
    assert_eq!(bulk.findings, row_findings);
    assert_eq!(bulk.held, row_held);
    assert_eq!(bulk.data, row_trusted);
    assert_eq!(bulk.held, BTreeSet::from([2]));
    assert_eq!(bulk.data, BTreeSet::from([1, 3]));
}

/// Lane A5.1: folding row-tree `unless` exception trees into SQL eliminates
/// the 124K residue cap (`MAX_RESIDUE_PAGES * RESIDUE_PAGE_ROWS = 124,000`).
#[test]
fn excepted_row_tree_law_handles_over_124k_rows_with_zero_residue() {
    let (mut artifacts, base) = support::artifacts(
        TYPE,
        &fields(),
        &[("price_capped", "error", "self.price <= 10")],
    );
    let cap_law = artifacts
        .iter_mut()
        .find(|artifact| artifact.artifact_id.as_str() == "bench.price_capped.law")
        .expect("cap law");
    let mut cap_json = serde_json::to_value(&*cap_law).expect("cap json");
    cap_json["payload"]["condition"] = json!("self.country != 'XX'");
    cap_json["payload"]["exceptions"] = json!([
        {
            "id": "wholesale",
            "overrides": "price_capped",
            "condition": "self.quantity >= 10",
            "rationale": "Bulk orders are exempt from the retail cap.",
            "source": {"artifactId": "bench.price_capped.law", "pointer": "/payload/exceptions/0"}
        },
        {
            "id": "audit_block",
            "overrides": "wholesale",
            "condition": "self.invoice == 'AUDIT'",
            "rationale": "Audited bulk orders still enforce the retail cap.",
            "source": {"artifactId": "bench.price_capped.law", "pointer": "/payload/exceptions/1"}
        }
    ]);
    *cap_law = serde_json::from_value(cap_json).expect("updated cap law");

    let (_dir, mut cell) = support::cell_from_artifacts(&artifacts, &base);
    // 5 repeating patterns * 25,000 = 125,000 rows (> 124,000 residue cap):
    // 0: inactive (`XX`) -> pass
    // 1: within cap -> pass
    // 2: over cap, no exception -> held
    // 3: over cap, wholesale lifts -> pass
    // 4: over cap, audit_block overrides wholesale -> held
    let sample = vec![
        Line {
            invoice: "IN0",
            quantity: 1,
            price: "50".into(),
            customer: Some("a"),
            country: "XX",
            occurred_at: INSTANTS[0],
        },
        Line {
            invoice: "IN1",
            quantity: 1,
            price: "5".into(),
            customer: Some("b"),
            country: "FR",
            occurred_at: INSTANTS[0],
        },
        Line {
            invoice: "IN2",
            quantity: 1,
            price: "50".into(),
            customer: Some("c"),
            country: "FR",
            occurred_at: INSTANTS[0],
        },
        Line {
            invoice: "OK3",
            quantity: 12,
            price: "50".into(),
            customer: Some("d"),
            country: "FR",
            occurred_at: INSTANTS[0],
        },
        Line {
            invoice: "AUDIT",
            quantity: 12,
            price: "50".into(),
            customer: Some("e"),
            country: "FR",
            occurred_at: INSTANTS[0],
        },
    ];
    let (_row_findings, row_held, row_trusted) = per_row(&mut cell, &sample);
    assert_eq!(row_held, BTreeSet::from([3, 5]));
    assert_eq!(row_trusted, BTreeSet::from([1, 2, 4]));

    const TOTAL_ROWS: usize = 125_000;
    let mut csv_bytes = String::with_capacity(TOTAL_ROWS * 64);
    csv_bytes.push_str(&format!("{}\n", FIELDS.join(",")));
    for index in 0..TOTAL_ROWS {
        let line = &sample[index % sample.len()];
        writeln!(
            csv_bytes,
            "line-{},{},c,{},{},{},{},{}",
            index + 1,
            line.invoice,
            line.quantity,
            line.price,
            line.customer.unwrap_or_default(),
            line.country,
            line.occurred_at,
        )
        .expect("csv line");
    }
    let (_engine_dir, engine) = engine_with("1GB");
    let mut request = land_request(&cell, csv_bytes.as_bytes(), FileFormat::Csv);
    request.residue_page = norn_cell::analytics::RESIDUE_PAGE_ROWS;
    let landed = engine
        .land(&request, |mut file| {
            file.write_all(csv_bytes.as_bytes()).expect("source");
            Ok(())
        })
        .expect("125k rows with row-tree unless law must not hit residue cap");
    assert_eq!(
        landed.residue_total, 0,
        "125k rows with row-tree unless law must produce zero residue"
    );
    assert_eq!(landed.load.rows, 125_000);
    assert_eq!(landed.held_by_rules, 50_000);
    assert_eq!(landed.data.as_ref().expect("data").rows, 75_000);
}

/// Lane A5.1: first-class `RowPredicate::Coalesce` preserves the fallback when
/// the LHS dynamically evaluates to `null` (fixing nested-null drop).
#[test]
fn coalesce_nested_null_and_nullable_conditionals_match_per_row() {
    let rules: [(&str, &str, &str); 5] = [
        (
            "nested_num_coalesce",
            "error",
            "((self.country == 'XX' ? null : self.price) ?? 99) <= 50",
        ),
        (
            "nested_text_coalesce",
            "warning",
            "((self.country == 'XX' ? null : self.customer) ?? 'fallback') != 'fallback'",
        ),
        (
            "literal_null_coalesce",
            "error",
            "(null ?? self.price) >= 0",
        ),
        (
            "deep_nested_coalesce",
            "error",
            "((self.country == 'XX' ? null : (self.invoice == 'C1' ? null : self.price)) ?? 50) <= 50",
        ),
        (
            "nullable_coalesce_eq_null",
            "warning",
            "((self.country == 'XX' ? null : self.price) ?? null) != null",
        ),
    ];
    let (_dir, mut cell) = cell(&rules);
    let table = cell.store().analytics_table(TYPE).expect("table");
    let checks = cell.store().dataset_checks(TYPE, &table).expect("checks");
    assert_eq!(checks.len(), rules.len());
    for check in &checks {
        assert!(
            check.predicate.is_some(),
            "{} must lower into RowPredicate::Coalesce",
            check.label
        );
    }

    // All columns present: zero residue across every nested-null branch!
    let present_lines = vec![
        // country == 'XX': conditional produces null -> ?? 99 (>50, fails), ?? 'fallback' (fails), ?? 50 (<=50, passes), ?? null == null (fails)
        Line {
            invoice: "IN1",
            quantity: 1,
            price: "10".into(),
            customer: Some("ann"),
            country: "XX",
            occurred_at: INSTANTS[0],
        },
        // country == 'FR', invoice == 'C1': inner conditional produces null -> ?? 50 (passes), outer non-null price 10 (passes)
        Line {
            invoice: "C1",
            quantity: 1,
            price: "10".into(),
            customer: Some("bob"),
            country: "FR",
            occurred_at: INSTANTS[0],
        },
        // country == 'FR', price 60 (>50): non-null LHS fails nested_num_coalesce and deep_nested_coalesce
        Line {
            invoice: "IN3",
            quantity: 1,
            price: "60".into(),
            customer: Some("cat"),
            country: "FR",
            occurred_at: INSTANTS[0],
        },
        // negative price: fails literal_null_coalesce
        Line {
            invoice: "IN4",
            quantity: 1,
            price: "-5".into(),
            customer: Some("dan"),
            country: "FR",
            occurred_at: INSTANTS[0],
        },
    ];
    let (row_findings, row_held, row_trusted) = per_row(&mut cell, &present_lines);
    let bulk = set_based(&cell, &present_lines);
    assert_eq!(
        bulk.residue, 0,
        "present-column nested-null coalesce decides in SQL"
    );
    assert_eq!(bulk.findings, row_findings);
    assert_eq!(bulk.held, row_held);
    assert_eq!(bulk.data, row_trusted);

    // Optional column absent (`customer: None`): falls back to residue and still matches per-row!
    let (_dir2, mut cell2) = cell(&rules);
    let mut mixed_lines = present_lines;
    mixed_lines.push(Line {
        invoice: "IN5",
        quantity: 1,
        price: "10".into(),
        customer: None,
        country: "FR",
        occurred_at: INSTANTS[0],
    });
    let (row_findings, row_held, row_trusted) = per_row(&mut cell2, &mixed_lines);
    let bulk = set_based(&cell2, &mixed_lines);
    assert_eq!(bulk.residue, 1);
    assert_eq!(bulk.findings, row_findings);
    assert_eq!(bulk.held, row_held);
    assert_eq!(bulk.data, row_trusted);
}

/// Observe arbitrary JSON rows through `bench.dataset.mapping` for a custom
/// schema test cell and return `(findings, held, trusted)`.
fn per_row_custom(
    cell: &mut Cell,
    object_type: &str,
    rows: &[serde_json::Value],
) -> (BTreeSet<Finding>, BTreeSet<u64>, BTreeSet<u64>) {
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
    for (index, payload_json) in rows.iter().enumerate() {
        let row = u64::try_from(index + 1).expect("row");
        let id = payload_json["id"].as_str().expect("id").to_owned();
        let text = payload_json.to_string();
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
        let _processed = cell.process_mapping_bundle(&bundle);
        let model = cell.store().model().clone();
        let catalog = norn_kernel::TypeCatalog::new(model.object_types())
            .with_interfaces(model.interfaces())
            .with_property_types(model.property_types())
            .with_governing_laws(model.law_artifacts());
        let reader = cell.store().read();
        let Some(record) = reader.object("main", object_type, &id).expect("read") else {
            held.insert(row);
            continue;
        };
        for finding in reader
            .observation_findings("main", object_type, &id)
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

/// Set-based land + settle for an arbitrary schema CSV.
fn set_based_custom(cell: &Cell, object_type: &str, csv_text: &str) -> Bulk {
    let store = cell.store();
    let table = store.analytics_table(object_type).expect("table");
    let checks = store.dataset_checks(object_type, &table).expect("checks");
    let bytes = csv_text.as_bytes();
    let mut request = LandRequest {
        load: LoadRequest {
            tenant: tenant(),
            table,
            load: "land-1".into(),
            file: FileSource {
                url: String::new(),
                uri: "s3://synthetic/uploads/custom".into(),
                sha256: norn_cell::analytics::hex_digest(bytes),
                format: FileFormat::Csv,
            },
            headers: BTreeMap::new(),
        },
        pins: store
            .dataset_pins(object_type, &BTreeMap::new(), "bench.dataset.mapping@test")
            .expect("pins"),
        landing: "landing-1".into(),
        attempt: String::new(),
        checks,
        residue_page: 7,
    };
    let (engine_dir, engine) = engine();
    let mut landed = engine
        .land(&request, |mut file| {
            file.write_all(bytes).expect("source");
            Ok(())
        })
        .expect("landed");
    request.attempt.clone_from(&landed.attempt);
    let mut residue = 0u64;
    while landed.residue_total > 0 {
        let page = landed.residue.clone();
        residue += u64::try_from(page.len()).expect("page");
        let findings = store
            .residue_findings(object_type, &request.load.table, &page)
            .expect("per-row residue");
        let through = page.last().expect("page").row;
        landed = engine
            .settle(&SettleRequest {
                land: request.clone(),
                through,
                findings,
            })
            .expect("settled");
    }
    let (data_path, held_path) = files(engine_dir.path(), &landed.attempt);
    let data = parquet_rows(&data_path, "CAST(NULL AS VARCHAR), CAST(NULL AS BOOLEAN)")
        .into_iter()
        .map(|(row, ..)| row)
        .collect();
    let mut findings = BTreeSet::new();
    let mut held = BTreeSet::new();
    for (row, rule, holds) in parquet_rows(&held_path, "\":rule\", \":holds\"") {
        if let Some(rule) = rule {
            findings.insert((row, rule, holds));
            if holds {
                held.insert(row);
            }
        } else {
            held.insert(row);
        }
    }
    Bulk {
        findings,
        held,
        data,
        residue,
        residue_rows: Vec::new(),
    }
}

/// Lane A5.1: civil-day `date(...)` and UTC-instant `timestamp(...)` ordering
/// and equality lower into SQL with `0001..=9999` guards and match per-row.
#[test]
fn civil_date_and_timestamp_ordering_and_guards_match_per_row() {
    use norn_cell::analytics::RowPredicate as P;
    const WINDOW_TYPE: &str = "bench.TemporalWindow";
    let schema_fields = vec![
        Field {
            id: "id",
            kind: json!({"kind":"text"}),
            required: true,
        },
        Field {
            id: "start_date",
            kind: json!({"kind":"date"}),
            required: true,
        },
        Field {
            id: "end_date",
            kind: json!({"kind":"date"}),
            required: true,
        },
        Field {
            id: "opt_date",
            kind: json!({"kind":"date"}),
            required: false,
        },
        Field {
            id: "created_at",
            kind: json!({"kind":"timestamp","timezone":"utc"}),
            required: true,
        },
        Field {
            id: "updated_at",
            kind: json!({"kind":"timestamp","timezone":"utc"}),
            required: true,
        },
        Field {
            id: "note",
            kind: json!({"kind":"text"}),
            required: true,
        },
    ];
    let rules = [
        (
            "date_ordered",
            "error",
            "date(self.start_date) <= date(self.end_date)",
        ),
        (
            "date_after_2020",
            "error",
            "date(self.start_date) >= date('2020-01-01')",
        ),
        (
            "opt_date_before_end",
            "warning",
            "date(self.opt_date ?? '2026-01-01') <= date(self.end_date)",
        ),
        (
            "ts_ordered",
            "error",
            "timestamp(self.created_at) <= timestamp(self.updated_at)",
        ),
        (
            "ts_not_midnight",
            "warning",
            "timestamp(self.created_at) != timestamp('2026-01-01T00:00:00Z')",
        ),
        ("raw_date_eq", "warning", "self.start_date != '2025-12-31'"),
    ];
    let (_dir, mut cell) = support::cell(WINDOW_TYPE, &schema_fields, &rules);
    let table = cell.store().analytics_table(WINDOW_TYPE).expect("table");
    let checks = cell
        .store()
        .dataset_checks(WINDOW_TYPE, &table)
        .expect("checks");
    assert_eq!(checks.len(), rules.len());
    for check in &checks {
        assert!(
            check.predicate.is_some(),
            "{} must lower into RowPredicate",
            check.label
        );
    }

    // Negative typing checks: raw wire dates/timestamps cannot order; mixed
    // Date/Timestamp cannot compare; year 0000 literals and free text reject.
    assert!(
        P::Less(
            Box::new(P::Column("start_date".into())),
            Box::new(P::Column("end_date".into()))
        )
        .kind(&table)
        .is_none()
    );
    assert!(
        P::Less(
            Box::new(P::Column("created_at".into())),
            Box::new(P::Column("updated_at".into()))
        )
        .kind(&table)
        .is_none()
    );
    assert!(
        P::Less(
            Box::new(P::Date(Box::new(P::Column("start_date".into())))),
            Box::new(P::Timestamp(Box::new(P::Column("created_at".into()))))
        )
        .kind(&table)
        .is_none()
    );
    assert!(
        P::Date(Box::new(P::Text("0000-01-01".into())))
            .kind(&table)
            .is_none()
    );
    assert!(
        P::Timestamp(Box::new(P::Text("0000-12-31T23:00:00-02:00".into())))
            .kind(&table)
            .is_none()
    );
    assert!(
        P::Timestamp(Box::new(P::Text("0001-01-01T00:00:00+02:00".into())))
            .kind(&table)
            .is_none()
    );
    assert!(
        P::Date(Box::new(P::Column("note".into())))
            .kind(&table)
            .is_none()
    );
    assert!(P::Date(Box::new(P::Null)).kind(&table).is_none());

    // Valid 0001..=9999 rows with all columns present -> zero residue!
    let rows = vec![
        // 1: leap day, all pass
        json!({
            "id": "w-1", "start_date": "2024-02-28", "end_date": "2024-02-29",
            "opt_date": "2024-02-28",
            "created_at": "2024-02-29T00:00:00.000000001Z",
            "updated_at": "2024-02-29T00:00:00.000000002Z",
            "note": "ok"
        }),
        // 2: cross-offset instant equality: 02:00+02:00 == 00:00Z -> triggers ts_not_midnight!
        json!({
            "id": "w-2", "start_date": "2026-01-01", "end_date": "2026-01-02",
            "opt_date": "2026-01-01",
            "created_at": "2026-01-01T02:00:00+02:00",
            "updated_at": "2026-01-01T00:00:00Z",
            "note": "offset-eq"
        }),
        // 3: cross-offset ordering failure: created_at (03:00+02:00 = 01:00Z) > updated_at (00:30Z)
        json!({
            "id": "w-3", "start_date": "2026-05-01", "end_date": "2026-04-30",
            "opt_date": "2026-05-01",
            "created_at": "2026-05-01T03:00:00+02:00",
            "updated_at": "2026-05-01T00:30:00Z",
            "note": "reversed"
        }),
        // 4: pre-1970 nanosecond ordering + raw_date_eq trigger + date_after_2020 violation
        json!({
            "id": "w-4", "start_date": "2025-12-31", "end_date": "2025-12-31",
            "opt_date": "2025-12-31",
            "created_at": "1969-12-31T23:59:59.500000000Z",
            "updated_at": "1970-01-01T00:00:00.000000000Z",
            "note": "pre-epoch"
        }),
        // 5: before 2020-01-01
        json!({
            "id": "w-5", "start_date": "2019-12-31", "end_date": "2020-01-01",
            "opt_date": "2019-12-31",
            "created_at": "2019-12-31T23:59:59Z",
            "updated_at": "2020-01-01T00:00:00Z",
            "note": "pre-2020"
        }),
    ];
    let mut csv_text = "id,start_date,end_date,opt_date,created_at,updated_at,note\n".to_owned();
    for row in &rows {
        writeln!(
            csv_text,
            "{},{},{},{},{},{},{}",
            row["id"].as_str().expect("id"),
            row["start_date"].as_str().expect("start_date"),
            row["end_date"].as_str().expect("end_date"),
            row["opt_date"].as_str().unwrap_or(""),
            row["created_at"].as_str().expect("created_at"),
            row["updated_at"].as_str().expect("updated_at"),
            row["note"].as_str().expect("note"),
        )
        .expect("csv");
    }
    let (row_findings, row_held, row_trusted) = per_row_custom(&mut cell, WINDOW_TYPE, &rows);
    let bulk = set_based_custom(&cell, WINDOW_TYPE, &csv_text);
    assert_eq!(
        bulk.residue, 0,
        "valid 0001..=9999 temporal rows decide in SQL"
    );
    assert_eq!(bulk.findings, row_findings);
    assert_eq!(bulk.held, row_held);
    assert_eq!(bulk.data, row_trusted);
    assert_eq!(bulk.data, BTreeSet::from([1]));
    assert_eq!(bulk.held, BTreeSet::from([2, 3, 4, 5]));

    // Raw timestamp equality (`self.created_at == self.updated_at`) must fall
    // back to residue because per-row canonicalizes offsets to UTC before ZEN
    // runs (`2026-01-01T02:00:00+02:00` == `2026-01-01T00:00:00Z`).
    let (_dir_raw_ts, mut cell_raw_ts) = support::cell(
        WINDOW_TYPE,
        &schema_fields,
        &[("raw_ts_eq", "error", "self.created_at == self.updated_at")],
    );
    let raw_ts_checks = cell_raw_ts
        .store()
        .dataset_checks(
            WINDOW_TYPE,
            &cell_raw_ts
                .store()
                .analytics_table(WINDOW_TYPE)
                .expect("table"),
        )
        .expect("checks");
    assert!(
        raw_ts_checks[0].predicate.is_none(),
        "raw Timestamp equality must not lower directly over un-canonicalized text"
    );
    let raw_ts_rows = vec![json!({
        "id": "w-raw-ts", "start_date": "2026-01-01", "end_date": "2026-01-02",
        "opt_date": "2026-01-01",
        "created_at": "2026-01-01T02:00:00+02:00",
        "updated_at": "2026-01-01T00:00:00Z",
        "note": "offset-raw-eq"
    })];
    let raw_ts_csv = "id,start_date,end_date,opt_date,created_at,updated_at,note\n\
                      w-raw-ts,2026-01-01,2026-01-02,2026-01-01,2026-01-01T02:00:00+02:00,2026-01-01T00:00:00Z,offset-raw-eq\n";
    let (raw_ts_findings, raw_ts_held, raw_ts_trusted) =
        per_row_custom(&mut cell_raw_ts, WINDOW_TYPE, &raw_ts_rows);
    let raw_ts_bulk = set_based_custom(&cell_raw_ts, WINDOW_TYPE, raw_ts_csv);
    assert_eq!(raw_ts_bulk.residue, 1);
    assert_eq!(raw_ts_bulk.findings, raw_ts_findings);
    assert_eq!(raw_ts_bulk.held, raw_ts_held);
    assert_eq!(raw_ts_bulk.data, raw_ts_trusted);
    assert_eq!(raw_ts_bulk.data, BTreeSet::from([1]));

    // Year-0000 guard test: DuckDB/DataFusion loaders accept 0000-06-15, while
    // ZEN Temporal::date only admits 0001..=9999 -> must fall back to residue!
    let (_dir0, mut cell0) = support::cell(
        WINDOW_TYPE,
        &schema_fields,
        &[(
            "date_ordered",
            "error",
            "date(self.start_date) <= date(self.end_date)",
        )],
    );
    let year0_rows = vec![json!({
        "id": "w-0", "start_date": "0000-06-15", "end_date": "2026-01-01",
        "opt_date": "2026-01-01",
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-01T00:00:01Z",
        "note": "year0"
    })];
    let year0_csv = "id,start_date,end_date,opt_date,created_at,updated_at,note\n\
                     w-0,0000-06-15,2026-01-01,2026-01-01,2026-01-01T00:00:00Z,2026-01-01T00:00:01Z,year0\n";
    let (row_findings0, row_held0, row_trusted0) =
        per_row_custom(&mut cell0, WINDOW_TYPE, &year0_rows);
    let bulk0 = set_based_custom(&cell0, WINDOW_TYPE, year0_csv);
    assert_eq!(
        bulk0.residue, 1,
        "year 0000 must go to residue via 0001..=9999 guard"
    );
    assert_eq!(bulk0.findings, row_findings0);
    assert_eq!(bulk0.held, row_held0);
    assert_eq!(bulk0.data, row_trusted0);
}

/// Lane A5.1: DuckDB byte-fuel uses `octet_length` (UTF-8 bytes, matching ZEN)
/// while `PointLaw::{MinLength, MaxLength}` preserves Unicode char count
/// (`entity_instance.rs:1155`).
#[test]
fn multibyte_utf8_byte_fuel_and_char_length_preserve_contracts() {
    // Part A: 4-byte UTF-8 `'😀'` whose char count fits under `row_fuel`,
    // while its UTF-8 byte length (4x) exceeds `row_fuel`.
    let (_dir, cell) = wide_typed_cell();
    let store = cell.store();
    let table = store.analytics_table(TYPE).expect("table");
    let checks = store.dataset_checks(TYPE, &table).expect("checks");
    let budget = norn_cell::analytics::row_fuel();
    let max_bytes = usize::try_from((budget - checks[0].fuel) / 2).expect("width");
    // 4-byte emoji: `over_chars` has `2 * over_chars < budget - static` (would
    // have passed `strlen`), yet `2 * (4 * over_chars) > budget - static`!
    let ok_chars = max_bytes / 4;
    let over_chars = ok_chars + 1;
    // 125,000 4-byte chars = 500,000 UTF-8 bytes per operand (1,000,000+ fuel
    // in ZEN), yet only 250,000 chars total (would have slipped past `strlen`).
    let exhaust_chars = 125_000;
    assert!(
        exhaust_chars * 2 < max_bytes * 2,
        "char count alone is well below byte budget"
    );
    assert!(over_chars * 4 > max_bytes, "UTF-8 bytes exceed byte budget");
    let decimals: Vec<String> = (0..TYPED).map(|i| format!("d{i}")).collect();
    let mut csv_bytes = format!("id,a,b,{}\n", decimals.join(","));
    let ones = vec!["1"; TYPED].join(",");
    let ok_text = "😀".repeat(ok_chars);
    let over_text = "😀".repeat(over_chars);
    let exhaust_text = "😀".repeat(exhaust_chars);
    writeln!(csv_bytes, "line-1,{ok_text},{ok_text},{ones}").expect("row 1");
    writeln!(csv_bytes, "line-2,{over_text},{over_text},{ones}").expect("row 2");
    writeln!(csv_bytes, "line-3,{exhaust_text},{exhaust_text},{ones}").expect("row 3");
    let (_engine_dir, engine) = engine_with("2GB");
    let (request, landed) = land(&engine, &cell, csv_bytes.as_bytes(), FileFormat::Csv);
    let residue_ids: Vec<u64> = landed.residue.iter().map(|r| r.row).collect();
    assert_eq!(
        residue_ids,
        vec![2, 3],
        "only the 4-byte UTF-8 rows past the UTF-8 byte fuel bound are residue"
    );
    store
        .residue_findings(TYPE, &request.load.table, &landed.residue[1..2])
        .expect_err("per-row ZEN evaluator exhausts fuel on the 500KB UTF-8 row");

    // Part B: `minLength: 3, maxLength: 3` counts Unicode chars, NOT UTF-8 bytes!
    const LEN_TYPE: &str = "bench.CharBound";
    let (mut artifacts, base) = support::artifacts(
        LEN_TYPE,
        &[
            Field {
                id: "id",
                kind: json!({"kind":"text"}),
                required: true,
            },
            Field {
                id: "code",
                kind: json!({"kind":"text"}),
                required: true,
            },
        ],
        &[],
    );
    let mut entity_json = serde_json::to_value(&artifacts[0]).expect("entity");
    entity_json["payload"]["objectType"]["properties"][1]["constraints"] =
        json!({"minLength": 3, "maxLength": 3});
    artifacts[0] = serde_json::from_value(entity_json).expect("entity");
    let (_len_dir, mut len_cell) = support::cell_from_artifacts(&artifacts, &base);
    let rows = vec![
        json!({"id": "c-1", "code": "😀😀😀"}), // 3 chars, 12 bytes -> admitted
        json!({"id": "c-2", "code": "😀😀"}),   // 2 chars, 8 bytes  -> too_short
        json!({"id": "c-3", "code": "😀😀😀😀"}), // 4 chars, 16 bytes -> too_long
    ];
    let len_csv = "id,code\nc-1,😀😀😀\nc-2,😀😀\nc-3,😀😀😀😀\n";
    let (row_findings, row_held, row_trusted) = per_row_custom(&mut len_cell, LEN_TYPE, &rows);
    let bulk = set_based_custom(&len_cell, LEN_TYPE, len_csv);
    assert_eq!(bulk.findings, row_findings);
    assert_eq!(bulk.held, row_held);
    assert_eq!(bulk.data, row_trusted);
    assert_eq!(bulk.data, BTreeSet::from([1]));
    assert_eq!(bulk.held, BTreeSet::from([2, 3]));
}

/// Lane A5.1: 3-way differential test proving exact agreement across
/// `Per-Row == DuckDB == DataFusion` (and identical residue counts between
/// DuckDB and DataFusion) over nested-null `Coalesce`, `Conditional`,
/// row-tree `unless` exception trees, `timestamp(...)` ordering, NFC/NFD text,
/// and optional nulls.
#[cfg(all(feature = "duckdb", feature = "datafusion", feature = "native"))]
#[test]
fn three_way_differential_per_row_duckdb_and_datafusion_agree() {
    use norn_cell::analytics::datafusion::{
        Engine as DataFusionEngine, Settings as DataFusionSettings,
    };
    use norn_cell::analytics::engine::{Engine as DuckDbEngine, Settings as DuckDbSettings};

    let (mut artifacts, base) = support::artifacts(
        TYPE,
        &fields(),
        &[
            (
                "nested_coalesce",
                "error",
                "((self.country == 'XX' ? null : self.price) ?? 25) <= 20",
            ),
            (
                "ts_floor",
                "warning",
                "timestamp(self.occurred_at) >= timestamp('2026-10-01T09:30:00.000000001Z')",
            ),
            ("price_capped", "error", "self.price <= 10"),
            (
                "nfc_customer",
                "warning",
                "(self.customer ?? 'nobody') != '\u{e9}'",
            ),
        ],
    );
    let cap_law = artifacts
        .iter_mut()
        .find(|artifact| artifact.artifact_id.as_str() == "bench.price_capped.law")
        .expect("cap law");
    let mut cap_json = serde_json::to_value(&*cap_law).expect("cap json");
    cap_json["payload"]["condition"] = json!("self.country != 'XX'");
    cap_json["payload"]["exceptions"] = json!([
        {
            "id": "wholesale",
            "overrides": "price_capped",
            "condition": "self.quantity >= 10",
            "rationale": "Bulk orders are exempt.",
            "source": {"artifactId": "bench.price_capped.law", "pointer": "/payload/exceptions/0"}
        },
        {
            "id": "audit_block",
            "overrides": "wholesale",
            "condition": "self.invoice == 'AUDIT'",
            "rationale": "Audited bulk orders still enforce the cap.",
            "source": {"artifactId": "bench.price_capped.law", "pointer": "/payload/exceptions/1"}
        }
    ]);
    *cap_law = serde_json::from_value(cap_json).expect("updated cap law");

    let strategy = proptest::collection::vec(
        (
            proptest::sample::select(&["IN1", "AUDIT", "C1"][..]),
            proptest::sample::select(&[1_i64, 12][..]),
            proptest::sample::select(&["5", "15", "30"][..]),
            proptest::sample::select(&[None, Some("ann"), Some("\u{e9}"), Some("e\u{301}")][..]),
            proptest::sample::select(&["FR", "XX", "UK"][..]),
            proptest::sample::select(&INSTANTS[..]),
        ),
        1..=10,
    );
    let mut runner =
        proptest::test_runner::TestRunner::new(proptest::test_runner::Config::with_cases(24));
    runner
        .run(&strategy, |batch| {
            let lines: Vec<Line> = batch
                .into_iter()
                .map(
                    |(invoice, quantity, price, customer, country, occurred_at)| Line {
                        invoice,
                        quantity,
                        price: price.to_owned(),
                        customer,
                        country,
                        occurred_at,
                    },
                )
                .collect();
            let (_dir, mut cell) = support::cell_from_artifacts(&artifacts, &base);
            let (row_findings, row_held, row_trusted) = per_row(&mut cell, &lines);
            let bytes = parquet(&lines);
            let base_request = land_request(&cell, &bytes, FileFormat::Parquet);
            let store = cell.store();

            // DuckDB
            let duck_dir = tempfile::tempdir().expect("duck dir");
            let duck = DuckDbEngine::new(DuckDbSettings {
                root: duck_dir.path().to_path_buf(),
                memory_limit: "256MB".into(),
                threads: 1,
            })
            .expect("duckdb");
            let mut duck_req = base_request.clone();
            let mut duck_landed = duck
                .land(&duck_req, |mut f| {
                    f.write_all(&bytes).expect("src");
                    Ok(())
                })
                .expect("duck land");
            duck_req.attempt.clone_from(&duck_landed.attempt);
            let mut duck_residue = 0u64;
            while duck_landed.residue_total > 0 {
                let page = duck_landed.residue.clone();
                duck_residue += u64::try_from(page.len()).expect("len");
                let findings = store
                    .residue_findings(TYPE, &duck_req.load.table, &page)
                    .expect("duck residue");
                let through = page.last().expect("page").row;
                duck_landed = duck
                    .settle(&SettleRequest {
                        land: duck_req.clone(),
                        through,
                        findings,
                    })
                    .expect("duck settle");
            }
            let (duck_data_p, duck_held_p) = files(duck_dir.path(), &duck_landed.attempt);
            let duck_data: BTreeSet<u64> =
                parquet_rows(&duck_data_p, "CAST(NULL AS VARCHAR), CAST(NULL AS BOOLEAN)")
                    .into_iter()
                    .map(|(r, ..)| r)
                    .collect();
            let mut duck_findings = BTreeSet::new();
            let mut duck_held = BTreeSet::new();
            for (r, rule, holds) in parquet_rows(&duck_held_p, "\":rule\", \":holds\"") {
                if let Some(rule) = rule {
                    duck_findings.insert((r, rule, holds));
                    if holds {
                        duck_held.insert(r);
                    }
                } else {
                    duck_held.insert(r);
                }
            }

            // DataFusion
            let df_dir = tempfile::tempdir().expect("df dir");
            let df = DataFusionEngine::new(DataFusionSettings {
                root: df_dir.path().to_path_buf(),
                memory_limit: "256MB".into(),
                threads: 1,
            })
            .expect("datafusion");
            let mut df_req = base_request;
            let mut df_landed = df
                .land(&df_req, |mut f| {
                    f.write_all(&bytes).expect("src");
                    Ok(())
                })
                .expect("df land");
            df_req.attempt.clone_from(&df_landed.attempt);
            let mut df_residue = 0u64;
            while df_landed.residue_total > 0 {
                let page = df_landed.residue.clone();
                df_residue += u64::try_from(page.len()).expect("len");
                let findings = store
                    .residue_findings(TYPE, &df_req.load.table, &page)
                    .expect("df residue");
                let through = page.last().expect("page").row;
                df_landed = df
                    .settle(&SettleRequest {
                        land: df_req.clone(),
                        through,
                        findings,
                    })
                    .expect("df settle");
            }
            let (df_data_p, df_held_p) = files(df_dir.path(), &df_landed.attempt);
            let df_data: BTreeSet<u64> =
                parquet_rows(&df_data_p, "CAST(NULL AS VARCHAR), CAST(NULL AS BOOLEAN)")
                    .into_iter()
                    .map(|(r, ..)| r)
                    .collect();
            let mut df_findings = BTreeSet::new();
            let mut df_held = BTreeSet::new();
            for (r, rule, holds) in parquet_rows(&df_held_p, "\":rule\", \":holds\"") {
                if let Some(rule) = rule {
                    df_findings.insert((r, rule, holds));
                    if holds {
                        df_held.insert(r);
                    }
                } else {
                    df_held.insert(r);
                }
            }

            proptest::prop_assert_eq!(&duck_findings, &row_findings);
            proptest::prop_assert_eq!(&df_findings, &row_findings);
            proptest::prop_assert_eq!(&duck_held, &row_held);
            proptest::prop_assert_eq!(&df_held, &row_held);
            proptest::prop_assert_eq!(&duck_data, &row_trusted);
            proptest::prop_assert_eq!(&df_data, &row_trusted);
            proptest::prop_assert_eq!(duck_residue, df_residue);
            Ok(())
        })
        .expect("Per-Row == DuckDB == DataFusion across all batches");

    // Sol 6.2 R1 (Medium 5): DECIMAL(38,12) branch widening overflow (`10^26`
    // coalesced with `0.000000000001` requires `10^38`) must send the row to
    // residue in BOTH DuckDB and DataFusion and agree with per-row.
    const BIG_TYPE: &str = "bench.BigDecimalBranch";
    let big_fields = vec![
        Field {
            id: "id",
            kind: json!({"kind":"text"}),
            required: true,
        },
        Field {
            id: "large",
            kind: json!({"kind":"decimal","scale":0,"unit":{"system":"ucum","code":"1"},
                "additivity":{"kind":"additive"}}),
            required: true,
        },
    ];
    let (_big_dir, big_cell) = support::cell(
        BIG_TYPE,
        &big_fields,
        &[(
            "wide_coalesce",
            "error",
            "((self.id == 'null' ? null : self.large) ?? 0.000000000001) > 0",
        )],
    );
    let ten_26 = "100000000000000000000000000";
    let big_csv = format!("id,large\nb-1,{ten_26}\nb-2,5\n");
    let big_store = big_cell.store();
    let big_table = big_store.analytics_table(BIG_TYPE).expect("table");
    let big_checks = big_store
        .dataset_checks(BIG_TYPE, &big_table)
        .expect("checks");
    let big_req_base = LandRequest {
        load: LoadRequest {
            tenant: tenant(),
            table: big_table,
            load: "land-big".into(),
            file: FileSource {
                url: String::new(),
                uri: "s3://synthetic/uploads/big".into(),
                sha256: norn_cell::analytics::hex_digest(big_csv.as_bytes()),
                format: FileFormat::Csv,
            },
            headers: BTreeMap::new(),
        },
        pins: big_store
            .dataset_pins(BIG_TYPE, &BTreeMap::new(), "bench.dataset.mapping@test")
            .expect("pins"),
        landing: "landing-big".into(),
        attempt: String::new(),
        checks: big_checks,
        residue_page: 7,
    };
    let duck_big_dir = tempfile::tempdir().expect("duck big dir");
    let duck_big = DuckDbEngine::new(DuckDbSettings {
        root: duck_big_dir.path().to_path_buf(),
        memory_limit: "256MB".into(),
        threads: 1,
    })
    .expect("duck");
    let duck_big_landed = duck_big
        .land(&big_req_base, |mut f| {
            f.write_all(big_csv.as_bytes()).expect("csv");
            Ok(())
        })
        .expect("duck land");
    let df_big_dir = tempfile::tempdir().expect("df big dir");
    let df_big = DataFusionEngine::new(DataFusionSettings {
        root: df_big_dir.path().to_path_buf(),
        memory_limit: "256MB".into(),
        threads: 1,
    })
    .expect("df");
    let df_big_landed = df_big
        .land(&big_req_base, |mut f| {
            f.write_all(big_csv.as_bytes()).expect("csv");
            Ok(())
        })
        .expect("df land");
    assert_eq!(
        duck_big_landed.residue_total, 1,
        "DuckDB sends 10^26 * 10^12 widening overflow to residue"
    );
    assert_eq!(
        df_big_landed.residue_total, 1,
        "DataFusion matches DuckDB's DECIMAL(38, s) storage bound on branch widening"
    );
    assert_eq!(duck_big_landed.residue, df_big_landed.residue);
    assert_eq!(
        big_store
            .residue_findings(BIG_TYPE, &big_req_base.load.table, &duck_big_landed.residue)
            .expect("per-row settles 10^26 cleanly"),
        Vec::new()
    );
}

/// Sol 6.2 R1 (High 4): `RowPlan::fuel` pays for referenced column key bytes
/// so parenthesized receivers (`(self).<long_key>`) that compile to ZEN's
/// metered `Fetch` opcode cannot bypass the S2 per-row fuel bound.
#[test]
fn row_plan_fuel_accounts_for_metered_property_key_bytes() {
    let long_key: &'static str = Box::leak(format!("k_{}", "x".repeat(98)).into_boxed_str());
    let (_dir, cell) = support::cell_with(
        "bench.LongKey",
        &[
            Field {
                id: "id",
                kind: json!({"kind":"text"}),
                required: true,
            },
            Field {
                id: long_key,
                kind: json!({"kind":"text"}),
                required: true,
            },
        ],
        &[],
        &[(
            "paren_key",
            &format!("(self).{long_key} == (self).{long_key}"),
        )],
    );
    let table = cell
        .store()
        .analytics_table("bench.LongKey")
        .expect("table");
    let checks = cell
        .store()
        .dataset_checks("bench.LongKey", &table)
        .expect("checks");
    assert!(checks[0].predicate.is_some());
    let key_bytes = u64::try_from(long_key.len()).expect("len");
    assert!(
        checks[0].fuel >= norn_cell::analytics::rule_fuel(3) + 2 * key_bytes,
        "fuel {} must include both column key lengths ({})",
        checks[0].fuel,
        2 * key_bytes
    );
}
