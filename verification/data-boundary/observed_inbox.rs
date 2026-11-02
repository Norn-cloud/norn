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
    let (definitions, model) = demo_measurement_definitions();
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

fn demo_measurement_definitions() -> (String, Model) {
    use norn_ir::{ArtifactEnvelope, EntityRoot};
    let artifacts: Vec<ArtifactEnvelope> = [
        include_str!("../../registry/core/artifacts/core.value_types.entity.json"),
        include_str!("../../registry/core/artifacts/std.value_types.entity.json"),
        include_str!("../../registry/core/artifacts/core.Agent.interface.json"),
        include_str!("../../registry/core/artifacts/core.AgentRole.interface.json"),
        include_str!("../../registry/core/artifacts/core.Resource.interface.json"),
        include_str!("../../registry/core/artifacts/core.EconomicEvent.interface.json"),
        include_str!("../../registry/core/artifacts/core.Commitment.interface.json"),
        include_str!("../../registry/core/artifacts/core.Hold.interface.json"),
        include_str!("../../registry/core/artifacts/core.LedgerEffect.interface.json"),
        include_str!("../../registry/core/artifacts/core.Claim.interface.json"),
        include_str!("../../registry/packs/demo-erpnext/artifacts/demo.erpnext.StockEntryMeasurement.entity.json"),
        include_str!("../../registry/packs/demo-erpnext/artifacts/demo.erpnext.Measurement.interface.json"),
        include_str!("../../registry/packs/demo-erpnext/artifacts/demo.erpnext.classifications.entity.json"),
        include_str!("../../registry/packs/demo-erpnext/artifacts/demo.erpnext.purity.law.json"),
        include_str!("../../registry/packs/demo-erpnext/artifacts/demo.erpnext.fine_mass_definition.law.json"),
        include_str!("../../registry/packs/demo-erpnext/artifacts/demo.erpnext.measurements.source.json"),
        include_str!("../../registry/packs/demo-erpnext/artifacts/demo.erpnext.measurements.mapping.json"),
    ].into_iter().map(|bytes| {
        let value = RuntimeValue::from_json_text(bytes).expect("compiled artifact source");
        norn_ir::runtime::decode(&value).expect("typed artifact")
    }).collect();
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
    (definitions, model)
}

fn checked_raw(raw: &norn_dataplane::RawObservation) -> Result<Observed<RuntimeValue>, Refusal> {
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
}

// DATA-FEED-REPLAY-1: authenticated capture is evidence; only inbox processing promotes it.
#[test]
fn signed_webhook_and_missed_delivery_sweep_replay_pack_law_quarantine_and_recovery() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use hmac::{Hmac, Mac};
    use norn_dataplane::{
        frappe::{FrappeSource, ingest_frappe_webhook},
        sweep::{FrappeSweep, SweepFeed},
    };
    use sha2::Sha256;
    let (definitions, model) = demo_measurement_definitions();
    let scope = CellScope {
        org_id: "org_demo".into(),
        environment_id: "dev".into(),
    };
    let directory = std::path::PathBuf::from(std::env::var("NORN_TEST_SCRATCH").expect("scratch"))
        .with_extension("replay");
    std::fs::create_dir_all(&directory).expect("directory");
    let path = directory.join("feed.sqlite");
    let host = DevHost::new(
        NativeEngine::open(&path, StoreOptions::default()).expect("engine"),
        &definitions,
        scope.clone(),
        LogicalTime::from_millis(1793527260000),
    )
    .expect("host");
    let source = FrappeSource {
        source: "synthetic-erpnext".into(),
        redaction: None,
    };
    let input = norn_cell::mapping_bundle::MappingInput {
        mapping: "demo.erpnext.measurements.mapping".into(),
        dataset: "Stock Entry Measurement".into(),
        connector_instance: "demo".into(),
        partition: "measurements".into(),
        recorded_at: norn_ir::provenance::UtcTimestamp("2026-11-01T10:01:00Z".into()),
    };
    let body = br#"{"event":"on_update","doc":{"doctype":"Stock Entry Measurement","name":"MEAS-1","modified":"2026-11-01 10:00:00","parent":"SYN-SE-1","custom_metal":"gold","custom_role":"input","custom_gross_grams":60000,"custom_purity":1.20,"custom_fine_grams":72000,"custom_occurred_at":"2026-10-26T10:00:00Z","custom_lot":"SYN-LOT-1"}}"#;
    let secret = b"synthetic-test-only-webhook-key";
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("fixture key");
    mac.update(body);
    let signature = STANDARD.encode(mac.finalize().into_bytes());
    assert!(
        host.observe_mapped_submission(&scope, &input, || {
            ingest_frappe_webhook(
                &source,
                secret,
                body,
                Some("invalid signature"),
                1793527260000,
            )
            .map_err(Refusal::host)
            .and_then(|raw| checked_raw(&raw))
        })
        .is_err()
    );
    let store = CellStore::open(&path, model.clone(), StoreOptions::default()).expect("read");
    assert!(
        store
            .read()
            .pending_observations()
            .expect("no rejected capture")
            .is_empty()
    );
    drop(store);
    let raw = ingest_frappe_webhook(&source, secret, body, Some(&signature), 1793527260000)
        .expect("authenticated adapter");
    let (id, inserted) = host
        .observe_mapped_submission(&scope, &input, || checked_raw(&raw))
        .expect("capture");
    assert!(inserted);
    for _ in 0..3 {
        assert_eq!(
            host.observe_mapped_submission(&scope, &input, || checked_raw(&raw))
                .expect("webhook redelivery"),
            (id.clone(), false)
        );
    }
    let mut cell = norn_cell::SqliteCell::<
        norn_kernel::ObservationEvent,
        norn_ir::RuntimeObject,
        norn_ir::RuntimeObject,
    >::open(
        &path,
        model.clone(),
        StoreOptions::default(),
        "org_demo/dev",
        |_| std::collections::BTreeMap::new(),
    )
    .expect("driver");
    assert!(
        cell.store()
            .read()
            .object(
                norn_cell::MAIN_WORLD,
                "demo.erpnext.StockEntryMeasurement",
                "MEAS-1"
            )
            .expect("capture has no write authority")
            .is_none()
    );
    assert_eq!(
        cell.store()
            .read()
            .pending_observations()
            .expect("one complete bundle")
            .len(),
        1
    );
    assert!(cell.process_mapping_bundle(&id).expect("process inbox"));
    assert!(
        !cell
            .process_mapping_bundle(&id)
            .expect("processed redelivery")
    );
    let before = cell
        .store()
        .read()
        .observation_findings(
            norn_cell::MAIN_WORLD,
            "demo.erpnext.StockEntryMeasurement",
            "MEAS-1",
        )
        .expect("purity finding");
    assert_eq!(before.len(), 1);
    assert!(
        matches!(&before[0].rule_ref, norn_ir::DataQualityRuleRef::Law {law_id,..} if law_id=="demo.erpnext.purity_in_range")
    );
    assert_eq!(before[0].status, norn_ir::DataQualityFindingStatus::Open);
    let page = cell
        .store()
        .read()
        .authoritative_page(
            norn_cell::MAIN_WORLD,
            "demo.erpnext.StockEntryMeasurement",
            None,
            10,
        )
        .expect("quarantine population");
    assert!(page.records.is_empty());
    assert_eq!(page.quarantine_count, 1);
    drop(cell);
    // The correction webhook was missed: hydrate the full document through the ingress sweep.
    let sweep = FrappeSweep {
        source,
        doctype: "Stock Entry Measurement".into(),
        page_size: 10,
        lookback_secs: 60,
    };
    let command = sweep.start_cycle(SweepFeed::Documents, None).expect("plan");
    let listing = br#"{"data":[{"name":"MEAS-1","modified":"2026-11-01 10:02:00"}]}"#;
    assert_eq!(
        sweep
            .plan_documents(&command, listing)
            .expect("full document request")
            .len(),
        1
    );
    let document = br#"{"message":{"doctype":"Stock Entry Measurement","name":"MEAS-1","modified":"2026-11-01 10:02:00","parent":"SYN-SE-1","custom_metal":"gold","custom_role":"input","custom_gross_grams":60000,"custom_purity":0.90,"custom_fine_grams":54000,"custom_occurred_at":"2026-10-26T10:00:00Z","custom_lot":"SYN-LOT-1"}}"#.to_vec();
    let page = sweep
        .ingest(&command, listing, &[document.clone()], 1793527380000)
        .expect("missed delivery evidence");
    assert!(page.exhausted && page.next.is_some());
    assert_eq!(page.observations.len(), 1);
    let checkpoint = page.next.clone().expect("source high-water cursor");
    let correction = &page.observations[0];
    let mut correction_input = input.clone();
    correction_input.recorded_at = norn_ir::provenance::UtcTimestamp("2026-11-01T10:03:00Z".into());
    let (fixed, inserted) = host
        .observe_mapped_submission(&scope, &correction_input, || checked_raw(correction))
        .expect("capture correction");
    assert!(inserted);
    assert_ne!(fixed, id);
    assert_eq!(
        host.observe_mapped_submission(&scope, &correction_input, || checked_raw(correction))
            .expect("sweep replay"),
        (fixed.clone(), false)
    );
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
    .expect("restart");
    assert!(
        cell.process_mapping_bundle(&fixed)
            .expect("governed recovery")
    );
    let after = cell
        .store()
        .read()
        .observation_findings(
            norn_cell::MAIN_WORLD,
            "demo.erpnext.StockEntryMeasurement",
            "MEAS-1",
        )
        .expect("retained finding");
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].id, before[0].id);
    assert_eq!(after[0].status, norn_ir::DataQualityFindingStatus::Resolved);
    let record = cell
        .store()
        .read()
        .authoritative_object(
            norn_cell::MAIN_WORLD,
            "demo.erpnext.StockEntryMeasurement",
            "MEAS-1",
        )
        .expect("eligible again")
        .expect("record");
    assert_eq!(record.version, 2);
    assert_eq!(
        record.values["purity"],
        RuntimeValue::from_json_text("0.90").expect("exact purity")
    );
    assert!(
        !cell
            .process_mapping_bundle(&fixed)
            .expect("processed sweep replay")
    );
    assert!(
        cell.store()
            .read()
            .pending_observations()
            .expect("consumed evidence")
            .is_empty()
    );
    let page = cell
        .store()
        .read()
        .authoritative_page(
            norn_cell::MAIN_WORLD,
            "demo.erpnext.StockEntryMeasurement",
            None,
            10,
        )
        .expect("aggregate population");
    assert_eq!(page.records.len(), 1);
    assert_eq!(page.quarantine_count, 0);
    // Replay the ingress sweep itself after recovery, using the same sealed capture context.
    use norn_cell::engine::SqlEngine as _;
    let inspector = NativeEngine::open(&path, StoreOptions::default()).expect("read-only evidence inspection");
    let bundles = || inspector.query_row("SELECT COUNT(*) FROM _norn_inbox WHERE observation_id LIKE 'mapping-bundle:%'", &[], |row| row.get::<i64>(0)).expect("bundle cardinality");
    let before_bundles = bundles();
    assert_eq!(before_bundles, 2);
    let replay_command = sweep.start_cycle(SweepFeed::Documents, Some(&checkpoint)).expect("second sweep cycle");
    assert_eq!(sweep.plan_documents(&replay_command, listing).expect("refetch full document").len(), 1);
    let replay_page = sweep.ingest(&replay_command, listing, &[document], correction.extracted_at).expect("second hydrated cycle");
    assert_eq!(replay_page.observations.len(), 1);
    assert_eq!(&replay_page.observations[0], correction);
    assert_eq!(host.observe_mapped_submission(&scope, &correction_input, || checked_raw(&replay_page.observations[0])).expect("second-cycle capture"), (fixed.clone(), false));
    assert!(!cell.process_mapping_bundle(&fixed).expect("second-cycle processing"));
    assert_eq!(bundles(), before_bundles);
    let replay_findings = cell.store().read().observation_findings(norn_cell::MAIN_WORLD, "demo.erpnext.StockEntryMeasurement", "MEAS-1").expect("finding stability after second cycle");
    assert_eq!(replay_findings, after);
    assert_eq!(cell.store().read().authoritative_object(norn_cell::MAIN_WORLD, "demo.erpnext.StockEntryMeasurement", "MEAS-1").expect("canonical version stability").expect("record").version, record.version);
    drop(inspector);
    // The planned cursor is not persisted by this cut; no full cursor-convergence claim.
    drop(cell);
    drop(host);
    std::fs::remove_dir_all(directory).expect("cleanup");
}
