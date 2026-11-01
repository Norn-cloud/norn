//! OBS-EVIDENCE-1: external-consumer conformance for the checked adapter/inbox port.
use norn_api::{host::DevHost, observe::ObservationStream, refusal::Refusal, scope::CellScope};
use norn_cell::{CellStore, Model, StoreOptions, engine::NativeEngine};
use norn_dataplane::ObservationDraft;
use norn_ir::RuntimeValue;
use norn_kernel::{LogicalTime, ObservationId, Observed, Provenance};

#[test]
fn dataplane_submission_enters_the_cell_only_through_observed_inbox() {
    let raw = ObservationDraft::from_json_slice(
        br#"{
        "source":"synthetic-erpnext", "source_id":"Stock Entry Measurement/MEAS-1",
        "event_id":"MEAS-1@1", "kind":"upsert", "source_version":"1",
        "payload":{"name":"MEAS-1", "custom_purity":1.20},
        "extracted_at":1793527260000, "pipeline_version":"synthetic-feed/1"
    }"#,
    )
    .expect("exact draft")
    .seal(None)
    .expect("sealed observation");
    let scope = CellScope {
        org_id: "org_demo".into(),
        environment_id: "dev".into(),
    };
    let directory =
        std::path::PathBuf::from(std::env::var("NORN_TEST_SCRATCH").expect("isolated scratch"));
    std::fs::create_dir_all(&directory).expect("directory");
    let path = directory.join("inbox.sqlite");
    let host = DevHost::new(
        NativeEngine::open(&path, StoreOptions::default()).expect("engine"),
        "[]",
        scope.clone(),
        LogicalTime::from_millis(raw.extracted_at),
    )
    .expect("admitted host");
    let model = Model::from_definitions(&[], &[]).expect("empty model");
    let read = || CellStore::open(&path, model.clone(), StoreOptions::default()).expect("reader");
    let before = read().read().ledger_page(0, 10).expect("ledger");
    let checked = || {
        let submission = raw.submission().map_err(Refusal::host)?;
        Ok(Observed {
            id: ObservationId(raw.observation_id()),
            provenance: Provenance {
                source: submission.source,
                source_id: submission.source_id,
                mapping_version: submission.mapping_version,
                observed_at: LogicalTime::from_millis(raw.extracted_at),
            },
            input: RuntimeValue::from_json_text(&submission.payload.to_string())
                .map_err(Refusal::host)?,
        })
    };
    let stream = ObservationStream {connector_instance:"erp-demo/a", partition:"measurements"};
    let called = std::cell::Cell::new(false);
    let other = CellScope {
        org_id: "other_org".into(),
        ..scope.clone()
    };
    assert!(
        host.observe_submission(&other, stream, || {
            called.set(true);
            checked()
        })
        .is_err()
    );
    assert!(!called.get(), "scope refuses before the adapter runs");
    assert!(
        read()
            .read()
            .pending_observations()
            .expect("inbox")
            .is_empty()
    );
    assert!(
        host.observe_submission(&scope, stream, checked)
            .expect("submission")
    );
    assert!(
        !host
            .observe_submission(&scope, stream, checked)
            .expect("redelivery")
    );
    assert!(
        host.observe_submission(&scope, stream, || Err(Refusal::invalid(
            "invalid transport framing"
        )))
        .is_err()
    );
    assert!(host.observe_submission(&scope, ObservationStream {connector_instance:"erp-demo/b", ..stream}, checked).expect("other connector"));
    assert!(host.observe_submission(&scope, ObservationStream {partition:"other-partition", ..stream}, checked).expect("other partition"));
    let store = read();
    let pending = store.read().pending_observations().expect("inbox");
    assert_eq!(pending.len(), 3);
    let ids: std::collections::BTreeSet<_> = pending.iter().map(|entry| &entry.observation_id).collect();
    assert_eq!(ids.len(), 3);
    assert_ne!(pending[0].observation_id, raw.observation_id());
    let observed: Observed<RuntimeValue> =
        norn_ir::runtime::decode(&pending[0].observed).expect("observed framing");
    assert_eq!(
        observed.input,
        RuntimeValue::from_json_text(&raw.submission().expect("checked").payload.to_string())
            .expect("exact envelope")
    );
    assert_eq!(
        store.read().ledger_page(0, 10).expect("ledger"),
        before,
        "submission cannot commit business facts"
    );
    drop(store);
    drop(host);
    std::fs::remove_dir_all(directory).expect("cleanup");
}
