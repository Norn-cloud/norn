//! MANIFEST-LAWS-1 (#1381): a dataset's laws run set-based on the engine and
//! hold back exactly what the per-row path holds back. The same rows go
//! through both paths. Per row, they are mapped and committed one at a time
//! as observations, and each observation's findings and quarantine are read
//! back. As a set, they are landed on the engine with every law checked in
//! one pass and the residue settled by the per-row evaluator. The two paths
//! must agree on the held-back rows and on every (row, rule) finding.
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
    FileFormat, FileSource, LandRequest, LoadRequest, SettleRequest, TenantId,
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
    ("customer_named", "warning", "(self.customer ?? 'nobody') != 'void'"),
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

#[derive(Debug, Clone)]
struct Line {
    invoice: &'static str,
    quantity: i64,
    price_milli: i64,
    customer: Option<&'static str>,
    country: &'static str,
}

fn price(line: &Line) -> String {
    norn_ir::Decimal::parse(&format!("{}e-3", line.price_milli))
        .expect("price")
        .canonical()
}

const AT: &str = "2026-10-01T09:30:00Z";

/// The per-row path: each line observed through the mapping and committed
/// on its own. Returns each line's findings (`(line, rule)`) and held-back lines.
fn per_row(cell: &mut Cell, lines: &[Line]) -> (BTreeSet<(u64, String)>, BTreeSet<u64>) {
    let input = norn_cell::mapping_bundle::MappingInput {
        mapping: "bench.dataset.mapping".into(),
        dataset: "Rows".into(),
        connector_instance: "public".into(),
        partition: "sample".into(),
        recorded_at: norn_ir::provenance::UtcTimestamp("2026-11-04T00:00:00Z".into()),
    };
    let mut findings = BTreeSet::new();
    let mut held = BTreeSet::new();
    for (index, line) in lines.iter().enumerate() {
        let row = u64::try_from(index + 1).expect("row");
        let id = format!("line-{row}");
        let mut payload = json!({"id":id,"invoice":line.invoice,"code":"c","quantity":line.quantity,
            "country":line.country,"occurred_at":AT});
        payload["price"] = serde_json::from_str(&price(line)).expect("price number");
        if let Some(customer) = line.customer {
            payload["customer"] = json!(customer);
        }
        let text = payload.to_string();
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
        let reader = cell.store().read();
        let Some(record) = reader.object("main", TYPE, &id).expect("read") else {
            held.insert(row);
            continue;
        };
        for finding in reader
            .observation_findings("main", TYPE, &id)
            .expect("findings")
        {
            findings.insert((row, rule_key(&finding.rule_ref).expect("rule")));
        }
        if reader
            .observation_is_quarantined("main", &record)
            .expect("quarantine")
        {
            held.insert(row);
        }
    }
    (findings, held)
}

/// The set-based path: the same lines landed on the engine, every law
/// checked in one pass, the residue settled by the per-row evaluator.
fn set_based(cell: &Cell, lines: &[Line]) -> (BTreeSet<(u64, String)>, BTreeSet<u64>, u64) {
    let dir = tempfile::tempdir().expect("engine dir");
    let engine = Engine::new(Settings {
        root: dir.path().to_path_buf(),
        memory_limit: "256MB".into(),
        threads: 1,
    })
    .expect("engine");
    let mut file = tempfile::NamedTempFile::new_in(engine.scratch()).expect("file");
    writeln!(file, "{}", FIELDS.join(",")).expect("header");
    for (index, line) in lines.iter().enumerate() {
        writeln!(
            file,
            "line-{},{},c,{},{},{},{},{AT}",
            index + 1,
            line.invoice,
            line.quantity,
            price(line),
            line.customer.unwrap_or_default(),
            line.country
        )
        .expect("row");
    }
    file.flush().expect("flush");
    let sha256 = norn_cell::analytics::hex_digest(&std::fs::read(file.path()).expect("read"));
    let store = cell.store();
    let table = store.analytics_table(TYPE).expect("table");
    let checks = store.dataset_checks(TYPE, &table).expect("checks");
    let request = LandRequest {
        load: LoadRequest {
            tenant: TenantId::of_scope("synthetic"),
            table: table.clone(),
            load: "land-1".into(),
            file: FileSource {
                url: String::new(),
                uri: "s3://synthetic/uploads/lines.csv".into(),
                sha256,
                format: FileFormat::Csv,
            },
            headers: BTreeMap::new(),
        },
        data_url: String::new(),
        held_url: String::new(),
        checks,
        residue_page: 7,
    };
    let mut landed = engine.land(&request, file.path()).expect("landed");
    let mut residue_rows = 0;
    while landed.response.residue_total > 0 {
        let page = &landed.response.residue;
        residue_rows += u64::try_from(page.len()).expect("page");
        let findings = store
            .residue_findings(TYPE, &table, page)
            .expect("per-row residue");
        let through = page.last().expect("a residue page").row;
        landed = engine
            .settle(&SettleRequest {
                land: request.clone(),
                through,
                findings,
            })
            .expect("settled");
    }
    let held_file = landed.held.expect("held file");
    let conn = duckdb::Connection::open_in_memory().expect("reader");
    let mut statement = conn
        .prepare(&format!(
            "SELECT \":row\", \":rule\", \":holds\" FROM read_parquet('{}')",
            held_file.display()
        ))
        .expect("held set");
    let mut findings = BTreeSet::new();
    let mut held = BTreeSet::new();
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, u64>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, bool>(2)?,
            ))
        })
        .expect("rows");
    for row in rows {
        let (offset, rule, holds) = row.expect("row");
        if let Some(rule) = rule {
            findings.insert((offset, rule));
        }
        if holds {
            held.insert(offset);
        }
    }
    assert_eq!(
        landed.response.held_by_rules,
        u64::try_from(
            findings
                .iter()
                .map(|(row, _)| *row)
                .filter(|row| held.contains(row))
                .collect::<BTreeSet<_>>()
                .len()
        )
        .expect("count"),
    );
    (findings, held, residue_rows)
}

fn lines() -> impl proptest::strategy::Strategy<Value = Vec<Line>> {
    let line = (
        proptest::sample::select(vec!["A1", "A2", "C1", "C9"]),
        -3i64..6,
        -2_000i64..5_000,
        proptest::option::of(proptest::sample::select(vec!["ann", "void", "bob"])),
        proptest::sample::select(vec!["GB", "FR", "XX"]),
    )
        .prop_map(|(invoice, quantity, price_milli, customer, country)| Line {
            invoice,
            quantity,
            price_milli,
            customer,
            country,
        });
    proptest::collection::vec(line, 1..24)
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(16))]
    /// MANIFEST-LAWS-1: lowered laws decide every row whose reads are present
    /// on the engine, and hold back the per-row path's rows with its findings.
    #[test]
    fn lowered_laws_hold_back_what_the_per_row_path_holds_back(lines in lines()) {
        let (_dir, mut cell) = cell(&LOWERED);
        let (set_findings, set_held, residue) = set_based(&cell, &lines);
        let (row_findings, row_held) = per_row(&mut cell, &lines);
        proptest::prop_assert_eq!(&set_held, &row_held);
        proptest::prop_assert_eq!(&set_findings, &row_findings);
        // Only rows with an absent read are left to the per-row evaluator.
        let absent = u64::try_from(lines.iter().filter(|line| line.customer.is_none()).count()).expect("count");
        proptest::prop_assert_eq!(residue, absent);
    }

    /// MANIFEST-LAWS-1: a law outside the lowered subset leaves every row to
    /// the per-row evaluator, with the same outcome.
    #[test]
    fn a_law_outside_the_subset_is_decided_per_row(lines in lines()) {
        let mut laws = LOWERED.to_vec();
        laws.push(FUNCTION);
        let (_dir, mut cell) = cell(&laws);
        let (set_findings, set_held, residue) = set_based(&cell, &lines);
        let (row_findings, row_held) = per_row(&mut cell, &lines);
        proptest::prop_assert_eq!(&set_held, &row_held);
        proptest::prop_assert_eq!(&set_findings, &row_findings);
        proptest::prop_assert_eq!(residue, u64::try_from(lines.len()).expect("rows"));
    }
}
