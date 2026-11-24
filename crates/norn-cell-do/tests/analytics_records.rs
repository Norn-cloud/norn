//! D102 / HOST-READ-3: a loaded dataset's records page is the native grid's.
//! Upload types are defined and loaded through observations (their admitted
//! mappings, MANIFEST-03); each type's rows are read through the cell's own
//! grid, then loaded as that type's dataset, whose grid the engine answers:
//! every filter operator and sort direction gives the same rows, totals and
//! order, and a caller without Values sees every row withheld, as the
//! native grid shows it. A demo type no plain mapping binds is refused.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    missing_docs,
    reason = "engine correspondence over the seeded demo cell"
)]

use std::collections::BTreeMap;
use std::sync::Mutex;

use norn_api::analytics_routes::{EngineAnswer, EngineCall, Transport, UploadRecord, Uploads};
use norn_api::host::DevHost;
use norn_cell::analytics::engine::{Engine, Settings};
use norn_cell::analytics::{
    Column, ColumnKind, LandRequest, PointLaw, SettleRequest, StoreRequest, TableSchema, hex_digest,
};
use norn_cell::engine::NativeEngine;
use norn_cell_do::olap::InProcess;
use norn_cell_do::policy::{DEMO_GRANTS, ReadPolicy};
use norn_cell_do::principal::{DEMO_FIXTURES, FixturePrincipals};
use norn_cell_do::{seed, server};
use serde_json::{Value, json};

const ADMIN: Option<&str> = Some("Bearer fixture:admin@demo");
const VIEWER: Option<&str> = Some("Bearer fixture:guest@demo");
const SCOPE: &str = "orgId=org_demo&environmentId=dev";

/// The in-process engine, plus loads of files this test wrote (`file://`).
struct Loading(InProcess, Mutex<BTreeMap<String, Vec<u8>>>);

impl Transport for Loading {
    fn capabilities(&self) -> norn_cell::analytics::EngineCapabilities {
        self.0.capabilities()
    }
    fn resumed(&self) -> Option<EngineAnswer> {
        None
    }
    fn call(&self, call: &EngineCall) -> Option<EngineAnswer> {
        let engine = &self.0.0;
        let landed = match call.op.as_str() {
            "land" => {
                // MANIFEST-02: the download goes into the attempt's own file.
                let request: LandRequest = serde_json::from_str(&call.body).expect("a landing");
                let bytes = self.1.lock().unwrap()[&request.load.file.url].clone();
                engine.land(&request, |mut file| {
                    use std::io::Write as _;
                    file.write_all(&bytes).expect("source file");
                    Ok(())
                })
            }
            "settle" => {
                let request: SettleRequest = serde_json::from_str(&call.body).expect("a settle");
                engine.settle(&request)
            }
            "store" => {
                // MANIFEST-LAND-1: the landed files go to their PUT URLs.
                let request: StoreRequest = serde_json::from_str(&call.body).expect("a store");
                engine.store(&request, |data, held| {
                    let mut files = self.1.lock().unwrap();
                    for (url, path) in [(&request.data.url, data), (&request.held.url, held)] {
                        files.insert(url.clone(), std::fs::read(path).expect("landed file"));
                    }
                    Ok(())
                })
            }
            _ => return self.0.call(call),
        };
        let (status, body) = match landed {
            Ok(landed) => (200, serde_json::to_string(&landed).unwrap()),
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
            file_name: format!("{id}.csv"),
        })
    }
    fn issued(&self, id: &str, _scope_key: &str, uri: &str) -> bool {
        self.record(id).is_some_and(|record| record.uri == uri)
    }
    fn presigned_get(&self, uri: &str, _seconds: u64, _now_ms: u64) -> Option<String> {
        let id = uri
            .strip_prefix("s3://uploads/demo/")?
            .strip_suffix(".csv")?;
        Some(format!("file://{id}"))
    }
    fn presigned_put(&self, key: &str, _seconds: u64, _now_ms: u64) -> Option<(String, String)> {
        Some((format!("put://{key}"), format!("s3://uploads/{key}")))
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
        query.push_str("&filter=");
        query.push_str(&encode(&filter.to_string()));
    }
    if let Some(sort) = sort {
        query.push_str("&sort=");
        query.push_str(&encode(sort));
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
        Value::Null | Value::Bool(_) | Value::String(_) | Value::Array(_) => values.clone(),
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
                Value::Array(_) | Value::Object(_) => None,
            })
            .collect::<Option<Vec<_>>>()?;
        out.push_str(&fields.join(","));
        out.push('\n');
    }
    Some(out)
}

/// The clauses one present value of `column` exercises, by kind (an enum
/// takes `eq`, `ne` and `in` only).
fn clauses(column: &Column, value: &Value) -> Vec<Value> {
    let id = column.id.as_str();
    // A query value is a JSON string (a boolean excepted): a number the grid
    // answers is asked back by its text.
    let text;
    let value = if let Value::Number(number) = value {
        text = Value::String(number.to_string());
        &text
    } else {
        value
    };
    let enumerated = column
        .laws
        .iter()
        .any(|law| matches!(law, PointLaw::OneOf(_)));
    let clause =
        |op: &str, value: Value| json!({"all": [{"property": id, "op": op, "value": value}]});
    let mut out = vec![
        json!({"all": [{"property": id, "op": "empty"}]}),
        json!({"all": [{"property": id, "op": "present"}]}),
    ];
    match column.kind {
        ColumnKind::Text if enumerated => out.extend([
            clause("eq", value.clone()),
            clause("ne", value.clone()),
            clause("in", json!([value, value])),
        ]),
        ColumnKind::Text => {
            let text = value.as_str().unwrap_or_default();
            let head: String = text.chars().take(2).collect();
            let middle: String = text.chars().skip(1).take(2).collect();
            out.extend([
                clause("eq", value.clone()),
                clause("ne", value.clone()),
                clause("in", json!([value, value])),
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
    let host = DevHost::from_store(store, scope, |command| command.payload.clone())
        .unwrap()
        .with_authorizer(policy);
    // MANIFEST-03: types an admitted mapping binds, loaded as observations.
    let uploads = [
        (
            "Orders",
            "order,region,units,price,paid,placed\nO1,north,3,1.50,true,2026-10-21T08:00:00Z\nO2,south,5,2.25,false,2026-10-22T09:30:00Z\nO3,north,1,10.00,true,2026-10-23T12:00:00Z\n",
        ),
        ("Regions", "code,label\nN,North\nS,South\nE,East\n"),
    ];
    // After the demo rows (`DEMO_ROWS_AT`), each install and load at its own
    // later time: the cell's clock never runs back.
    let times = (norn_cell::provision::DEMO_ROWS_AT + 1..).step_by(2);
    for (at, (name, text)) in times.zip(uploads) {
        let table = norn_api::uploads::parse_csv(text).unwrap();
        let request: norn_api::uploads::LoadRequest =
            serde_json::from_value(json!({"typeName": name})).unwrap();
        let uri = format!("s3://uploads/demo/{name}.csv");
        let sha256 = hex_digest(text.as_bytes());
        let generated =
            norn_api::uploads::generate(&table, &request, &uri, &sha256, "2026-11-12T12:00:00Z")
                .unwrap();
        norn_api::uploads::install(&host, &generated, at).unwrap();
        norn_api::uploads::load_rows(&host, &generated, &table, &uri, &sha256, at + 1).unwrap();
    }
    let tables: BTreeMap<String, TableSchema> = host
        .analytics_tables()
        .unwrap()
        .into_iter()
        .map(|table| (table.name.clone(), table))
        .collect();
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
                queries.extend(clauses(column, value).into_iter().map(|f| (Some(f), None)));
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
        let files = Files(&transport);
        let uploads: &dyn Uploads = &files;
        let load = server::analytics(
            &host,
            &principals,
            &transport,
            Some(uploads),
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
            // A type no plain mapping binds (MANIFEST-03), whose laws the
            // engine does not evaluate, or whose rows a CSV cannot carry as
            // themselves, is not a correspondence case.
            assert!(
                !matches!(name.as_str(), "upload.Orders" | "upload.Regions"),
                "{name}: {}",
                load.body
            );
            continue;
        }
        // MANIFEST-LINEAGE-1: a dataset row traces to the manifest that
        // admitted it and to its source and data files, by content hash and
        // row offset; a row past the source is absent.
        let lineage_path = format!("/v1/datasets/{name}/rows/1:lineage");
        let (traced, routed) = get(&host, &principals, &transport, ADMIN, &lineage_path, SCOPE);
        assert!(routed, "{name}: a dataset row's lineage is the engine's");
        assert_eq!(traced.status, 200, "{name}: {}", traced.body);
        let traced: Value = serde_json::from_str(&traced.body).unwrap();
        assert_eq!(traced["manifest"], loaded["manifest"], "{name}");
        assert_eq!(traced["commitSeq"], loaded["commitSeq"], "{name}");
        assert_eq!(traced["source"]["sha256"], loaded["sha256"], "{name}");
        assert_eq!(traced["data"]["sha256"], loaded["data"]["sha256"], "{name}");
        assert_eq!(
            (&traced["data"]["row"], &traced["held"]),
            (&json!(1), &json!(false)),
            "{name}"
        );
        let past = format!("/v1/datasets/{name}/rows/{}:lineage", raw.len() + 1);
        assert_eq!(
            get(&host, &principals, &transport, ADMIN, &past, SCOPE)
                .0
                .status,
            404,
            "{name}"
        );
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
            // Ties order by identity, which differs: compare the sorted column.
            if let Some(sort) = sort {
                let column = sort.split(':').next().unwrap();
                let project =
                    |rows: &[Value]| rows.iter().map(|r| r[column].clone()).collect::<Vec<_>>();
                assert_eq!(project(&got), project(&want), "{name} {sort}");
            } else {
                let sorted = |mut rows: Vec<Value>| {
                    rows.sort_by_key(Value::to_string);
                    rows
                };
                assert_eq!(sorted(got), sorted(want), "{name} {filter:?}");
            }
        }
        // A row of the grid reads alone by its identity; an absent one is 404.
        let listed: Value = serde_json::from_str(
            &get(
                &host,
                &principals,
                &transport,
                ADMIN,
                &path,
                &query(None, None),
            )
            .0
            .body,
        )
        .unwrap();
        let first = &listed["records"][0];
        let (one, routed) = get(
            &host,
            &principals,
            &transport,
            ADMIN,
            &format!("{path}/{}", encode(first["id"].as_str().unwrap())),
            SCOPE,
        );
        assert!(routed, "{name}: a dataset row is read from the engine");
        assert_eq!(one.status, 200, "{name}: {}", one.body);
        let alone: Value = serde_json::from_str(&one.body).unwrap();
        assert_eq!(alone["values"], first["values"], "{name}");
        let (absent, _) = get(
            &host,
            &principals,
            &transport,
            ADMIN,
            &format!("{path}/0"),
            SCOPE,
        );
        assert_eq!(absent.status, 404, "{name}: {}", absent.body);
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
    assert!(compared >= 2, "at least the two upload types correspond");
}

/// B4 / MANIFEST-LINEAGE-1: `GET /v1/datasets/{type}/rows/{row}:lineage`
/// requires Population, Values and Sources (matching `/objects/{type}/{id}/lineage`).
/// Both a Population-only caller (`viewer`) and a Population+Values caller
/// without Sources (`member`) receive the exact same opaque 403 before row
/// parsing or manifest reads, leaking nothing about row existence, held status,
/// refusal codes, findings or object-store URIs.
#[test]
fn dataset_lineage_requires_values_and_sources_capabilities() {
    const LINEAGE_FIXTURES: &[(&str, &[&str])] = &[
        ("admin@demo", &["admin"]),
        ("member@demo", &["member"]),
        ("guest@demo", &["viewer"]),
    ];
    const MEMBER: Option<&str> = Some("Bearer fixture:member@demo");

    let scope = seed::demo_scope();
    let store = seed::provision(NativeEngine::open_in_memory().unwrap(), &scope).expect("demo");
    let policy = ReadPolicy::compile(store.model().object_types(), &scope, &DEMO_GRANTS).unwrap();
    let principals = FixturePrincipals::new(LINEAGE_FIXTURES, policy.caller());
    let host = DevHost::from_store(store, scope, |command| command.payload.clone())
        .unwrap()
        .with_authorizer(policy);

    let schema_csv = "code,units\nA1,10\nA2,20\n";
    let table = norn_api::uploads::parse_csv(schema_csv).unwrap();
    let request: norn_api::uploads::LoadRequest =
        serde_json::from_value(json!({"typeName": "LineageItems"})).unwrap();
    let uri = "s3://uploads/demo/LineageItems.csv";
    let sha256 = hex_digest(schema_csv.as_bytes());
    let generated =
        norn_api::uploads::generate(&table, &request, uri, &sha256, "2026-11-12T12:00:00Z")
            .unwrap();
    norn_api::uploads::install(&host, &generated, norn_cell::provision::DEMO_ROWS_AT + 1).unwrap();

    // Row 1 is valid; row 2 fails integer typing so it is held back.
    let data_csv = "row_id,code,units\nr1,A1,10\nr2,A2,not-an-int\n";
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
    transport
        .1
        .lock()
        .unwrap()
        .insert("file://lineage_items".into(), data_csv.as_bytes().to_vec());
    let files = Files(&transport);
    let uploads: &dyn Uploads = &files;
    let load = server::analytics(
        &host,
        &principals,
        &transport,
        Some(uploads),
        &server::Request {
            method: "POST",
            path: "/v1/datasets:load",
            query: SCOPE,
            authorization: ADMIN,
            now: 0,
            body: &json!({"uploadId": "lineage_items", "objectType": "upload.LineageItems"})
                .to_string(),
        },
    )
    .expect("load route");
    assert_eq!(load.status, 200, "{}", load.body);
    let loaded: Value = serde_json::from_str(&load.body).unwrap();
    assert_eq!(loaded["rows"], 1);
    assert_eq!(loaded["rejected"], 1);

    let probes = ["1", "2", "999", "not-a-row"];
    for caller in [VIEWER, MEMBER] {
        let mut bodies = Vec::new();
        for row in probes {
            let path = format!("/v1/datasets/upload.LineageItems/rows/{row}:lineage");
            let (resp, routed) = get(&host, &principals, &transport, caller, &path, SCOPE);
            assert!(routed, "lineage route for {caller:?} row={row}");
            assert_eq!(resp.status, 403, "{caller:?} row={row}: {}", resp.body);
            let doc: Value = serde_json::from_str(&resp.body).unwrap();
            assert_eq!(doc["code"], "authz.denied", "{caller:?} row={row}: {doc}");
            assert!(
                !resp.body.contains("s3://"),
                "must not leak URIs: {}",
                resp.body
            );
            assert!(
                !resp.body.contains("bulk."),
                "must not leak refusal codes: {}",
                resp.body
            );
            bodies.push(resp.body);
        }
        // Every probe gets the exact same opaque 403 regardless of row existence or syntax.
        assert!(
            bodies.windows(2).all(|w| w[0] == w[1]),
            "{caller:?} got distinguishable 403 bodies: {bodies:?}"
        );
    }

    // Admin (Population + Values + Sources) traces both admitted and held rows.
    let (admitted, routed) = get(
        &host,
        &principals,
        &transport,
        ADMIN,
        "/v1/datasets/upload.LineageItems/rows/1:lineage",
        SCOPE,
    );
    assert!(routed);
    assert_eq!(admitted.status, 200, "{}", admitted.body);
    let admitted: Value = serde_json::from_str(&admitted.body).unwrap();
    assert_eq!(admitted["held"], false);
    assert_eq!(admitted["manifest"], loaded["manifest"]);
    assert_eq!(admitted["data"]["row"], 1);
    assert_eq!(
        admitted["source"]["uri"],
        "s3://uploads/demo/lineage_items.csv"
    );

    let (held, routed) = get(
        &host,
        &principals,
        &transport,
        ADMIN,
        "/v1/datasets/upload.LineageItems/rows/2:lineage",
        SCOPE,
    );
    assert!(routed);
    assert_eq!(held.status, 200, "{}", held.body);
    let held: Value = serde_json::from_str(&held.body).unwrap();
    assert_eq!(held["held"], true);
    assert!(held["data"].is_null());
    assert!(held["refused"].as_str().is_some(), "{held}");

    for missing in ["999", "not-a-row"] {
        let (resp, _) = get(
            &host,
            &principals,
            &transport,
            ADMIN,
            &format!("/v1/datasets/upload.LineageItems/rows/{missing}:lineage"),
            SCOPE,
        );
        assert_eq!(resp.status, 404, "{missing}: {}", resp.body);
    }
}

/// B2 + B3 / MANIFEST-MARK-1: adding a marking to a column in a definitions
/// update after a dataset was already loaded (with `manifest.markings == {}`)
/// immediately governs every dataset read path:
/// 1. `page_objects` and `record_object` mask the column (and refuse filter/sort),
/// 2. `profile_objects` refuses with `privacy.marked_column` before touching `DuckDB`,
/// 3. questions over the dataset refuse with `privacy.marked_column`,
/// 4. `export_objects` refuses.
struct PanicTransport;

impl Transport for PanicTransport {
    fn capabilities(&self) -> norn_cell::analytics::EngineCapabilities {
        norn_cell::analytics::EngineCapabilities::duckdb_default()
    }
    fn resumed(&self) -> Option<EngineAnswer> {
        None
    }
    fn call(&self, call: &EngineCall) -> Option<EngineAnswer> {
        panic!(
            "profile must not reach DuckDB for a marked dataset: {}",
            call.op
        );
    }
}

#[test]
fn dataset_markings_enforce_post_load_additions_and_profile_refusal() {
    use norn_api::analytics_routes::EngineQuestions;
    use norn_api::question_routes::AdHocQuestions;
    use norn_cell_do::questions::DerivedQuestions;
    use norn_ir::ArtifactEnvelope;

    let scope = seed::demo_scope();
    let store = seed::provision(NativeEngine::open_in_memory().unwrap(), &scope).expect("demo");
    let policy = ReadPolicy::compile(store.model().object_types(), &scope, &DEMO_GRANTS).unwrap();
    let principals = FixturePrincipals::new(DEMO_FIXTURES, policy.caller());
    let host = DevHost::from_store(store, scope.clone(), |command| command.payload.clone())
        .unwrap()
        .with_authorizer(policy)
        .with_pseudonym_secret(Some(&[7_u8; 32]));

    let csv_text = "contact_id,email,score,recorded_at\nC1,alice@example.com,10,2026-10-21T08:00:00Z\nC2,bob@example.com,90,2026-10-22T09:00:00Z\n";
    let table = norn_api::uploads::parse_csv(csv_text).unwrap();
    let load_req: norn_api::uploads::LoadRequest = serde_json::from_value(json!({
        "typeName": "Contacts",
        "columns": [{"id": "score", "type": "integer", "measure": true}]
    }))
    .unwrap();
    let uri = "s3://uploads/demo/contacts.csv";
    let sha256 = hex_digest(csv_text.as_bytes());
    let generated =
        norn_api::uploads::generate(&table, &load_req, uri, &sha256, "2026-11-12T12:00:00Z")
            .unwrap();
    norn_api::uploads::install(&host, &generated, norn_cell::provision::DEMO_ROWS_AT + 1).unwrap();

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
    let data_csv = "row_id,contact_id,email,score,recorded_at\nr1,C1,alice@example.com,10,2026-10-21T08:00:00Z\nr2,C2,bob@example.com,90,2026-10-22T09:00:00Z\n";
    transport
        .1
        .lock()
        .unwrap()
        .insert("file://contacts".into(), data_csv.as_bytes().to_vec());
    let files = Files(&transport);
    let uploads: &dyn Uploads = &files;
    let load = server::analytics(
        &host,
        &principals,
        &transport,
        Some(uploads),
        &server::Request {
            method: "POST",
            path: "/v1/datasets:load",
            query: SCOPE,
            authorization: ADMIN,
            now: 0,
            body: &json!({"uploadId": "contacts", "objectType": "upload.Contacts"}).to_string(),
        },
    )
    .expect("load route");
    assert_eq!(load.status, 200, "{}", load.body);

    // Confirm the committed v1 manifest has empty markings.
    let v1_manifest = norn_api::analytics_routes::current_manifest(&host, "upload.Contacts")
        .unwrap()
        .expect("committed manifest");
    assert!(v1_manifest.manifest.markings.is_empty());

    // Positive controls at v1 (unmarked): raw email readable, profile runs on DuckDB,
    // question succeeds.
    let (page_v1, routed) = get(
        &host,
        &principals,
        &transport,
        ADMIN,
        "/v1/objects/upload.Contacts",
        &query(None, None),
    );
    assert!(routed && page_v1.status == 200, "{}", page_v1.body);
    assert!(page_v1.body.contains("alice@example.com"));

    let (profile_v1, routed) = get(
        &host,
        &principals,
        &transport,
        ADMIN,
        "/v1/objects/upload.Contacts:profile",
        SCOPE,
    );
    assert!(routed && profile_v1.status == 200, "{}", profile_v1.body);
    let profile_v1_doc: Value = serde_json::from_str(&profile_v1.body).unwrap();
    assert_eq!(profile_v1_doc["engine"], "duckdb");
    assert_eq!(profile_v1_doc["rows"], 2);

    let dq_v1 = DerivedQuestions::new([7; 32], scope.clone());
    let derived_v1 = dq_v1.questions(&host).expect("derived metrics at v1");
    let engine_q_v1 = EngineQuestions::new(&derived_v1, &transport, host.scope());
    let q_v1: &dyn AdHocQuestions<NativeEngine> = &engine_q_v1;
    let listed = server::handle_with(
        &host,
        &principals,
        Some(q_v1),
        &server::Request {
            method: "GET",
            path: "/v1/questions:metrics",
            query: SCOPE,
            authorization: ADMIN,
            now: 0,
            body: "",
        },
    );
    assert_eq!(listed.status, 200, "{}", listed.body);
    let listed_doc: Value = serde_json::from_str(&listed.body).unwrap();
    let score_metric = listed_doc["metrics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["objectType"] == "upload.Contacts" && m["expression"] == "score")
        .expect("derived score metric")["name"]
        .as_str()
        .unwrap()
        .to_owned();
    let question_body = json!({
        "question": {
            "irVersion": "0.1",
            "name": "test.contacts_score",
            "objectSet": "upload.Contacts",
            "aggregate": [{"metric": score_metric}],
            "groupBy": [],
            "window": {"start": "2026-10-01T00:00:00Z", "end": "2026-11-01T00:00:00Z"},
            "freshness": {"kind": "live", "maxAgeMs": 60000},
        }
    })
    .to_string();
    let answer_v1 = server::handle_with(
        &host,
        &principals,
        Some(q_v1),
        &server::Request {
            method: "POST",
            path: "/v1/questions:evaluate",
            query: SCOPE,
            authorization: ADMIN,
            now: 0,
            body: &question_body,
        },
    );
    assert_eq!(answer_v1.status, 200, "{}", answer_v1.body);

    // Definitions v2: add `pii.contact` marking catalog and mark `upload.Contacts.email`.
    let markings_artifact: ArtifactEnvelope = serde_json::from_value(json!({
        "artifactId": "privacy.markings.entity",
        "dependencies": [],
        "irVersion": "0.1",
        "kind": "entity",
        "lifecycle": "active",
        "name": "Personal data markings",
        "namespace": "privacy",
        "payload": {
            "entityVersion": "2.0",
            "markings": [{
                "category": "pii.contact",
                "id": "pii.contact",
                "label": "Contact",
                "mask": {"kind": "pseudonym", "subject": "Contact"},
                "severity": "high"
            }]
        },
        "provenance": {"source": "system", "sourcePath": "test/privacy.markings.entity.json"},
        "scope": {},
        "version": "0.1.0"
    }))
    .unwrap();
    let mut contacts_entity_json: Value = serde_json::to_value(
        generated
            .artifacts
            .iter()
            .find(|a| a.artifact_id == "upload.Contacts.entity")
            .expect("upload.Contacts entity"),
    )
    .unwrap();
    for prop in contacts_entity_json["payload"]["objectType"]["properties"]
        .as_array_mut()
        .unwrap()
    {
        if prop["id"] == "email" {
            prop["markings"] = json!(["pii.contact"]);
        }
    }
    let marked_contacts_entity: ArtifactEnvelope =
        serde_json::from_value(contacts_entity_json).unwrap();
    host.install_artifacts(
        &[markings_artifact, marked_contacts_entity],
        norn_cell::provision::DEMO_ROWS_AT + 10,
    )
    .expect("install v2 adding marking to email");

    // 1. `page_objects` and `record_object` mask `email`, leave `score` clear,
    //    and refuse filter/sort on `email`.
    let (page_v2, routed) = get(
        &host,
        &principals,
        &transport,
        ADMIN,
        "/v1/objects/upload.Contacts",
        &query(None, None),
    );
    assert!(routed && page_v2.status == 200, "{}", page_v2.body);
    assert!(
        !page_v2.body.contains("alice@example.com") && !page_v2.body.contains("bob@example.com"),
        "post-load marking must mask email on page_objects: {}",
        page_v2.body
    );
    let page_v2_doc: Value = serde_json::from_str(&page_v2.body).unwrap();
    let first = &page_v2_doc["records"][0];
    assert_eq!(first["masked"]["email"]["mode"], "pseudonym", "{first}");
    assert_eq!(
        first["masked"]["email"]["markings"],
        json!(["pii.contact"]),
        "{first}"
    );
    assert!(first["masked"].get("score").is_none(), "{first}");

    let first_id = first["id"].as_str().unwrap();
    let (one_v2, routed) = get(
        &host,
        &principals,
        &transport,
        ADMIN,
        &format!("/v1/objects/upload.Contacts/{}", encode(first_id)),
        SCOPE,
    );
    assert!(routed && one_v2.status == 200, "{}", one_v2.body);
    assert!(
        !one_v2.body.contains("alice@example.com") && !one_v2.body.contains("bob@example.com"),
        "post-load marking must mask email on record_object: {}",
        one_v2.body
    );
    let one_v2_doc: Value = serde_json::from_str(&one_v2.body).unwrap();
    assert_eq!(one_v2_doc["values"], first["values"]);
    assert_eq!(one_v2_doc["masked"]["email"]["mode"], "pseudonym");

    let filter_email =
        json!({"all": [{"property": "email", "op": "eq", "value": "alice@example.com"}]});
    let (filtered, routed) = get(
        &host,
        &principals,
        &transport,
        ADMIN,
        "/v1/objects/upload.Contacts",
        &query(Some(&filter_email), None),
    );
    assert!(routed && filtered.status == 403, "{}", filtered.body);
    assert_eq!(
        serde_json::from_str::<Value>(&filtered.body).unwrap()["code"],
        "privacy.marked_column"
    );

    let (sorted, routed) = get(
        &host,
        &principals,
        &transport,
        ADMIN,
        "/v1/objects/upload.Contacts",
        &query(None, Some("email:asc")),
    );
    assert!(routed && sorted.status == 403, "{}", sorted.body);
    assert_eq!(
        serde_json::from_str::<Value>(&sorted.body).unwrap()["code"],
        "privacy.marked_column"
    );

    // 2. `profile_objects` refuses with `privacy.marked_column` before touching DuckDB.
    let profile_v2 = server::analytics(
        &host,
        &principals,
        &PanicTransport,
        None,
        &server::Request {
            method: "GET",
            path: "/v1/objects/upload.Contacts:profile",
            query: SCOPE,
            authorization: ADMIN,
            now: 0,
            body: "",
        },
    )
    .expect("profile route");
    assert_eq!(profile_v2.status, 403, "{}", profile_v2.body);
    assert_eq!(
        serde_json::from_str::<Value>(&profile_v2.body).unwrap()["code"],
        "privacy.marked_column"
    );

    // 3. Questions over the dataset refuse with `privacy.marked_column`.
    let dq_v2 = DerivedQuestions::new([7; 32], scope);
    let derived_v2 = dq_v2.questions(&host).expect("derived metrics at v2");
    let engine_q_v2 = EngineQuestions::new(&derived_v2, &PanicTransport, host.scope());
    let q_v2: &dyn AdHocQuestions<NativeEngine> = &engine_q_v2;
    let answer_v2 = server::handle_with(
        &host,
        &principals,
        Some(q_v2),
        &server::Request {
            method: "POST",
            path: "/v1/questions:evaluate",
            query: SCOPE,
            authorization: ADMIN,
            now: 0,
            body: &question_body,
        },
    );
    assert_eq!(answer_v2.status, 422, "{}", answer_v2.body);
    assert_eq!(
        serde_json::from_str::<Value>(&answer_v2.body).unwrap()["code"],
        "privacy.marked_column"
    );

    // 4. `export_objects` refuses without touching DuckDB.
    let export_v2 = server::analytics(
        &host,
        &principals,
        &PanicTransport,
        None,
        &server::Request {
            method: "GET",
            path: "/v1/objects/upload.Contacts:export",
            query: &format!("{SCOPE}&columns=score"),
            authorization: ADMIN,
            now: 0,
            body: "",
        },
    )
    .expect("export route");
    assert_eq!(export_v2.status, 403, "{}", export_v2.body);
}

/// B2 + B3 / MANIFEST-MARK-1: a dataset whose manifest carries markings at load
/// time refuses `:profile` with `privacy.marked_column` before touching `DuckDB`,
/// and stays refused even if a later model drops the marking declaration
/// (monotone union). Unauthorized callers still receive opaque `authz.denied`.
#[test]
fn profile_refuses_load_time_marked_dataset_and_stays_monotone() {
    use norn_cell::{
        CellStore, MAIN_WORLD, Model, StoreOptions,
        dataset_manifest::{
            DatasetManifest, MANIFEST_VERSION, ManifestCounts, ManifestFile, ManifestSource,
        },
    };
    use norn_ir::ObjectTypeDef;

    let make_model = |marked: bool| -> Model {
        let email_prop = if marked {
            json!({
                "id": "email",
                "type": {"kind": "text"},
                "markings": ["pii.contact"]
            })
        } else {
            json!({
                "id": "email",
                "type": {"kind": "text"}
            })
        };
        let def: ObjectTypeDef = serde_json::from_value(json!({
            "name": "test.Customer",
            "properties": [
                {"id": "customer_id", "type": {"kind": "text"}},
                email_prop,
                {"id": "score", "type": {"kind": "integer"}}
            ]
        }))
        .unwrap();
        Model::from_definitions(&[def], &[]).unwrap()
    };

    let scope = seed::demo_scope();
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("marked-dataset.sqlite");

    // Load time: commit a manifest with `email` marked `pii.contact`.
    let mut store = CellStore::open(&db_path, make_model(true), StoreOptions::default()).unwrap();
    let table = store.analytics_table("test.Customer").unwrap();
    let binding = BTreeMap::new();
    let (data_sha, held_sha) = ("11".repeat(32), "22".repeat(32));
    let manifest = DatasetManifest {
        version: MANIFEST_VERSION.into(),
        object_type: "test.Customer".into(),
        load: norn_cell::analytics::dataset_cache_id(&table, &data_sha, &held_sha),
        source: ManifestSource {
            uri: "s3://uploads/demo/customers.csv".into(),
            sha256: "33".repeat(32),
            format: norn_cell::analytics::FileFormat::Csv,
            bytes: 64,
        },
        pins: store
            .dataset_pins("test.Customer", &binding, "test.Customer.mapping@v1")
            .unwrap(),
        binding,
        markings: store.dataset_markings("test.Customer", &table).unwrap(),
        table,
        data: ManifestFile {
            uri: "s3://uploads/demo/customers-data.parquet".into(),
            sha256: data_sha,
            rows: 2,
            bytes: 64,
        },
        held: ManifestFile {
            uri: "s3://uploads/demo/customers-held.parquet".into(),
            sha256: held_sha,
            rows: 0,
            bytes: 0,
        },
        counts: ManifestCounts {
            source: 2,
            admitted: 2,
            held: BTreeMap::new(),
            findings: BTreeMap::new(),
            held_by_rules: 0,
        },
        valid_time: Vec::new(),
    };
    assert_eq!(
        manifest.markings.get("email"),
        Some(&vec!["pii.contact".to_owned()])
    );
    store
        .commit_dataset_manifest(MAIN_WORLD, &scope.key(), 1, &manifest, None)
        .unwrap();

    // 1. Marked at load time -> `:profile` refuses before touching DuckDB.
    let policy = ReadPolicy::compile(store.model().object_types(), &scope, &DEMO_GRANTS).unwrap();
    let principals = FixturePrincipals::new(DEMO_FIXTURES, policy.caller());
    let host = DevHost::from_store(store, scope.clone(), |command| command.payload.clone())
        .unwrap()
        .with_authorizer(policy);
    let profile_marked = server::analytics(
        &host,
        &principals,
        &PanicTransport,
        None,
        &server::Request {
            method: "GET",
            path: "/v1/objects/test.Customer:profile",
            query: SCOPE,
            authorization: ADMIN,
            now: 0,
            body: "",
        },
    )
    .expect("profile route");
    assert_eq!(profile_marked.status, 403, "{}", profile_marked.body);
    assert_eq!(
        serde_json::from_str::<Value>(&profile_marked.body).unwrap()["code"],
        "privacy.marked_column"
    );

    // Wrong scope gets opaque `authz.denied`, never `privacy.marked_column`.
    let denied = server::analytics(
        &host,
        &principals,
        &PanicTransport,
        None,
        &server::Request {
            method: "GET",
            path: "/v1/objects/test.Customer:profile",
            query: "orgId=org_other&environmentId=dev",
            authorization: ADMIN,
            now: 0,
            body: "",
        },
    )
    .expect("profile route");
    assert_eq!(denied.status, 403, "{}", denied.body);
    assert_eq!(
        serde_json::from_str::<Value>(&denied.body).unwrap()["code"],
        "authz.denied"
    );
    drop(host);

    // 2. Reopen with an unmarked model: the manifest's stamped marking is
    //    monotone and still refuses `:profile` without touching DuckDB.
    let store_unmarked =
        CellStore::open(&db_path, make_model(false), StoreOptions::default()).unwrap();
    let policy_unmarked =
        ReadPolicy::compile(store_unmarked.model().object_types(), &scope, &DEMO_GRANTS).unwrap();
    let principals_unmarked = FixturePrincipals::new(DEMO_FIXTURES, policy_unmarked.caller());
    let host_unmarked =
        DevHost::from_store(store_unmarked, scope, |command| command.payload.clone())
            .unwrap()
            .with_authorizer(policy_unmarked);
    let profile_monotone = server::analytics(
        &host_unmarked,
        &principals_unmarked,
        &PanicTransport,
        None,
        &server::Request {
            method: "GET",
            path: "/v1/objects/test.Customer:profile",
            query: SCOPE,
            authorization: ADMIN,
            now: 0,
            body: "",
        },
    )
    .expect("profile route");
    assert_eq!(profile_monotone.status, 403, "{}", profile_monotone.body);
    assert_eq!(
        serde_json::from_str::<Value>(&profile_monotone.body).unwrap()["code"],
        "privacy.marked_column"
    );
}
