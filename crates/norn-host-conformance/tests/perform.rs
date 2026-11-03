//! HOST-CONFORMANCE-1 / ANALYTICS-FUNCTION-2: real persisted native perform receipts.
#![cfg(target_os = "linux")]
#![allow(
    clippy::expect_used,
    clippy::panic,
    missing_docs,
    reason = "native host correspondence"
)]
use norn_ir::{
    RuntimeValue,
    function::{FunctionInvocation, FunctionOutcome, FunctionReceipt},
};
use norn_metrics::temporal::{TemporalPlan, TemporalRow};
use norn_metrics::{host::ForecastSummary, performer::perform_forecast, producer::ForecastExport};

use norn_cell::{
    CellStore, CommitBatch, EffectRow, Model, OutboxCommand, SqliteCell, StoreOptions,
};
use norn_kernel::{CellHost, Command, CommandId, FreshnessPolicy, Generation, IdempotencyKey};
fn export<'a>(
    plan: &'a norn_metrics::temporal::TemporalPlan,
    rows: &'a [norn_metrics::temporal::TemporalRow],
    digest: &norn_ir::function::Digest,
    version: u64,
) -> ForecastExport<'a> {
    let tenant = "tenant_acme";
    ForecastExport {
        tenant,
        world: "main",
        plan,
        rows,
        version,
        source_digest: digest.clone(),
        source_head: 1,
        as_of: 8_000_000,
        complete: true,
        quarantine_count: 0,
        fresh: true,
        assumptions_digest: digest.clone(),
        remedy_digest: digest.clone(),
        frontier_age_ms: 0,
    }
}
fn enqueue(invocation: &FunctionInvocation, path: &std::path::Path) -> CellStore {
    let model = Model::from_definitions(&[], &[]).expect("model");
    let mut store = CellStore::open(path, model, StoreOptions::default()).expect("store");
    let command = Command {
        id: CommandId(invocation.command_id.clone()),
        generation: Generation(invocation.run.generation),
        idempotency_key: IdempotencyKey("native-forecast".into()),
        freshness: FreshnessPolicy::AppendOnly,
        deadline: None,
        payload: norn_ir::runtime_object! { "type" => "norn.function/v1", "invocation" => norn_ir::runtime::encode(&invocation).expect("invocation") },
    };
    store
        .commit(&CommitBatch {
            world: "main".into(),
            scope: invocation.tenant.clone(),
            logical_time: 10_000,
            effects: vec![EffectRow::Enqueue {
                command: OutboxCommand {
                    command_id: command.id.0.clone(),
                    generation: command.generation.0,
                    idempotency_key: command.idempotency_key.0.clone(),
                    deadline: None,
                    command: norn_ir::runtime::encode(&command).expect("command"),
                },
            }],
            ..CommitBatch::default()
        })
        .expect("enqueue");
    store
}
proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(16))]
    #[test]
    fn native_report_is_recorded_by_perform_and_wrong_pins_refuse(version in 1u64..1_000_000) {
        let (plan, rows) = fixture();
        let digest = norn_metrics::host::implementation_digest().expect("code");
        let tenant = "tenant_acme";
        let admitted = norn_checker::function::admit_function_manifest(
            &norn_metrics::host::manifest().expect("manifest"),
        )
        .expect("admit");
        let mut invocation: FunctionInvocation = serde_json::from_str(include_str!(
            "../../../fixtures/function/invocation.effectful.json"
        ))
        .expect("dispatch fixture");
        let (input, blob) = export(&plan, &rows, &digest, version).input(6_000_000).expect("export");
        invocation.manifest = admitted.digest.clone();
        invocation.retry.max_attempts = 1;
        invocation.provenance_commit = 1;
        invocation.inputs = norn_ir::runtime::encode(&input)
            .expect("input")
            .as_object()
            .expect("object")
            .clone();
        invocation.blobs = [("table".into(), blob)].into();
        let mut wrong = invocation.clone();
        wrong.tenant = "another-tenant".into();
        assert!(perform_forecast(&wrong, &export(&plan, &rows, &digest, version), "unit-host", 10_000).is_err());
        wrong = invocation.clone();
        wrong.inputs.insert("tableVersion".into(), norn_ir::runtime::encode(&version.saturating_add(1)).expect("bounded version"));
        assert!(perform_forecast(&wrong, &export(&plan, &rows, &digest, version), "unit-host", 10_000).is_err());
        wrong = invocation.clone();
        wrong.blobs.clear();
        assert!(perform_forecast(&wrong, &export(&plan, &rows, &digest, version), "unit-host", 10_000).is_err());
        let directory = tempfile::tempdir().expect("store directory");
        let path = directory.path().join("function.sqlite");
        let store = enqueue(&invocation, &path);
        let mut cell: SqliteCell<RuntimeValue, RuntimeValue, FunctionReceipt> =
            SqliteCell::new(store, tenant, move |command: &Command<RuntimeValue>| {
                let actual: FunctionInvocation = norn_ir::runtime::decode(
                    command.payload.get("invocation").expect("host envelope"),
                )
                .expect("invocation");
                let export = export(&plan, &rows, &digest, version);
                perform_forecast(&actual, &export, "unit-host", 10_000).expect("real execution")
            })
            .expect("cell");
        let performed = cell.perform().expect("perform").expect("result");
        assert!(
            norn_checker::function::check_function_receipt(
                &performed.input,
                &invocation,
                &admitted
            )
            .is_empty()
        );
        let recorded: FunctionReceipt = norn_ir::runtime::decode(
            &cell
                .store()
                .read()
                .head_command()
                .expect("head")
                .expect("command")
                .result
                .expect("persisted result"),
        )
        .expect("receipt");
        assert_eq!(recorded, performed.input);
        assert!(recorded.resources.peak_memory_bytes > 0 && recorded.resources.fuel.is_none());
        let FunctionOutcome::Success(success) = recorded.outcome else {
            panic!("native forecast success")
        };
        let summary: ForecastSummary =
            norn_ir::runtime::decode(&RuntimeValue::Object(success.summary))
                .expect("typed summary");
        assert_eq!(summary.rmse_nanos, Some(800_000_000));
        assert_eq!(summary.table_digest, input.table_digest);
        assert_eq!(summary.table_version, version.to_string());
        drop(cell);
        let store = CellStore::open(&path, Model::from_definitions(&[], &[]).expect("model"), StoreOptions::default()).expect("reopen recorded result");
        let mut restored: SqliteCell<RuntimeValue, RuntimeValue, FunctionReceipt> = SqliteCell::new(store, tenant, |_: &Command<RuntimeValue>| -> FunctionReceipt { panic!("recorded results must replay without execution") }).expect("restored cell");
        let replayed = restored.perform().expect("recorded retry").expect("redelivery");
        assert_eq!(replayed.input, performed.input);
    }
}

#[allow(clippy::arithmetic_side_effects, reason = "bounded numerical fixture")]
fn fixture() -> (TemporalPlan, Vec<TemporalRow>) {
    use norn_metrics::temporal::{TemporalRow, TemporalValue};
    let root = std::env::var_os("NORN_UPSTREAM_FIXTURE_DIR").expect("typed fixtures");
    let artifacts: Vec<norn_ir::ArtifactEnvelope> = serde_json::from_slice(
        &std::fs::read(std::path::Path::new(&root).join("metric-demand.json")).expect("fixture"),
    )
    .expect("artifacts");
    let catalog = norn_checker::EffectiveCatalog::from_artifacts(&artifacts);
    let plan = TemporalPlan::compile(&catalog, "demo.Demand", &["stock".into()], "units", 0)
        .expect("plan");
    let value = |n: i32, at| TemporalValue {
        value: norn_ir::RuntimeValue::from_json_text(n.to_string()).expect("number"),
        valid_at: at,
        recorded_at: at,
        source: format!("unit:{n}:{at}"),
    };
    let rows: Vec<_> = [4, 4, 8, 8, 12, 14, 16]
        .into_iter()
        .enumerate()
        .map(|(i, y)| {
            let x = i32::try_from(i).expect("index") + 1;
            let at = i64::from(x) * 1_000_000;
            TemporalRow {
                key: format!("unit-{i}"),
                at,
                features: vec![value(x, at)],
                outcome: value(y, at + 1000),
            }
        })
        .collect();
    (plan, rows)
}

/// A previous process peak conservatively refuses a later, small invocation.
#[test]
#[allow(
    clippy::disallowed_methods,
    reason = "test capability: launch only this RSS scenario in its own process"
)]
fn prior_process_rss_peak_refuses_small_forecast() {
    const CHILD: &str = "NORN_RSS_REFUSAL_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let status = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "prior_process_rss_peak_refuses_small_forecast",
                "--test-threads=1",
            ])
            .env(CHILD, "1")
            .status()
            .expect("bounded RSS child");
        assert!(status.success());
        return;
    }
    // Touch actual pages, then release them before the forecast begins.
    let memory = vec![7u8; 300 * 1024 * 1024];
    std::hint::black_box(&memory);
    drop(memory);
    let (plan, rows) = fixture();
    let digest = norn_metrics::host::implementation_digest().expect("code");
    let admitted = norn_checker::function::admit_function_manifest(
        &norn_metrics::host::manifest().expect("manifest"),
    )
    .expect("admit");
    assert_eq!(admitted.manifest.bounds.max_memory_bytes, Some(268_435_456));
    let mut invocation: FunctionInvocation = serde_json::from_str(include_str!(
        "../../../fixtures/function/invocation.effectful.json"
    ))
    .expect("invocation");
    let export = export(&plan, &rows, &digest, 1);
    let (input, blob) = export.input(6_000_000).expect("export");
    invocation.manifest = admitted.digest.clone();
    invocation.retry.max_attempts = 1;
    invocation.inputs = norn_ir::runtime::encode(&input)
        .expect("input")
        .as_object()
        .expect("object")
        .clone();
    invocation.blobs = [("table".into(), blob)].into();
    let receipt =
        perform_forecast(&invocation, &export, "unit-host", 10_000).expect("metered receipt");
    assert!(receipt.resources.peak_memory_bytes > 268_435_456);
    assert!(matches!(
        receipt.outcome,
        FunctionOutcome::ResourceExhausted {
            resource: norn_ir::function::ExhaustedResource::Memory
        }
    ));
    assert!(
        norn_checker::function::check_function_receipt(&receipt, &invocation, &admitted).is_empty()
    );
}
