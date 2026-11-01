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
    let stream = ObservationStream {
        connector_instance: "erp-demo/a",
        partition: "measurements",
    };
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
    assert!(
        host.observe_submission(
            &scope,
            ObservationStream {
                connector_instance: "erp-demo/b",
                ..stream
            },
            checked
        )
        .expect("other connector"),
        "independent connector"
    );
    assert!(
        host.observe_submission(
            &scope,
            ObservationStream {
                partition: "other-partition",
                ..stream
            },
            checked
        )
        .expect("other partition"),
        "independent partition"
    );
    let store = read();
    let pending = store.read().pending_observations().expect("inbox");
    assert_eq!(pending.len(), 3);
    let ids: std::collections::BTreeSet<_> =
        pending.iter().map(|entry| &entry.observation_id).collect();
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

// OBS-EVIDENCE-1: exercise the real host identity boundary over generated trusted streams.
norn_testkit::proptest::proptest! {
    #[test]
    fn source_stream_identity_survives_redelivery(connector in "[a-z%/]{1,12}", partition in "[a-z%/]{1,12}", event in "[a-z%/]{1,12}") {
        let scope = CellScope {org_id:"org_demo".into(), environment_id:"dev".into()};
        let host = DevHost::new(NativeEngine::open_in_memory().expect("engine"), "[]", scope.clone(), LogicalTime::from_millis(1)).expect("host");
        let checked = || Ok(Observed {id:ObservationId(event.clone()), provenance:Provenance {source:"synthetic".into(),source_id:"row".into(),mapping_version:"pipeline/1".into(),observed_at:LogicalTime::from_millis(1)},input:RuntimeValue::Null});
        let stream = ObservationStream {connector_instance:&connector, partition:&partition};
        norn_testkit::proptest::prop_assert!(host.observe_submission(&scope, stream, checked).expect("capture"));
        norn_testkit::proptest::prop_assert!(!host.observe_submission(&scope, stream, checked).expect("replay"));
        norn_testkit::proptest::prop_assert!(host.observe_submission(&scope, ObservationStream {connector_instance:&format!("x{connector}"), ..stream}, checked).expect("other connector"), "independent connector");
        norn_testkit::proptest::prop_assert!(host.observe_submission(&scope, ObservationStream {partition:&format!("x{partition}"), ..stream}, checked).expect("other partition"), "independent partition");
    }
}

#[test]
fn checked_scrap_yield_submission_is_mapped_then_quarantined_by_the_cell() {
    use norn_ir::{ArtifactEnvelope, EntityRoot};
    let mut artifacts = Vec::<ArtifactEnvelope>::new();
    for directory in [
        "registry/core/artifacts",
        "registry/packs/demo-erpnext/artifacts",
    ] {
        for entry in std::fs::read_dir(directory).expect("synthetic declarations") {
            let source = RuntimeValue::from_json_text(
                std::fs::read(entry.expect("entry").path()).expect("artifact bytes"),
            )
            .expect("exact source");
            let artifact: ArtifactEnvelope = norn_ir::runtime::decode(&source).expect("artifact");
            if artifact.artifact_id.starts_with("demo.")
                || [
                    "core.value_types.entity",
                    "std.value_types.entity",
                    "core.Agent.interface",
                    "core.AgentRole.interface",
                    "core.Resource.interface",
                    "core.EconomicEvent.interface",
                    "core.Commitment.interface",
                    "core.Hold.interface",
                    "core.LedgerEffect.interface",
                    "core.Claim.interface",
                ]
                .contains(&artifact.artifact_id.as_str())
            {
                artifacts.push(artifact);
            }
        }
    }
    let diagnostics = norn_checker::check_artifact_set(
        &artifacts,
        &norn_checker::EntityArtifactSetContext {
            registry: &std::collections::BTreeSet::new(),
            base: &[],
            tenant: None,
        },
    );
    assert!(
        !diagnostics.iter().any(norn_checker::Diagnostic::is_error),
        "{diagnostics:#?}"
    );
    let sources: Vec<_> = artifacts
        .iter()
        .map(|artifact| norn_ir::runtime::encode(artifact).expect("source"))
        .collect();
    let definitions = String::from_utf8(
        RuntimeValue::Array(sources)
            .canonical_json()
            .expect("canonical definitions"),
    )
    .expect("UTF8");
    let mut objects = Vec::new();
    let mut interfaces = Vec::new();
    let mut properties = Vec::new();
    for artifact in &artifacts {
        if artifact.kind() != norn_ir::ArtifactKind::Entity {
            continue;
        }
        let source = norn_ir::runtime::encode(artifact).expect("source");
        match norn_ir::runtime::decode::<EntityRoot>(source.get("payload").expect("payload"))
            .expect("root")
        {
            EntityRoot::V2(root) => objects.push(root.object_type),
            EntityRoot::V2Interface(root) => interfaces.push(root.interface),
            EntityRoot::V2PropertyTypes(root) => properties.extend(root.property_types),
            EntityRoot::V2Markings(_) => {}
            EntityRoot::V2Extension(_) | EntityRoot::V1(_) => panic!("unexpected demo profile"),
        }
    }
    let model = Model::from_definitions_with_interfaces(&objects, &properties, &interfaces)
        .expect("model")
        .with_governing_laws(&artifacts)
        .expect("retained laws");
    let raw = ObservationDraft::from_json_slice(br#"{
        "source":"synthetic-erpnext", "source_id":"Stock Entry Measurement/MEAS-1", "event_id":"MEAS-1@1", "kind":"upsert", "source_version":"1",
        "payload":{"name":"MEAS-1","parent":"SYN-SE-1","custom_metal":"gold","custom_role":"input","custom_gross_grams":60000,"custom_purity":1.20,"custom_fine_grams":72000,"custom_occurred_at":"2026-10-26T10:00:00Z","custom_lot":"SYN-LOT-1"},
        "extracted_at":1793527260000,"pipeline_version":"synthetic-feed/1"
    }"#).expect("exact draft").seal(None).expect("sealed raw");
    let scope = CellScope {
        org_id: "org_demo".into(),
        environment_id: "dev".into(),
    };
    let directory = std::path::PathBuf::from(std::env::var("NORN_TEST_SCRATCH").expect("scratch"))
        .with_extension("mapped");
    std::fs::create_dir_all(&directory).expect("directory");
    let path = directory.join("mapped.sqlite");
    let host = DevHost::new(
        NativeEngine::open(&path, StoreOptions::default()).expect("engine"),
        &definitions,
        scope.clone(),
        LogicalTime::from_millis(raw.extracted_at),
    )
    .expect("admitted public host");
    let input = norn_cell::mapping_bundle::MappingInput {
        mapping: "demo.erpnext.measurements.mapping".into(),
        dataset: "Stock Entry Measurement".into(),
        connector_instance: "demo".into(),
        partition: "measurements".into(),
        recorded_at: norn_ir::provenance::UtcTimestamp("2026-11-01T10:01:00Z".into()),
    };
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
    let (id, inserted) = host
        .observe_mapped_submission(&scope, &input, checked)
        .expect("mapped inbox capture");
    assert!(inserted);
    assert_eq!(
        host.observe_mapped_submission(&scope, &input, checked)
            .expect("redelivery"),
        (id.clone(), false)
    );
    let store = CellStore::open(&path, model.clone(), StoreOptions::default()).expect("read");
    assert_eq!(store.read().pending_observations().expect("inbox").len(), 1);
    assert!(
        store
            .read()
            .object(
                norn_cell::MAIN_WORLD,
                "demo.erpnext.StockEntryMeasurement",
                "MEAS-1"
            )
            .expect("no direct write")
            .is_none()
    );
    drop(store);
    drop(host);
    let mut cell = norn_cell::SqliteCell::<
        norn_kernel::ObservationEvent,
        norn_ir::RuntimeObject,
        norn_ir::RuntimeObject,
    >::open(
        &path,
        model,
        StoreOptions::default(),
        "org_demo/dev",
        |_| std::collections::BTreeMap::new(),
    )
    .expect("cell driver");
    assert!(
        cell.process_mapping_bundle(&id)
            .expect("recomputed authority")
    );
    assert!(!cell.process_mapping_bundle(&id).expect("processed replay"));
    let reader = cell.store().read();
    let finding = reader
        .observation_findings(
            norn_cell::MAIN_WORLD,
            "demo.erpnext.StockEntryMeasurement",
            "MEAS-1",
        )
        .expect("findings");
    assert_eq!(finding.len(), 1);
    assert!(
        matches!(&finding[0].rule_ref, norn_ir::DataQualityRuleRef::Law {law_id,..} if law_id=="demo.erpnext.purity_in_range")
    );
    assert!(
        reader
            .object(
                norn_cell::MAIN_WORLD,
                "demo.erpnext.StockEntryMeasurement",
                "MEAS-1"
            )
            .expect("raw")
            .is_some()
    );
    assert!(
        reader
            .authoritative_object(
                norn_cell::MAIN_WORLD,
                "demo.erpnext.StockEntryMeasurement",
                "MEAS-1"
            )
            .expect("authoritative")
            .is_none()
    );
    drop(cell);
    std::fs::remove_dir_all(directory).expect("cleanup");
}
