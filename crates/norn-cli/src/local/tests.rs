//! Integration and unit tests for `norn test` and `norn dev` local cell runtimes.

use std::collections::BTreeMap;
use std::error::Error;
use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use norn_api::local_cell::LocalActor;
use norn_api::scope::ScopeQuery;
use norn_ir::RuntimeValue;
use serde_json::{Value, json};

use super::dev_auth::DevAuth;
use super::dev_server::{
    dispatch_http_request, handle_connection, handle_connection_with_timeout, parse_loopback_bind,
};
use super::http_wire::{HttpRequest, MAX_BODY_BYTES, percent_decode_path, percent_decode_query};
use super::pack::{PackInput, load_packs, load_packs_with_budget};
use super::scenario::{TestArgs, encode_action_body, execute_test};
use super::scenario_assert::values_match;
use super::{BootFailure, boot_local_cell, format_refusal};

type TestResult = Result<(), Box<dyn Error>>;

const TEST_DEV_TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

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

fn test_auth() -> DevAuth {
    DevAuth::with_token(TEST_DEV_TOKEN)
}

fn make_http_request(
    method: &str,
    path: &str,
    query: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> HttpRequest {
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (key, value) in headers {
        map.entry((*key).to_ascii_lowercase())
            .or_default()
            .push((*value).to_owned());
    }
    HttpRequest {
        method: method.to_owned(),
        path: path.to_owned(),
        query: query.to_owned(),
        headers: map,
        body: body.to_owned(),
    }
}

fn send_raw_http(addr: SocketAddr, raw: &str) -> Result<String, Box<dyn Error>> {
    let mut stream = TcpStream::connect(addr)?;
    stream.write_all(raw.as_bytes())?;
    stream.flush()?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(response)
}

fn write_crm_v2_and_breaking_packs(
    scratch: &TempWorkspace,
) -> Result<(PathBuf, PathBuf), Box<dyn Error>> {
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
    breaking.push(crm_active_only_law());
    let breaking_path = scratch.write("crm-breaking.json", &serde_json::to_string(&breaking)?)?;
    Ok((v2_path, breaking_path))
}

fn crm_active_only_law() -> Value {
    json!({
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
    })
}

fn crm_authz_and_action_steps() -> Vec<Value> {
    vec![
        json!({
            "op": "defs",
            "actor": { "userId": "v1", "roles": ["viewer"] },
            "expect": { "ok": true, "minDefinitions": 6, "version": 2 }
        }),
        json!({ "op": "defs", "expect": { "ok": false, "refusalCode": "authz.denied" } }),
        json!({
            "op": "action",
            "actionId": "crm.contact.upsert",
            "idempotencyKey": "denied-viewer",
            "actor": { "userId": "v1", "roles": ["viewer"] },
            "input": { "contact_id": "c-1", "email": "a@example.com" },
            "expect": { "ok": false, "refusalCode": "authz.denied" }
        }),
        json!({
            "op": "action",
            "actionId": "crm.contact.upsert",
            "idempotencyKey": "denied-anon",
            "input": { "contact_id": "c-1", "email": "a@example.com" },
            "expect": { "ok": false, "refusalCode": "authz.denied" }
        }),
        json!({
            "op": "action",
            "actionId": "crm.contact.upsert",
            "idempotencyKey": "denied-scope",
            "org": "other_org",
            "actor": { "userId": "m1", "roles": ["member"] },
            "input": { "contact_id": "c-1", "email": "a@example.com" },
            "expect": { "ok": false, "refusalCode": "authz.denied" }
        }),
        json!({
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
                // Monotonic +1ms after initial tenant definition install at 1_767_225_600_000.
                "outputContains": { "receipt": { "recordedAtMs": 1_767_225_600_001_u64 } }
            }
        }),
        json!({
            "op": "action",
            "actionId": "crm.contact.upsert",
            "idempotencyKey": "create-c1",
            "actor": { "userId": "m1", "roles": ["member"] },
            "input": { "contact_id": "c-1", "email": "a@example.com" },
            "expect": { "ok": true, "replayed": true, "objectId": "c-1", "version": 1 }
        }),
    ]
}

fn crm_lifecycle_and_read_steps() -> Vec<Value> {
    vec![
        json!({ "op": "advance_clock", "ms": 60_000 }),
        json!({
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
        }),
        json!({
            "op": "action",
            "actionId": "crm.contact.archive",
            "idempotencyKey": "archive-c1-again",
            "actor": { "userId": "m1", "roles": ["member"] },
            "input": { "contact_id": "c-1" },
            "expect": { "ok": false }
        }),
        json!({
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
        }),
        json!({
            "op": "get_object",
            "type": "crm.Contact",
            "id": "c-1",
            "actor": { "userId": "v1", "roles": ["viewer"] },
            "expect": { "ok": true, "withheld": true }
        }),
        json!({
            "op": "list_objects",
            "type": "crm.Contact",
            "actor": { "userId": "m1", "roles": ["member"] },
            "expect": { "ok": true, "count": 1 }
        }),
    ]
}

fn crm_plan_and_apply_steps(v2_path: &Path, breaking_path: &Path) -> Vec<Value> {
    vec![
        json!({
            "op": "plan",
            "actor": { "userId": "m1", "roles": ["member"] },
            "artifacts": v2_path.to_string_lossy(),
            "expect": { "ok": false, "refusalCode": "authz.denied" }
        }),
        json!({
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
        }),
        json!({
            "op": "apply",
            "actor": { "userId": "a1", "roles": ["admin"] },
            "artifacts": v2_path.to_string_lossy(),
            "expect": { "ok": true, "unchanged": false, "definitionsVersion": 3 }
        }),
        json!({
            "op": "plan",
            "actor": { "userId": "a1", "roles": ["admin"] },
            "artifacts": breaking_path.to_string_lossy(),
            "expect": { "ok": false, "refusalCode": "defs.live_data_refused" }
        }),
    ]
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
    let (v2_path, breaking_path) = write_crm_v2_and_breaking_packs(&scratch)?;
    let mut steps = crm_authz_and_action_steps();
    steps.extend(crm_lifecycle_and_read_steps());
    steps.extend(crm_plan_and_apply_steps(&v2_path, &breaking_path));
    let scenario_doc = json!({
        "name": "crm-end-to-end",
        "org": "org_demo",
        "env": "dev",
        "clockMs": 1_767_225_600_000_u64,
        "pack": { "source": crm_source_path().to_string_lossy() },
        "steps": steps
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
fn norn_dev_requires_session_token_and_rejects_ambiguous_headers() -> TestResult {
    let pack = PackInput::Source(crm_source_path());
    let cell =
        boot_local_cell("org_demo", "dev", None, None, &[], Some(&pack)).map_err(boot_err)?;
    let auth = test_auth();
    let bearer_header = format!("Bearer {}", auth.token());

    assert_eq!(
        dispatch_http_request(
            &cell,
            &auth,
            &make_http_request("GET", "/health", "", &[], "")
        )
        .status,
        401
    );
    assert_eq!(
        dispatch_http_request(
            &cell,
            &auth,
            &make_http_request("GET", "/health", "", &[("x-norn-dev-token", "wrong")], ""),
        )
        .status,
        401
    );
    assert_eq!(
        dispatch_http_request(
            &cell,
            &auth,
            &make_http_request(
                "GET",
                "/health",
                "",
                &[("authorization", &bearer_header)],
                ""
            ),
        )
        .status,
        200
    );

    for bad_headers in [
        vec![
            ("authorization", bearer_header.as_str()),
            ("x-norn-actor", "u1"),
            ("x-norn-roles", "viewer"),
        ],
        vec![
            ("x-norn-dev-token", auth.token()),
            ("x-norn-org", "org_demo"),
            ("x-norn-org-id", "other_org"),
            ("x-norn-actor", "u1"),
            ("x-norn-roles", "viewer"),
        ],
        vec![
            ("x-norn-dev-token", auth.token()),
            ("x-norn-actor", "u1"),
            ("x-norn-actor", "u2"),
            ("x-norn-roles", "viewer"),
        ],
    ] {
        let resp = dispatch_http_request(
            &cell,
            &auth,
            &make_http_request(
                "GET",
                "/v1/defs",
                "orgId=org_demo&environmentId=dev",
                &bad_headers,
                "",
            ),
        );
        assert_eq!(resp.status, 400, "expected 400 for {bad_headers:?}");
    }
    Ok(())
}

#[test]
fn norn_dev_http_dispatch_routes_and_literal_plus_path_decoding() -> TestResult {
    let pack = PackInput::Source(crm_source_path());
    let cell =
        boot_local_cell("org_demo", "dev", None, None, &[], Some(&pack)).map_err(boot_err)?;
    let auth = test_auth();
    let tok = auth.token();
    let member_headers = [
        ("x-norn-dev-token", tok),
        ("x-norn-actor", "m1"),
        ("x-norn-roles", "member"),
    ];
    let q = "orgId=org_demo&environmentId=dev";

    assert_eq!(
        dispatch_http_request(
            &cell,
            &auth,
            &make_http_request("POST", "/health", "", &[("x-norn-dev-token", tok)], ""),
        )
        .status,
        405
    );
    assert_eq!(
        dispatch_http_request(
            &cell,
            &auth,
            &make_http_request("GET", "/v1/defs", q, &[("x-norn-dev-token", tok)], ""),
        )
        .status,
        403
    );

    let call_body = json!({
        "input": { "contact_id": "dev+1", "email": "dev@example.com" },
        "idempotencyKey": "dev-k1"
    })
    .to_string();
    let committed = dispatch_http_request(
        &cell,
        &auth,
        &make_http_request(
            "POST",
            "/v1/actions/crm.contact.upsert:call",
            q,
            &member_headers,
            &call_body,
        ),
    );
    assert_eq!(committed.status, 200, "body={}", committed.body);

    let obj = dispatch_http_request(
        &cell,
        &auth,
        &make_http_request(
            "GET",
            "/v1/objects/crm.Contact/dev+1",
            q,
            &member_headers,
            "",
        ),
    );
    assert_eq!(obj.status, 200, "body={}", obj.body);
    let obj_json: Value = serde_json::from_str(&obj.body)?;
    assert_eq!(
        obj_json
            .pointer("/values/contact_id")
            .and_then(Value::as_str),
        Some("dev+1")
    );
    assert_eq!(percent_decode_path("a+b%20c")?, "a+b c");
    assert_eq!(percent_decode_query("a+b%20c")?, "a b c");
    assert!(percent_decode_path("bad%ZZ").is_err());
    Ok(())
}

#[test]
fn http_wire_rejects_malformed_framing_and_enforces_deadline() -> TestResult {
    let pack = PackInput::Source(crm_source_path());
    let cell =
        boot_local_cell("org_demo", "dev", None, None, &[], Some(&pack)).map_err(boot_err)?;
    let auth = test_auth();
    let tok = auth.token();
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;

    let bad_requests = [
        format!("GET /health HTTP/1.1\nHost: 127.0.0.1\nX-Norn-Dev-Token: {tok}\n\n"),
        format!("GET /health HTTP/1.1\r\nHost : 127.0.0.1\r\nX-Norn-Dev-Token: {tok}\r\n\r\n"),
        format!(
            "POST /health HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Norn-Dev-Token: {tok}\r\nContent-Length: +10\r\n\r\n"
        ),
        format!(
            "POST /health HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Norn-Dev-Token: {tok}\r\nContent-Length: 0\r\nContent-Length: 0\r\n\r\n"
        ),
        format!(
            "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Norn-Dev-Token: {tok}\r\nX-Norn-Dev-Token: {tok}\r\n\r\n"
        ),
    ];
    let oversized_len = MAX_BODY_BYTES.saturating_add(1);
    let oversized = format!(
        "POST /v1/actions/crm.contact.upsert:call?orgId=org_demo&environmentId=dev HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Norn-Dev-Token: {tok}\r\nContent-Length: {oversized_len}\r\n\r\n"
    );

    std::thread::scope(|scope| -> TestResult {
        let server = scope.spawn(|| {
            for _ in 0..bad_requests.len().saturating_add(2) {
                if let Ok((stream, _peer)) = listener.accept() {
                    let _handled = handle_connection(&stream, &cell, &auth);
                }
            }
            if let Ok((stream, _peer)) = listener.accept() {
                let _handled = handle_connection_with_timeout(
                    &stream,
                    &cell,
                    &auth,
                    Duration::from_millis(120),
                );
            }
        });
        for raw in &bad_requests {
            let resp = send_raw_http(addr, raw)?;
            assert!(
                resp.starts_with("HTTP/1.1 400 Bad Request\r\n"),
                "expected 400 for `{raw:?}`, got `{resp}`"
            );
        }
        let ok_health = send_raw_http(
            addr,
            &format!("GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Norn-Dev-Token: {tok}\r\n\r\n"),
        )?;
        assert!(ok_health.starts_with("HTTP/1.1 200 OK\r\n"));
        let too_large = send_raw_http(addr, &oversized)?;
        assert!(too_large.starts_with("HTTP/1.1 413 Payload Too Large\r\n"));

        // Stalled client held past the 120ms whole-request deadline receives 408.
        let mut slow = TcpStream::connect(addr)?;
        slow.write_all(b"GET /he")?;
        slow.flush()?;
        let mut slow_resp = String::new();
        slow.read_to_string(&mut slow_resp)?;
        assert!(
            slow_resp.starts_with("HTTP/1.1 408 Request Timeout\r\n"),
            "expected 408 timeout, got `{slow_resp}`"
        );
        assert!(server.join().is_ok(), "server thread panicked");
        Ok(())
    })?;
    Ok(())
}

#[test]
fn scenario_assert_preserves_exact_decimals_and_distinguishes_numeric_strings() -> TestResult {
    // Distinct numeric-looking strings must never compare equal.
    assert!(!values_match(
        &RuntimeValue::String("01".to_owned()),
        &RuntimeValue::String("1.0".to_owned()),
    ));
    assert!(values_match(
        &RuntimeValue::String("01".to_owned()),
        &RuntimeValue::String("01".to_owned()),
    ));
    assert!(!values_match(
        &RuntimeValue::String("1.0".to_owned()),
        &RuntimeValue::from_json_text("1.0")?,
    ));

    // High-precision decimals (>53 bits) must survive parsing and encoding without f64 rounding.
    let high_a = RuntimeValue::from_json_text(r#"{"amount":12345678901234567890.0001}"#)?;
    let high_a_zero = RuntimeValue::from_json_text(r#"{"amount":12345678901234567890.00010}"#)?;
    let high_b = RuntimeValue::from_json_text(r#"{"amount":12345678901234567890.0002}"#)?;
    assert!(values_match(&high_a, &high_a_zero));
    assert!(!values_match(&high_a, &high_b));
    let encoded = encode_action_body(&high_a, "exact-dec-key")?;
    assert!(
        encoded.contains("12345678901234567890.0001"),
        "encoded action body rounded decimal: {encoded}"
    );
    Ok(())
}

#[test]
fn load_packs_preserves_layers_refuses_duplicate_base_and_shares_budget() -> TestResult {
    let scratch = TempWorkspace::new("pack-layers")?;
    let base_a = scratch.write(
        "base-a.json",
        &json!([{ "artifactId": "core.a.entity" }]).to_string(),
    )?;
    let base_b = scratch.write(
        "base-b.json",
        &json!([{ "artifactId": "core.b.entity" }]).to_string(),
    )?;
    let app_spoof = scratch.write(
        "app-spoof.json",
        &json!([{ "artifactId": "core.a.entity" }]).to_string(),
    )?;

    // App artifact with same reserved ID stays in app_artifacts and does not replace base.
    let loaded = load_packs(
        std::slice::from_ref(&base_a),
        Some(&PackInput::Artifacts(app_spoof)),
        "org_demo",
        "dev",
    )
    .map_err(|err| match err {
        super::pack::PackLoadError::Diagnostics(doc) => format!("load_packs diagnostics: {doc}"),
        super::pack::PackLoadError::Io(msg) => format!("load_packs io error: {msg}"),
    })?;
    assert_eq!(loaded.base_artifacts.len(), 1);
    assert_eq!(loaded.app_artifacts.len(), 1);

    // Duplicate reserved ID across repeatable --base packs refuses fail-closed.
    assert!(
        load_packs(&[base_a.clone(), base_a.clone()], None, "org_demo", "dev").is_err(),
        "duplicate base artifactId must fail closed"
    );

    // Aggregate budget shared across repeatable --base packs refuses when combined size exceeds cap.
    let single_len = std::fs::metadata(&base_a)?.len();
    let combined_cap = single_len.saturating_add(10);
    assert!(
        load_packs_with_budget(&[base_a, base_b], None, "org_demo", "dev", combined_cap).is_err(),
        "combined packs exceeding aggregate budget must fail closed"
    );
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
