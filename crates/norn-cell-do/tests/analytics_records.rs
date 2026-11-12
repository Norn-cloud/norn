//! D102 / HOST-READ-3: a loaded dataset's records page is the native grid's.
//! Each demo type's rows are read through the cell's own grid, then loaded as
//! that type's dataset, whose grid the engine answers: every filter operator
//! and sort direction gives the same rows, totals and order, and a caller
//! without Values sees every row withheld, as the native grid shows it.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    missing_docs,
    reason = "engine correspondence over the seeded demo cell"
)]

use std::collections::BTreeMap;
use std::sync::Mutex;

use norn_api::analytics_routes::{EngineAnswer, EngineCall, Transport, UploadRecord, Uploads};
use norn_api::host::DevHost;
use norn_cell::analytics::engine::{Engine, Settings};
use norn_cell::analytics::{ColumnKind, LoadRequest, TableSchema, hex_digest};
use norn_cell::engine::NativeEngine;
use norn_cell_do::olap::InProcess;
use norn_cell_do::policy::{DEMO_GRANTS, ReadPolicy};
use norn_cell_do::principal::{DEMO_FIXTURES, FixturePrincipals};
use norn_cell_do::{seed, server};
use serde_json::{Value, json};

const ADMIN: Option<&str> = Some("Bearer fixture:viewer@demo");
const VIEWER: Option<&str> = Some("Bearer fixture:guest@demo");
const SCOPE: &str = "orgId=org_demo&environmentId=dev";

/// The in-process engine, plus loads of files this test wrote (`file://`).
struct Loading(InProcess, Mutex<BTreeMap<String, Vec<u8>>>);

impl Transport for Loading {
    fn resumed(&self) -> Option<EngineAnswer> {
        None
    }
    fn call(&self, call: &EngineCall) -> Option<EngineAnswer> {
        if call.op != "load" {
            return self.0.call(call);
        }
        let request: LoadRequest = serde_json::from_str(&call.body).expect("a load");
        let bytes = self.1.lock().unwrap()[&request.file.url].clone();
        let path = self.0.0.scratch().join(format!("load-{}", request.load));
        std::fs::write(&path, bytes).expect("scratch file");
        let loaded = self.0.0.load(&request, &path);
        std::fs::remove_file(&path).expect("scratch removed");
        let (status, body) = match loaded {
            Ok(loaded) => (200, serde_json::to_string(&loaded).unwrap()),
            Err(refused) => (422, serde_json::to_string(&refused).unwrap()),
        };
        Some(EngineAnswer {
            op: call.op.clone(),
            digest: call.digest.clone(),
            status,
            body,
        })
    }
}

/// Every upload `id` is the CSV the test wrote for it.
struct Files<'a>(&'a Loading);

impl Uploads for Files<'_> {
    fn record(&self, id: &str) -> Option<UploadRecord> {
        let files = self.0.1.lock().unwrap();
        let bytes = files.get(&format!("file://{id}"))?;
        Some(UploadRecord {
            uri: format!("s3://uploads/demo/{id}.csv"),
            sha256: hex_digest(bytes),
            content_type: "text/csv".into(),
        })
    }
    fn presigned_get(&self, uri: &str, _seconds: u64, _now_ms: u64) -> Option<String> {
        let id = uri
            .strip_prefix("s3://uploads/demo/")?
            .strip_suffix(".csv")?;
        Some(format!("file://{id}"))
    }
}

fn get(
    host: &DevHost<NativeEngine>,
    principals: &FixturePrincipals,
    transport: &Loading,
    who: Option<&str>,
    path: &str,
    query: &str,
) -> (server::Response, bool) {
    let request = server::Request {
        method: "GET",
        path,
        query,
        authorization: who,
        now: 0,
        body: "",
    };
    match server::analytics(host, principals, transport, None, &request) {
        Some(answered) => (answered, true),
        None => (server::handle(host, principals, &request), false),
    }
}

fn encode(text: &str) -> String {
    percent_encoding::utf8_percent_encode(text, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// A grid query string for `filter` and `sort`.
fn query(filter: Option<&Value>, sort: Option<&str>) -> String {
    let mut query = format!("{SCOPE}&limit=200");
    if let Some(filter) = filter {
        query.push_str(&format!("&filter={}", encode(&filter.to_string())));
    }
    if let Some(sort) = sort {
        query.push_str(&format!("&sort={}", encode(sort)));
    }
    query
}

/// The page's rows as their values (identities differ between the two
/// sources), its total, and the withheld flags.
fn rows(response: &server::Response) -> (Vec<Value>, Value, Vec<bool>) {
    assert_eq!(response.status, 200, "{}", response.body);
    let page: Value = serde_json::from_str(&response.body).unwrap();
    let records = page["records"].as_array().unwrap();
    assert!(page["next"].is_null(), "one page holds the demo rows");
    (
        records.iter().map(|r| canonical(&r["values"])).collect(),
        page["total"]["count"].clone(),
        records
            .iter()
            .map(|r| r["withheld"].as_bool().unwrap())
            .collect(),
    )
}

/// Values with numbers as exact decimals, so `1.50` and `1.5` agree.
fn canonical(values: &Value) -> Value {
    match values {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k.clone(), canonical(v)))
                .collect(),
        ),
        Value::Number(n) => Value::String(
            norn_ir::Decimal::parse(&n.to_string())
                .map_or_else(|_| n.to_string(), |d| d.canonical()),
        ),
        other => other.clone(),
    }
}

/// The CSV of `records` over `table`'s columns, or `None` when a value
/// does not survive a CSV as itself (blank text reads as absent).
fn csv(table: &TableSchema, records: &[Value]) -> Option<String> {
    let mut out = table
        .columns
        .iter()
        .map(|c| c.id.clone())
        .collect::<Vec<_>>()
        .join(",");
    out.push('\n');
    for values in records {
        let fields = table
            .columns
            .iter()
            .map(|column| match &values[&column.id] {
                Value::Null => Some(String::new()),
                Value::String(text) if text.trim().is_empty() => None,
                Value::String(text) => Some(format!("\"{}\"", text.replace('"', "\"\""))),
                Value::Number(n) => Some(n.to_string()),
                Value::Bool(flag) => Some(flag.to_string()),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()?;
        out.push_str(&fields.join(","));
        out.push('\n');
    }
    Some(out)
}

/// The clauses one present value of `column` exercises, by kind.
fn clauses(id: &str, kind: ColumnKind, value: &Value) -> Vec<Value> {
    let clause =
        |op: &str, value: Value| json!({"all": [{"property": id, "op": op, "value": value}]});
    let mut out = vec![
        json!({"all": [{"property": id, "op": "empty"}]}),
        json!({"all": [{"property": id, "op": "present"}]}),
    ];
    match kind {
        ColumnKind::Text => {
            let text = value.as_str().unwrap_or_default();
            let head: String = text.chars().take(2).collect();
            let middle: String = text.chars().skip(1).take(2).collect();
            out.extend([
                clause("eq", value.clone()),
                clause("ne", value.clone()),
                clause("in", json!([value, "no such value"])),
                clause("prefix", json!(head)),
                clause("contains", json!(middle)),
            ]);
        }
        ColumnKind::Integer | ColumnKind::Decimal { .. } => out.extend([
            clause("eq", value.clone()),
            clause("ne", value.clone()),
            clause("lt", value.clone()),
            clause("lte", value.clone()),
            clause("gt", value.clone()),
            clause("gte", value.clone()),
            clause("between", json!([value, value])),
            clause("in", json!([value])),
        ]),
        ColumnKind::Timestamp | ColumnKind::Date => out.extend([
            clause("eq", value.clone()),
            clause("ne", value.clone()),
            clause("lt", value.clone()),
            clause("gte", value.clone()),
            clause("between", json!([value, value])),
        ]),
        ColumnKind::Boolean => out.push(clause("eq", value.clone())),
        ColumnKind::Opaque => {}
    }
    out
}

#[test]
fn a_loaded_datasets_grid_equals_the_native_grid_of_the_same_rows() {
    let scope = seed::demo_scope();
    let store = seed::provision(NativeEngine::open_in_memory().unwrap(), &scope).expect("demo");
    let policy = ReadPolicy::compile(store.model().object_types(), &scope, &DEMO_GRANTS).unwrap();
    let principals = FixturePrincipals::new(DEMO_FIXTURES, policy.caller());
    let tables: BTreeMap<String, TableSchema> = store
        .analytics_tables()
        .unwrap()
        .into_iter()
        .map(|table| (table.name.clone(), table))
        .collect();
    let host = DevHost::from_store(store, scope, |command| command.payload.clone())
        .unwrap()
        .with_authorizer(policy);
    let dir = tempfile::tempdir().unwrap();
    let transport = Loading(
        InProcess(
            Engine::new(Settings {
                root: dir.path().to_path_buf(),
                memory_limit: "256MB".into(),
                threads: 1,
            })
            .unwrap(),
        ),
        Mutex::default(),
    );
    let mut compared = 0;
    for (name, table) in &tables {
        let path = format!("/v1/objects/{}", encode(name));
        let (native, routed) = get(
            &host,
            &principals,
            &transport,
            ADMIN,
            &path,
            &query(None, None),
        );
        assert!(
            !routed,
            "{name}: no dataset yet, so the cell's grid answers"
        );
        let (records, _, _) = rows(&native);
        let page: Value = serde_json::from_str(&native.body).unwrap();
        let raw: Vec<Value> = page["records"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["values"].clone())
            .collect();
        let plain_kinds = table.columns.iter().all(|c| c.kind != ColumnKind::Opaque);
        let Some(file) = csv(table, &raw).filter(|_| plain_kinds && records.len() > 1) else {
            continue;
        };
        // Every query's native answer, before the dataset exists.
        let mut queries: Vec<(Option<Value>, Option<String>)> = vec![(None, None)];
        for column in &table.columns {
            queries.push((None, Some(format!("{}:asc", column.id))));
            queries.push((None, Some(format!("{}:desc", column.id))));
            if let Some(value) = raw.iter().map(|r| &r[&column.id]).find(|v| !v.is_null()) {
                queries.extend(
                    clauses(&column.id, column.kind, value)
                        .into_iter()
                        .map(|f| (Some(f), None)),
                );
            }
        }
        let before: Vec<_> = queries
            .iter()
            .map(|(filter, sort)| {
                let (response, _) = get(
                    &host,
                    &principals,
                    &transport,
                    ADMIN,
                    &path,
                    &query(filter.as_ref(), sort.as_deref()),
                );
                response
            })
            .collect();
        let viewer_before = get(
            &host,
            &principals,
            &transport,
            VIEWER,
            &path,
            &query(None, None),
        )
        .0;
        let id = name.replace('.', "_");
        transport
            .1
            .lock()
            .unwrap()
            .insert(format!("file://{id}"), file.into_bytes());
        let load = server::analytics(
            &host,
            &principals,
            &transport,
            Some(&Files(&transport) as &dyn Uploads),
            &server::Request {
                method: "POST",
                path: "/v1/datasets:load",
                query: SCOPE,
                authorization: ADMIN,
                now: 0,
                body: &json!({"uploadId": id, "objectType": name}).to_string(),
            },
        )
        .expect("the load route");
        let loaded: Value = serde_json::from_str(&load.body).unwrap();
        if load.status != 200 || loaded["rejected"] != 0 {
            // A type whose laws the engine does not evaluate, or whose rows a
            // CSV cannot carry as themselves, is not a correspondence case.
            continue;
        }
        for ((filter, sort), native) in queries.iter().zip(&before) {
            let (engine, routed) = get(
                &host,
                &principals,
                &transport,
                ADMIN,
                &path,
                &query(filter.as_ref(), sort.as_deref()),
            );
            assert!(routed, "{name}: the dataset's grid is the engine's");
            let (want, want_total, _) = rows(native);
            let (got, got_total, _) = rows(&engine);
            assert_eq!(got_total, want_total, "{name} {filter:?} {sort:?}");
            match sort {
                // Ties order by identity, which differs: compare the sorted column.
                Some(sort) => {
                    let column = sort.split(':').next().unwrap();
                    let project =
                        |rows: &[Value]| rows.iter().map(|r| r[column].clone()).collect::<Vec<_>>();
                    assert_eq!(project(&got), project(&want), "{name} {sort}");
                }
                None => {
                    let sorted = |mut rows: Vec<Value>| {
                        rows.sort_by_key(Value::to_string);
                        rows
                    };
                    assert_eq!(sorted(got), sorted(want), "{name} {filter:?}");
                }
            }
        }
        let (_, viewer_total, viewer_withheld) = rows(&viewer_before);
        let (_, engine_total, engine_withheld) = rows(
            &get(
                &host,
                &principals,
                &transport,
                VIEWER,
                &path,
                &query(None, None),
            )
            .0,
        );
        assert_eq!(
            (engine_total, engine_withheld),
            (viewer_total, viewer_withheld),
            "{name}: withheld as natively"
        );
        compared += 1;
    }
    assert!(compared >= 2, "at least two demo types correspond");
}
