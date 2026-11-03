//! SURFACE-DEMO-1/2/3: real HTTP, MCP, CLI and rendered browser submissions share authority.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    missing_docs,
    reason = "four-surface correspondence assertions"
)]

use super::*;
use crate::{
    authz::{AccessRequest, Authorizer, DenyAll},
    routes::router,
};
use norn_cell::engine::NativeEngine;
use norn_kernel::{
    ActionAuthzRequest, ActionDenial, ActionReadAuthzRequest, ActionReadTypeAuthzRequest,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
use tower::ServiceExt;

struct Demo;
impl Authorizer for Demo {
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Refusal> {
        if matches!(request.actor, Some("demo" | "peer")) {
            Ok(())
        } else {
            DenyAll.authorize(request)
        }
    }
    fn action(&self, _: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
    fn action_read(&self, _: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
    fn action_read_type(&self, _: &ActionReadTypeAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
}

fn dump(host: &DevHost<NativeEngine>) -> BTreeMap<String, Vec<String>> {
    host.cell()
        .expect("cell")
        .store()
        .read()
        .dump()
        .expect("complete dump")
}

#[tokio::test]
async fn action_document_is_authorized_before_rendering() {
    let host = Arc::new(
        DevHost::new(
            NativeEngine::open_in_memory().expect("engine"),
            "[]",
            CellScope {
                org_id: "org_surfaces".into(),
                environment_id: "dev".into(),
            },
            LogicalTime::EPOCH,
        )
        .expect("admission")
        .with_authorizer(Demo),
    );
    let before = dump(&host);
    let app = router(Arc::clone(&host));
    for (actor, accept, status, content_type) in [
        ("demo", "text/html", 200, "text/html"),
        ("demo", "application/json", 200, "application/json"),
        ("demo", "text/html;q=0", 200, "application/json"),
        ("forbidden", "text/html", 403, "application/json"),
    ] {
        let response = app
            .clone()
            .oneshot(
                http::Request::builder()
                    .uri("/v1/defs?orgId=org_surfaces&environmentId=dev")
                    .header("x-norn-actor", actor)
                    .header("accept", accept)
                    .body(axum::body::Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status().as_u16(), status);
        assert!(
            response.headers()["content-type"]
                .to_str()
                .expect("type")
                .starts_with(content_type)
        );
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(response.headers()["vary"], "Accept");
        let bytes = axum::body::to_bytes(response.into_body(), 100_000)
            .await
            .expect("body");
        let body = String::from_utf8(bytes.to_vec()).expect("text");
        if status == 403 {
            assert!(body.contains("authz.denied"));
            assert!(!body.contains("<form"));
        }
    }
    assert_eq!(dump(&host), before, "rendering never executes an action");
}

#[allow(
    clippy::disallowed_methods,
    reason = "acceptance invokes real CLI and browser processes"
)]
async fn process(program: String, args: Vec<String>, input: Value) -> (Value, bool) {
    tokio::task::spawn_blocking(move || {
        use std::io::Write;
        let mut child = std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("transport process");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(input.to_string().as_bytes())
            .expect("input");
        let output = child.wait_with_output().expect("process exit");
        (
            serde_json::from_slice(&output.stdout).expect("transport JSON"),
            !output.status.success(),
        )
    })
    .await
    .expect("process worker")
}

#[derive(Clone, Copy)]
struct Metadata<'a> {
    org: &'a str,
    environment: &'a str,
    version: u64,
}
const METADATA: Metadata<'static> = Metadata {
    org: "org_surfaces",
    environment: "dev",
    version: 1,
};

async fn invoke(surface: &str, url: &str, actor: &str, input: Value, key: &str) -> (Value, bool) {
    invoke_with_metadata(surface, url, actor, input, key, METADATA).await
}

async fn invoke_with_metadata(
    surface: &str,
    url: &str,
    actor: &str,
    input: Value,
    key: &str,
    metadata: Metadata<'_>,
) -> (Value, bool) {
    let request =
        json!({"actionId":"demo.melt.record", "version":metadata.version, "input":input, "idempotencyKey":key});
    if surface == "cli" {
        let args = [
            "remote",
            "--url",
            &format!("{url}/mcp"),
            "--org",
            metadata.org,
            "--env",
            metadata.environment,
            "--dev-actor",
            actor,
            "call",
            "call_action",
            "--idempotency-key",
            key,
        ]
        .map(str::to_owned)
        .to_vec();
        let (reply, failed) =
            process(std::env::var("NORN_CLI").expect("NORN_CLI"), args, request).await;
        return (reply["result"]["structuredContent"].clone(), failed);
    }
    let driver = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages/norn-surface-acceptance/transport.mjs");
    let (reply, failed) = process(
        std::env::var("NORN_NODE").expect("NORN_NODE"), vec![driver.display().to_string()],
        json!({"surface":surface,"url":url,"actor":actor,"actionId":"demo.melt.record","input":input,"idempotencyKey":key,"org":metadata.org,"environment":metadata.environment,"version":metadata.version}),
    ).await;
    assert!(!failed, "transport driver process failed");
    let failed = reply["status"].as_u64().expect("HTTP status") >= 400;
    if surface == "mcp" && !failed {
        (
            reply["body"]["result"]["structuredContent"].clone(),
            reply["body"]["result"]["isError"] == true,
        )
    } else {
        (reply["body"].clone(), failed)
    }
}

#[tokio::test]
#[ignore = "explicit demo acceptance requires compiled CLI, authored definitions and Chromium"]
async fn equivalent_governed_surfaces() {
    let source = std::fs::read_to_string(
        std::env::var("NORN_SURFACE_DEFINITIONS").expect("authored definitions path"),
    )
    .expect("authored fixture");
    let mut results = Vec::new();
    let mut states = Vec::new();
    for surface in ["http", "mcp", "cli", "ui"] {
        let host = Arc::new(
            DevHost::new(
                NativeEngine::open_in_memory().expect("fresh engine"),
                &source,
                CellScope {
                    org_id: "org_surfaces".into(),
                    environment_id: "dev".into(),
                },
                LogicalTime::EPOCH,
            )
            .expect("admitted fresh tenant")
            .with_authorizer(Demo),
        );
        let definitions = crate::routes::get_defs(
            &host,
            &crate::routes::DefinitionsQuery {
                org_id: Some("org_surfaces".into()),
                environment_id: Some("dev".into()),
                version: None,
            },
            Some("demo"),
        )
        .expect("authorized retained document");
        let law = definitions
            .artifacts
            .iter()
            .map(|artifact| serde_json::from_str::<Value>(artifact.get()).expect("artifact"))
            .find(|artifact| artifact["artifactId"] == "demo.fine_mass_conservation.law")
            .expect("retained governing law");
        assert_eq!(law["payload"]["response"]["kind"], "invariant");
        assert_eq!(
            law["payload"]["response"]["objectType"],
            "demo.MeltMeasurement"
        );
        assert_eq!(
            law["payload"]["response"]["invariantId"],
            "fine_mass_conservation"
        );
        let app = router(Arc::clone(&host));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback");
        let url = format!("http://{}", listener.local_addr().expect("address"));
        let server_app = app.clone();
        let server =
            tokio::spawn(async move { axum::serve(listener, server_app).await.expect("serve") });
        let input = json!({"id":"melt-1","input_fine_g":"100.000","output_fine_g":"99.200","loss_fine_g":"0.800"});
        let (accepted, failed) = invoke(surface, &url, "demo", input.clone(), "melt").await;
        assert!(!failed, "{surface}: {accepted}");
        assert_eq!(accepted["replayed"], false);
        assert_eq!(accepted["record"]["actionId"], "demo.melt.record");
        assert_eq!(accepted["record"]["actor"], "demo");
        let measurement = host
            .cell()
            .expect("cell")
            .store()
            .read()
            .object("main", "demo.MeltMeasurement", "melt-1")
            .expect("read")
            .expect("committed measurement");
        for (property, number) in [
            ("input_fine_g", "100"),
            ("output_fine_g", "99.2"),
            ("loss_fine_g", "0.8"),
        ] {
            assert_eq!(
                measurement.values.get(property),
                Some(&norn_ir::RuntimeValue::from_json_text(number).expect("exact mass"))
            );
        }
        let before = dump(&host);
        for retry_surface in ["http", "mcp", "cli", "ui"] {
            let (replayed, failed) =
                invoke(retry_surface, &url, "demo", input.clone(), "melt").await;
            assert!(!failed, "retry through {retry_surface}: {replayed}");
            assert_eq!(replayed["replayed"], true);
            assert_eq!(replayed["record"], accepted["record"]);
            assert_eq!(dump(&host), before, "retry commits nothing");
        }
        let (denied, failed) = invoke(surface, &url, "forbidden",
            json!({"id":"melt-denied","input_fine_g":"100.000","output_fine_g":"99.200","loss_fine_g":"0.800"}), "denied").await;
        assert!(failed, "{surface}: {denied}");
        assert_eq!(denied["code"], "authz.denied");
        assert_eq!(denied["category"], "permission");
        assert_eq!(dump(&host), before, "permission refusal changes no state");
        let ledger_before = host
            .cell()
            .expect("cell")
            .store()
            .read()
            .ledger(0)
            .expect("ledger");
        let (law, failed) = invoke( surface, &url, "demo",
            json!({"id":"melt-bad","input_fine_g":"100.000","output_fine_g":"99.200","loss_fine_g":"0.000"}), "omitted-loss").await;
        assert!(failed, "{surface}: {law}");
        assert_eq!(law["code"], "core.laws.violated");
        assert_eq!(law["category"], "precondition");
        let after = dump(&host);
        assert_eq!(
            before.keys().collect::<Vec<_>>(),
            after.keys().collect::<Vec<_>>()
        );
        for (table, rows) in &before {
            let next = after.get(table).expect("same tables");
            match table.as_str() {
                "_norn_ledger"
                | "_norn_receipt"
                | "_norn_export_journal"
                | "_norn_export_payload" => {
                    assert_eq!(
                        next.len(),
                        rows.len() + 1,
                        "one refusal audit append: {table}"
                    );
                    assert!(
                        rows.iter().all(|row| next.contains(row)),
                        "audit history retained: {table}"
                    );
                }
                "_norn_clock" => assert_eq!(
                    next.len(),
                    rows.len(),
                    "audit advances the existing clock pointer"
                ),
                _ => assert_eq!(next, rows, "law refusal changes no business state: {table}"),
            }
        }
        let ledger_after = host
            .cell()
            .expect("cell")
            .store()
            .read()
            .ledger(0)
            .expect("ledger");
        assert_eq!(
            ledger_after.len(),
            ledger_before.len() + 1,
            "one refusal audit event"
        );
        let event = &ledger_after.last().expect("refusal event").event;
        assert_eq!(
            event.get("op").and_then(norn_ir::RuntimeValue::as_str),
            Some("invariant_refused")
        );
        assert_eq!(
            event.get("id").and_then(norn_ir::RuntimeValue::as_str),
            Some("fine_mass_conservation")
        );
        let owner = event.get("owner").expect("inherited invariant owner");
        assert_eq!(
            owner.get("kind").and_then(norn_ir::RuntimeValue::as_str),
            Some("interface")
        );
        assert_eq!(
            owner.get("name").and_then(norn_ir::RuntimeValue::as_str),
            Some("demo.ConservedFineMass")
        );
        results.push((accepted, denied, law));
        states.push(dump(&host));
        server.abort();
    }
    for pair in results.windows(2) {
        assert_eq!(pair[0], pair[1], "complete records and refusal envelopes");
    }
    for pair in states.windows(2) {
        assert_eq!(pair[0], pair[1], "complete committed tenant state");
    }
}


/// SURFACE-ACTION-2 / SURFACE-DENY-1/2: metadata and retry state remain server-owned.
#[tokio::test]
#[ignore = "explicit transport acceptance requires compiled CLI and authored definitions"]
async fn transport_metadata_and_refusal_key_conformance() {
    let source = std::fs::read_to_string(
        std::env::var("NORN_SURFACE_DEFINITIONS").expect("authored definitions path"),
    ).expect("authored fixture");
    let mut results = Vec::new();
    let mut states = Vec::new();
    for surface in ["http", "mcp", "cli"] {
        let host = Arc::new(DevHost::new(
            NativeEngine::open_in_memory().expect("fresh engine"),
            &source,
            CellScope { org_id: "org_surfaces".into(), environment_id: "dev".into() },
            LogicalTime::EPOCH,
        ).expect("admitted fresh tenant").with_authorizer(Demo));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("loopback");
        let url = format!("http://{}", listener.local_addr().expect("address"));
        let app = router(Arc::clone(&host));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
        let input = json!({"id":"metadata-1","input_fine_g":"100.000","output_fine_g":"99.200","loss_fine_g":"0.800"});
        let initial = dump(&host);
        let mut refusals = Vec::new();
        // Every rejected attempt uses the key that must still commit below.
        for (metadata, actor, code) in [
            (Metadata { org: "other", ..METADATA }, "demo", "scope.denied"),
            (Metadata { environment: "other", ..METADATA }, "demo", "scope.denied"),
            (Metadata { version: 0, ..METADATA }, "demo", "defs.stale"),
            (METADATA, "forbidden", "authz.denied"),
        ] {
            let (refusal, failed) = invoke_with_metadata(surface, &url, actor, input.clone(), "metadata", metadata).await;
            assert!(failed, "{surface}: {refusal}");
            assert_eq!(refusal["code"], code);
            assert_eq!(dump(&host), initial, "{code} consumes no successful-call key or effects");
            refusals.push(refusal);
        }
        let (accepted, failed) = invoke(surface, &url, "demo", input.clone(), "metadata").await;
        assert!(!failed, "{surface}: {accepted}");
        assert_eq!(accepted["replayed"], false);
        assert_eq!(accepted["record"]["actor"], "demo");
        assert_eq!(accepted["record"]["actionArtifactId"], "demo.melt.record.action");
        assert_eq!(accepted["record"]["actionVersion"], "0.1.0");
        assert_eq!(accepted["record"]["input"], input);
        let committed = dump(&host);
        let mut changed = input.clone();
        changed["id"] = json!("different-intent");
        for (metadata, actor, args, code) in [
            (METADATA, "forbidden", input.clone(), "authz.denied"),
            (METADATA, "peer", input.clone(), "action.idempotency_conflict"),
            (Metadata { version: 0, ..METADATA }, "demo", input.clone(), "defs.stale"),
            (METADATA, "demo", changed, "action.idempotency_conflict"),
        ] {
            let (refusal, failed) = invoke_with_metadata(surface, &url, actor, args, "metadata", metadata).await;
            assert!(failed, "{surface}: {refusal}");
            assert_eq!(refusal["code"], code);
            assert!(refusal.get("record").is_none(), "refusal discloses no success receipt");
            assert_eq!(dump(&host), committed, "{code} mutates no persisted table");
            refusals.push(refusal);
        }
        for retry in ["http", "mcp", "cli"] {
            let (replayed, failed) = invoke(retry, &url, "demo", input.clone(), "metadata").await;
            assert!(!failed, "{retry}: {replayed}");
            assert_eq!(replayed["replayed"], true);
            assert_eq!(replayed["record"], accepted["record"]);
            assert_eq!(dump(&host), committed, "authorized retry adds no consequence");
        }
        results.push((accepted, refusals));
        states.push(committed);
        server.abort();
    }
    for pair in results.windows(2) {
        assert_eq!(pair[0], pair[1], "complete receipts and refusal envelopes agree");
    }
    for pair in states.windows(2) {
        assert_eq!(pair[0], pair[1], "complete committed tenant state agrees");
    }
}
