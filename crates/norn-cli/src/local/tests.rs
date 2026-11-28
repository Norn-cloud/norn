//! Integration and unit tests for `norn test` and `norn dev` local cell runtimes.

use std::collections::BTreeMap;
use std::error::Error;
use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use norn_api::local_cell::LocalActor;
use norn_api::scope::ScopeQuery;
use serde_json::{Value, json};

use super::dev_server::{dispatch_http_request, handle_connection, parse_loopback_bind};
use super::http_wire::{HttpRequest, MAX_BODY_BYTES};
use super::pack::PackInput;
use super::scenario::{TestArgs, execute_test};
use super::{BootFailure, boot_local_cell, format_refusal};

type TestResult = Result<(), Box<dyn Error>>;

fn boot_err(failure: BootFailure) -> String {
    let out = failure.into_output();
    if out.stdout.is_empty() {
        out.stderr
    } else {
        out.stdout
    }
}

static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

struct TempWorkspace(PathBuf);

impl TempWorkspace {
    fn new(label: &str) -> Result<Self, Box<dyn Error>> {
        let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "norn-cli-local-{label}-{}-{seq}",
            std::process::id()
        ));
        if root.exists() {
            std::fs::remove_dir_all(&root)?;
        }
        std::fs::create_dir_all(&root)?;
        Ok(Self(std::fs::canonicalize(&root)?))
    }

    fn write(&self, rel: &str, text: &str) -> Result<PathBuf, Box<dyn Error>> {
        let dest = self.0.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, text)?;
        Ok(dest)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let _cleaned = std::fs::remove_dir_all(&self.0);
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn crm_source_path() -> PathBuf {
    repo_root().join("tools/norn-ts/fixtures/definitions/crm.source.ts")
}

fn crm_ir_path() -> PathBuf {
    repo_root().join("tools/norn-ts/fixtures/definitions/crm.ir.json")
}

fn make_http_request(
    method: &str,
    path: &str,
    query: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> HttpRequest {
    let mut map = BTreeMap::new();
    for (key, value) in headers {
        map.insert((*key).to_ascii_lowercase(), (*value).to_owned());
    }
    HttpRequest {
        method: method.to_owned(),
        path: path.to_owned(),
        query: query.to_owned(),
        headers: map,
        body: body.to_owned(),
    }
}

fn send_raw_http(addr: std::net::SocketAddr, raw: &str) -> Result<String, Box<dyn Error>> {
    let mut stream = TcpStream::connect(addr)?;
    stream.write_all(raw.as_bytes())?;
    stream.flush()?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(response)
}

#[test]
fn loopback_bind_rejects_non_loopback_and_accepts_loopback() -> TestResult {
    for forbidden in [
        "0.0.0.0:4040",
        "192.168.1.5:4040",
        "[::]:4040",
        "not-an-addr",
    ] {
        assert!(
            parse_loopback_bind(forbidden).is_err(),
            "expected `{forbidden}` to be rejected"
        );
    }
    assert!(parse_loopback_bind("127.0.0.1:0")?.ip().is_loopback());
    assert!(parse_loopback_bind("[::1]:0")?.ip().is_loopback());
    Ok(())
}

#[test]
fn norn_test_runs_crm_ts_pack_actions_clock_authz_reads_and_plan_apply() -> TestResult {
    let scratch = TempWorkspace::new("crm-scenario")?;
    let crm_ir_text = std::fs::read_to_string(crm_ir_path())?;
    let mut v2_artifacts: Vec<Value> = serde_json::from_str(&crm_ir_text)?;
    for artifact in &mut v2_artifacts {
        if artifact.get("artifactId").and_then(Value::as_str) == Some("crm.Contact.entity")
            && let Some(obj_type) = artifact
                .get_mut("payload")
                .and_then(Value::as_object_mut)
                .and_then(|p| p.get_mut("objectType"))
                .and_then(Value::as_object_mut)
        {
            obj_type.insert(
                "description".to_owned(),
                Value::from("A person at an account (v2)."),
            );
        }
    }
    let v2_path = scratch.write("crm-v2.json", &serde_json::to_string(&v2_artifacts)?)?;

    let mut breaking = v2_artifacts;
    breaking.push(json!({
        "artifactId": "crm.contact.active_only.law",
        "dependencies": [
            { "artifactId": "crm.Contact.entity", "kind": "entity", "required": true, "version": "0.1.0" }
        ],
        "irVersion": "0.1",
        "kind": "law",
        "lifecycle": "draft",
        "name": "Contacts must remain active.",
        "namespace": "crm",
        "payload": {
            "condition": "true",
            "id": "crm.contact.active_only",
            "lawVersion": "0.1",
            "modality": { "kind": "always" },
            "owner": "crm",
            "provenance": { "source": "authoring_sdk", "sourcePath": "test/crm.law.ts" },
            "rationale": "Test live-law refusal on archived contacts.",
            "response": { "expression": "self.status == \"active\"", "kind": "predicate" },
            "scope": {
                "objectTypes": ["crm.Contact"],
                "root": { "id": "crm.Contact", "kind": "entity" }
            },
            "severity": "error",
            "subject": "crm.Contact"
        },
        "provenance": { "source": "authoring_sdk", "sourcePath": "test/crm.law.ts" },
        "scope": { "orgId": "org_demo", "environmentId": "dev" },
        "version": "0.1.0"
    }));
    let breaking_path = scratch.write("crm-breaking.json", &serde_json::to_string(&breaking)?)?;

    let scenario_doc = json!({
        "name": "crm-end-to-end",
        "org": "org_demo",
        "env": "dev",
        "clockMs": 1_767_225_600_000_u64,
        "pack": { "source": crm_source_path().to_string_lossy() },
        "steps": [
            {
                "op": "defs",
                "actor": { "userId": "v1", "roles": ["viewer"] },
                "expect": { "ok": true, "minDefinitions": 6, "version": 2 }
            },
            {
                "op": "defs",
                "expect": { "ok": false, "refusalCode": "authz.denied" }
            },
            {
                "op": "action",
                "actionId": "crm.contact.upsert",
                "idempotencyKey": "denied-viewer",
                "actor": { "userId": "v1", "roles": ["viewer"] },
                "input": { "contact_id": "c-1", "email": "a@example.com" },
                "expect": { "ok": false, "refusalCode": "authz.denied" }
            },
            {
                "op": "action",
                "actionId": "crm.contact.upsert",
                "idempotencyKey": "denied-anon",
                "input": { "contact_id": "c-1", "email": "a@example.com" },
                "expect": { "ok": false, "refusalCode": "authz.denied" }
            },
            {
                "op": "action",
                "actionId": "crm.contact.upsert",
                "idempotencyKey": "denied-scope",
                "org": "other_org",
                "actor": { "userId": "m1", "roles": ["member"] },
                "input": { "contact_id": "c-1", "email": "a@example.com" },
                "expect": { "ok": false, "refusalCode": "authz.denied" }
            },
            {
                "op": "action",
                "actionId": "crm.contact.upsert",
                "idempotencyKey": "create-c1",
                "actor": { "userId": "m1", "roles": ["member"] },
                "input": { "contact_id": "c-1", "email": "a@example.com" },
                "expect": {
                    "ok": true,
                    "assertReceipt": true,
                    "replayed": false,
                    "objectType": "crm.Contact",
                    "objectId": "c-1",
                    "version": 1,
                    "outputContains": { "receipt": { "recordedAtMs": 1_767_225_600_000_u64 } }
                }
            },
            {
                "op": "action",
                "actionId": "crm.contact.upsert",
                "idempotencyKey": "create-c1",
                "actor": { "userId": "m1", "roles": ["member"] },
                "input": { "contact_id": "c-1", "email": "a@example.com" },
                "expect": { "ok": true, "replayed": true, "objectId": "c-1", "version": 1 }
            },
            { "op": "advance_clock", "ms": 60_000 },
            {
                "op": "action",
                "actionId": "crm.contact.archive",
                "idempotencyKey": "archive-c1",
                "actor": { "userId": "m1", "roles": ["member"] },
                "input": { "contact_id": "c-1" },
                "expect": {
                    "ok": true,
                    "assertReceipt": true,
                    "version": 2,
                    "outputContains": { "receipt": { "recordedAtMs": 1_767_225_660_000_u64 } }
                }
            },
            {
                "op": "action",
                "actionId": "crm.contact.archive",
                "idempotencyKey": "archive-c1-again",
                "actor": { "userId": "m1", "roles": ["member"] },
                "input": { "contact_id": "c-1" },
                "expect": { "ok": false }
            },
            {
                "op": "get_object",
                "type": "crm.Contact",
                "id": "c-1",
                "actor": { "userId": "m1", "roles": ["member"] },
                "expect": {
                    "ok": true,
                    "withheld": false,
                    "version": 2,
                    "fields": { "contact_id": "c-1", "status": "archived", "email": "a@example.com" }
                }
            },
            {
                "op": "get_object",
                "type": "crm.Contact",
                "id": "c-1",
                "actor": { "userId": "v1", "roles": ["viewer"] },
                "expect": { "ok": true, "withheld": true }
            },
            {
                "op": "list_objects",
                "type": "crm.Contact",
                "actor": { "userId": "m1", "roles": ["member"] },
                "expect": { "ok": true, "count": 1 }
            },
            {
                "op": "plan",
                "actor": { "userId": "m1", "roles": ["member"] },
                "artifacts": v2_path.to_string_lossy(),
                "expect": { "ok": false, "refusalCode": "authz.denied" }
            },
            {
                "op": "plan",
                "actor": { "userId": "a1", "roles": ["admin"] },
                "artifacts": v2_path.to_string_lossy(),
                "expect": {
                    "ok": true,
                    "unchanged": false,
                    "baseVersion": 2,
                    "nextVersion": 3,
                    "rippleIds": ["crm.contact.upsert.action"]
                }
            },
            {
                "op": "apply",
                "actor": { "userId": "a1", "roles": ["admin"] },
                "artifacts": v2_path.to_string_lossy(),
                "expect": { "ok": true, "unchanged": false, "definitionsVersion": 3 }
            },
            {
                "op": "plan",
                "actor": { "userId": "a1", "roles": ["admin"] },
                "artifacts": breaking_path.to_string_lossy(),
                "expect": { "ok": false, "refusalCode": "defs.live_data_refused" }
            }
        ]
    });
    let scenario_path = scratch.write("crm.scenario.json", &scenario_doc.to_string())?;
    let output = execute_test(&TestArgs {
        scenarios: vec![scenario_path],
        source: None,
        artifacts: None,
        base: Vec::new(),
        org: None,
        env: None,
        json: true,
    });
    assert_eq!(
        output.exit_code, 0,
        "crm scenario failed: stdout={} stderr={}",
        output.stdout, output.stderr
    );
    let report: Value = serde_json::from_str(&output.stdout)?;
    assert_eq!(report.get("ok").and_then(Value::as_bool), Some(true));
    Ok(())
}

#[test]
fn norn_test_runs_python_pack_with_reserved_core_units_base() -> TestResult {
    let scratch = TempWorkspace::new("py-hr")?;
    let scenario_path = scratch.write(
        "hr.scenario.json",
        &json!({
            "name": "hr-pilot-python",
            "steps": [
                {
                    "op": "defs",
                    "actor": { "userId": "u1", "roles": ["viewer"] },
                    "expect": { "ok": true, "minDefinitions": 7, "version": 2 }
                }
            ]
        })
        .to_string(),
    )?;
    let output = execute_test(&TestArgs {
        scenarios: vec![scenario_path],
        source: Some(repo_root().join("tools/norn-py/examples/hr_pilot.py")),
        artifacts: None,
        base: vec![repo_root().join("registry/core/artifacts/core.units.json")],
        org: Some("org_demo".to_owned()),
        env: Some("dev".to_owned()),
        json: false,
    });
    assert_eq!(
        output.exit_code, 0,
        "hr_pilot scenario failed: stdout={} stderr={}",
        output.stdout, output.stderr
    );
    assert!(output.stdout.contains("PASS hr-pilot-python"));
    Ok(())
}

#[test]
fn norn_test_rejects_unknown_scenario_keys_and_inadmissible_pack() -> TestResult {
    let scratch = TempWorkspace::new("invalid-scenario")?;
    let bad_key = scratch.write(
        "unknown-key.json",
        &json!({
            "name": "bad",
            "org": "org_demo",
            "env": "dev",
            "bogus": true,
            "steps": []
        })
        .to_string(),
    )?;
    let out_bad_key = execute_test(&TestArgs {
        scenarios: vec![bad_key],
        source: None,
        artifacts: None,
        base: Vec::new(),
        org: None,
        env: None,
        json: false,
    });
    assert_eq!(out_bad_key.exit_code, 2);
    assert!(out_bad_key.stderr.contains("unknown field"));

    let bad_pack = scratch.write(
        "bad-pack.json",
        &json!([{
            "artifactId": "broken.Bad.entity",
            "kind": "entity",
            "irVersion": "0.1",
            "lifecycle": "draft",
            "name": "Bad",
            "namespace": "broken",
            "version": "0.1.0",
            "scope": { "orgId": "org_demo", "environmentId": "dev" },
            "dependencies": [],
            "provenance": { "source": "authoring_sdk", "sourcePath": "bad.ts" },
            "payload": { "entityVersion": "2.0" }
        }])
        .to_string(),
    )?;
    let bad_pack_scenario = scratch.write(
        "inadmissible.json",
        &json!({
            "name": "inadmissible-startup",
            "org": "org_demo",
            "env": "dev",
            "pack": { "artifacts": bad_pack.to_string_lossy() },
            "steps": []
        })
        .to_string(),
    )?;
    let out_bad_pack = execute_test(&TestArgs {
        scenarios: vec![bad_pack_scenario],
        source: None,
        artifacts: None,
        base: Vec::new(),
        org: None,
        env: None,
        json: true,
    });
    assert_eq!(out_bad_pack.exit_code, 1);
    let report: Value = serde_json::from_str(&out_bad_pack.stdout)?;
    assert_eq!(report.get("ok").and_then(Value::as_bool), Some(false));
    Ok(())
}

#[test]
fn norn_dev_http_dispatch_and_loopback_tcp_round_trip() -> TestResult {
    let pack = PackInput::Source(crm_source_path());
    let cell =
        boot_local_cell("org_demo", "dev", None, None, &[], Some(&pack)).map_err(boot_err)?;

    let health = dispatch_http_request(&cell, &make_http_request("GET", "/health", "", &[], ""));
    assert_eq!(health.status, 200);
    let health_json: Value = serde_json::from_str(&health.body)?;
    assert_eq!(health_json.get("ok").and_then(Value::as_bool), Some(true));

    let bad_method =
        dispatch_http_request(&cell, &make_http_request("POST", "/health", "", &[], ""));
    assert_eq!(bad_method.status, 405);

    let anon_defs = dispatch_http_request(
        &cell,
        &make_http_request(
            "GET",
            "/v1/defs",
            "orgId=org_demo&environmentId=dev",
            &[],
            "",
        ),
    );
    assert_eq!(anon_defs.status, 403);

    let conflict_scope = dispatch_http_request(
        &cell,
        &make_http_request(
            "GET",
            "/v1/defs",
            "orgId=org_demo&environmentId=dev",
            &[
                ("x-norn-org", "other_org"),
                ("x-norn-actor", "u1"),
                ("x-norn-roles", "viewer"),
            ],
            "",
        ),
    );
    assert_eq!(conflict_scope.status, 403);

    let defs_ok = dispatch_http_request(
        &cell,
        &make_http_request(
            "GET",
            "/v1/defs",
            "orgId=org_demo&environmentId=dev",
            &[("x-norn-actor", "u1"), ("x-norn-roles", "viewer")],
            "",
        ),
    );
    assert_eq!(defs_ok.status, 200);

    let call_body = json!({
        "input": { "contact_id": "dev-1", "email": "dev@example.com" },
        "idempotencyKey": "dev-k1"
    })
    .to_string();
    let denied_call = dispatch_http_request(
        &cell,
        &make_http_request(
            "POST",
            "/v1/actions/crm.contact.upsert:call",
            "orgId=org_demo&environmentId=dev",
            &[("x-norn-actor", "v1"), ("x-norn-roles", "viewer")],
            &call_body,
        ),
    );
    assert_eq!(denied_call.status, 403);

    let committed = dispatch_http_request(
        &cell,
        &make_http_request(
            "POST",
            "/v1/actions/crm.contact.upsert:call",
            "orgId=org_demo&environmentId=dev",
            &[("x-norn-actor", "m1"), ("x-norn-roles", "member")],
            &call_body,
        ),
    );
    assert_eq!(committed.status, 200, "body={}", committed.body);

    let obj = dispatch_http_request(
        &cell,
        &make_http_request(
            "GET",
            "/v1/objects/crm.Contact/dev-1",
            "orgId=org_demo&environmentId=dev",
            &[("x-norn-actor", "m1"), ("x-norn-roles", "member")],
            "",
        ),
    );
    assert_eq!(obj.status, 200);
    let obj_json: Value = serde_json::from_str(&obj.body)?;
    assert_eq!(
        obj_json.pointer("/values/email").and_then(Value::as_str),
        Some("dev@example.com")
    );

    let plan_body = format!(
        r#"{{"artifacts":{}}}"#,
        std::fs::read_to_string(crm_ir_path())?
    );
    let plan_bearer = dispatch_http_request(
        &cell,
        &make_http_request(
            "POST",
            "/v1/cell/definitions:plan",
            "orgId=org_demo&environmentId=dev",
            &[("authorization", "Bearer dev:alice:owner")],
            &plan_body,
        ),
    );
    assert_eq!(plan_bearer.status, 200, "body={}", plan_bearer.body);

    // Real TCP loopback round-trip: 200 /health and 413 > MAX_BODY_BYTES.
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    std::thread::scope(|scope| -> TestResult {
        let server = scope.spawn(|| {
            for _ in 0..2 {
                if let Ok((stream, _peer)) = listener.accept() {
                    let _handled = handle_connection(&stream, &cell);
                }
            }
        });
        let raw_health = send_raw_http(addr, "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")?;
        assert!(raw_health.starts_with("HTTP/1.1 200 OK\r\n"));

        let oversized_len = MAX_BODY_BYTES.saturating_add(1);
        let raw_large = send_raw_http(
            addr,
            &format!(
                "POST /v1/actions/crm.contact.upsert:call?orgId=org_demo&environmentId=dev HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {oversized_len}\r\n\r\n"
            ),
        )?;
        assert!(raw_large.starts_with("HTTP/1.1 413 Payload Too Large\r\n"));
        assert!(raw_large.contains("request.too_large"));
        let _joined = server.join();
        Ok(())
    })?;
    Ok(())
}

#[test]
fn local_cell_sqlite_db_persists_state_across_reopen() -> TestResult {
    let scratch = TempWorkspace::new("sqlite-persist")?;
    let db_path = scratch.path().join("cell.sqlite");
    let pack = PackInput::Source(crm_source_path());
    let member = LocalActor::new("m1", vec!["member".to_owned()]);
    let scope_query = ScopeQuery {
        org_id: Some("org_demo".to_owned()),
        environment_id: Some("dev".to_owned()),
    };
    {
        let cell = boot_local_cell("org_demo", "dev", None, Some(&db_path), &[], Some(&pack))
            .map_err(boot_err)?;
        let query = norn_api::actions::ActionQuery {
            org_id: Some("org_demo".to_owned()),
            environment_id: Some("dev".to_owned()),
            version: None,
        };
        let body = json!({
            "input": { "contact_id": "persist-1", "email": "p@example.com" },
            "idempotencyKey": "persist-k1"
        })
        .to_string();
        let _answer = cell
            .call_action(&query, Some(&member), "crm.contact.upsert", &body)
            .map_err(|refusal| format_refusal(&refusal))?;
    }
    let reopened = boot_local_cell("org_demo", "dev", None, Some(&db_path), &[], Some(&pack))
        .map_err(boot_err)?;
    let record = reopened
        .get_object(&scope_query, Some(&member), "crm.Contact", "persist-1")
        .map_err(|refusal| format_refusal(&refusal))?;
    assert!(!record.withheld);
    let raw = norn_api::privacy::serialize_disclosed(&record)
        .map_err(|refusal| format_refusal(&refusal))?;
    let parsed: Value = serde_json::from_str(&raw)?;
    assert_eq!(
        parsed.pointer("/values/email").and_then(Value::as_str),
        Some("p@example.com")
    );
    Ok(())
}
