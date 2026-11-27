//! ASOF-KNOWLEDGE-1: cutoff-bound `AsOfReader` and fail-closed linked `ASOF`
//! regressions.
//!
//! Pins:
//! 1. Historical finding recovery for a `LAW-LINK-1` governing law fails closed
//!    with `core.compute.unsupported_engine` at `K1` instead of consulting live
//!    `CurrentLinks` — both before any `K2` mutation and after target-only
//!    changes, edge replacement, link deletion, and subject delete+recreation.
//! 2. Non-linked (pure property) historical findings still reproduce `K1-before
//!    == K1-after` across subject deletion and recreation through `AsOfReader`.
//! 3. Linked-time (`time_link`) questions fail closed at a knowledge cutoff in
//!    both `evaluate_question_known_at` and `answer_source_disagreements`.

#[cfg(feature = "native")]
#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "ASOF-KNOWLEDGE-1 regression suite"
)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use norn_cell::compute::QueryContext;
    use norn_cell::mapping_bundle::MappingInput;
    use norn_cell::{
        CellError, CellStore, KnowledgeCutoff, MAIN_WORLD, Model, SqliteCell, StoreOptions,
    };
    use norn_checker::entity_scopes::ScopedView;
    use norn_checker::metric::compute::QuestionPlan;
    use norn_checker::metric::derive::derive_metric_space;
    use norn_ir::{
        ArtifactEnvelope, InterfaceDef, LawRoot, ObjectTypeDef, QuestionIR, RuntimeObject,
        RuntimeValue as V,
    };
    use norn_kernel::{
        CellCommit, CellHost as _, FactKey, FactMutation, FactVersion, FactWrite, LinkKey,
        LinkMutation, LinkWrite, LogicalTime, ObservationEvent, ObservationId, Observed,
        Provenance,
    };
    use serde_json::{Value, json};

    type ObsCell = SqliteCell<ObservationEvent, RuntimeObject, RuntimeObject>;

    const SCOPE: &str = "org_demo/dev";
    const CLOCK: u64 = 1_793_702_400_000;

    fn envelope(id: &str, kind: &str, ns: &str, payload: Value, deps: Value) -> ArtifactEnvelope {
        let mut object = json!({
            "irVersion": "0.1", "artifactId": id, "kind": kind, "namespace": ns,
            "name": id, "version": "0.1.0", "lifecycle": "active", "scope": {},
            "provenance": {"source": "system"}
        });
        let map = object.as_object_mut().expect("envelope object");
        map.insert("dependencies".into(), deps);
        map.insert("payload".into(), payload);
        serde_json::from_value(object).expect("artifact envelope")
    }

    fn build_model(artifacts: &[ArtifactEnvelope]) -> Model {
        let mut objects: Vec<ObjectTypeDef> = Vec::new();
        let mut interfaces: Vec<InterfaceDef> = Vec::new();
        for artifact in artifacts {
            let payload = serde_json::to_value(artifact).expect("artifact")["payload"].clone();
            if let Some(ot) = payload.get("objectType") {
                objects.push(serde_json::from_value(ot.clone()).expect("objectType"));
            }
            if let Some(iface) = payload.get("interface") {
                interfaces.push(serde_json::from_value(iface.clone()).expect("interface"));
            }
        }
        Model::from_definitions_with_interfaces(&objects, &[], &interfaces)
            .expect("model")
            .with_governing_laws(artifacts)
            .expect("laws")
    }

    fn open_cell(artifacts: &[ArtifactEnvelope], tag: &str) -> (ObsCell, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(format!("{tag}.sqlite"));
        let model = build_model(artifacts);
        let mut store = CellStore::open(&path, model.clone(), StoreOptions::default()).expect("db");
        store.bind("scope", SCOPE).expect("bind scope");
        store
            .install_definitions(
                model,
                norn_cell::InstalledDefinitionInput {
                    canonical_set: String::from_utf8(
                        norn_ir::canonical::canonical_json_of(artifacts).expect("canonical set"),
                    )
                    .expect("utf8"),
                    registry: Vec::new(),
                    receipt_sha256: norn_ir::canonical::domain_hash_hex(
                        b"norn.test.receipt\0",
                        &tag,
                    )
                    .expect("receipt digest"),
                    admission_receipt: None,
                    readmission: None,
                    diff: json!({}),
                    installed_at: CLOCK,
                    layout: None,
                },
                None,
                SCOPE,
            )
            .expect("install definitions");
        let cell = SqliteCell::new(store, SCOPE, |_| BTreeMap::new()).expect("cell");
        (cell, dir)
    }

    fn ingest(
        cell: &mut ObsCell,
        mapping: &str,
        dataset: &str,
        source_id: &str,
        event_id: &str,
        source_version: &str,
        record: &Value,
    ) -> bool {
        let payload = V::from_json(record.clone()).expect("payload");
        let digest = norn_ir::canonical::HashProfile::Norn1
            .hash_json(serde_json::to_string(&payload).expect("json"))
            .expect("digest");
        let commits = cell.store().export_commit_count().expect("commits");
        let ts = CLOCK
            .checked_add(commits.saturating_mul(10))
            .expect("clock");
        let raw = Observed {
            id: ObservationId(format!("delivery-{event_id}-{commits}")),
            provenance: Provenance {
                source: "synthetic-demo".into(),
                source_id: source_id.into(),
                mapping_version: "demo/1".into(),
                observed_at: LogicalTime::from_millis(ts),
            },
            input: norn_ir::runtime_object! {
                "contract" => "norn.raw_observation/v1",
                "source" => "synthetic-demo",
                "source_id" => source_id,
                "event_id" => event_id,
                "source_version" => source_version,
                "pipeline_version" => "demo/1",
                "kind" => "upsert",
                "extracted_at" => ts,
                "payload" => payload,
                "payload_digest" => digest,
            },
        };
        let input = MappingInput {
            mapping: mapping.into(),
            dataset: dataset.into(),
            connector_instance: "demo".into(),
            partition: "default".into(),
            recorded_at: norn_ir::provenance::UtcTimestamp("2026-11-03T00:00:00Z".into()),
        };
        let (id, _) = cell.inject_mapping_bundle(&raw, &input).expect("inject");
        cell.process_mapping_bundle(&id).expect("process")
    }

    fn last_cutoff(cell: &ObsCell) -> KnowledgeCutoff {
        let receipt = cell
            .store()
            .read()
            .receipts()
            .expect("receipts")
            .pop()
            .expect("at least one receipt");
        KnowledgeCutoff {
            recorded_at: LogicalTime::from_millis(receipt.body.logical_time),
            frontier: receipt.body.commit_seq,
        }
    }

    fn query_context(now_ms: u64) -> QueryContext {
        QueryContext {
            world: MAIN_WORLD.into(),
            scope: SCOPE.into(),
            population_pin: "all".into(),
            authorization_epoch: 1,
            mapping_pin: "fixture:asof".into(),
            now_ms,
            source_cursors: BTreeMap::new(),
            watermarks: BTreeMap::new(),
        }
    }

    fn admit_weekly_sum(
        artifacts: &[ArtifactEnvelope],
        entity: &str,
        expr: &str,
        question_name: &str,
    ) -> QuestionPlan {
        let space = derive_metric_space(
            &ScopedView::uniform(artifacts, &BTreeSet::new()),
            &LawRoot::Entity(entity.to_owned()),
        )
        .expect("derived space");
        let metric = space
            .candidates()
            .iter()
            .find(|c| c.definition.type_params.expr.as_deref() == Some(expr))
            .expect("derived sum metric");
        let question: QuestionIR = serde_json::from_value(json!({
            "irVersion": "0.1", "name": question_name, "objectSet": entity,
            "aggregate": [{"metric": metric.definition.name}],
            "groupBy": [{"path": {"links": [], "property": "occurred_at"}, "grain": "week"}],
            "window": {"start": "2026-07-06T00:00:00Z", "end": "2026-08-10T00:00:00Z"},
            "freshness": {"kind": "live", "maxAgeMs": 5000},
        }))
        .expect("question");
        space.admit(&question, 100).expect("admitted question")
    }

    fn linked_law_artifacts() -> Vec<ArtifactEnvelope> {
        let iface = envelope(
            "demo.Trackable.interface",
            "entity",
            "demo",
            json!({"entityVersion": "2.0", "interface": {"name": "demo.Trackable", "properties": [{"id": "id", "type": {"kind": "text"}}]}}),
            json!([]),
        );
        let receipt = envelope(
            "demo.Receipt.entity",
            "entity",
            "demo",
            json!({
                "entityVersion": "2.0",
                "objectType": {
                    "name": "demo.Receipt", "implements": ["demo.Trackable"],
                    "properties": [
                        {"id": "id", "type": {"kind": "text"}},
                        {"id": "received_qty", "type": {"kind": "decimal", "scale": 2, "additivity": {"kind": "additive"}}}
                    ]
                }
            }),
            json!([]),
        );
        let inspection = envelope(
            "demo.Inspection.entity",
            "entity",
            "demo",
            json!({
                "entityVersion": "2.0",
                "objectType": {
                    "name": "demo.Inspection", "implements": ["demo.Trackable"],
                    "properties": [
                        {"id": "id", "type": {"kind": "text"}},
                        {"id": "inspected_qty", "type": {"kind": "decimal", "scale": 2, "additivity": {"kind": "additive"}}},
                        {"id": "occurred_at", "type": {"kind": "timestamp", "timezone": "utc"}}
                    ],
                    "links": [{"id": "receipt", "target": "demo.Receipt", "cardinality": {"to": "one", "from": "many"}}]
                }
            }),
            json!([]),
        );
        let law = envelope(
            "demo.Inspection.within_receipt.law",
            "law",
            "demo",
            json!({
                "lawVersion": "0.1", "id": "demo.Inspection.within_receipt", "owner": "demo",
                "subject": "demo.Inspection",
                "scope": {"root": {"kind": "entity", "id": "demo.Inspection"}, "objectTypes": ["demo.Inspection"]},
                "severity": "error", "modality": {"kind": "always"},
                "condition": "self.receipt != null",
                "response": {"kind": "predicate", "expression": "self.inspected_qty <= self.receipt.received_qty"},
                "rationale": "Inspected quantity must not exceed linked receipt quantity.",
                "provenance": {"source": "authoring_sdk"}
            }),
            json!([{"artifactId": "demo.Inspection.entity", "kind": "entity", "required": true, "version": "0.1.0"}]),
        );
        let source = linked_law_source();
        let mapping = linked_law_mapping(&source);
        vec![iface, receipt, inspection, law, source, mapping]
    }

    fn linked_law_source() -> ArtifactEnvelope {
        envelope(
            "demo.linked.source",
            "source_schema",
            "demo",
            json!({
                "format": "norn.source-schema/1",
                "source": {"system": "synthetic-demo", "instance": "demo", "version": "demo/1"},
                "observedAt": "2026-11-02T00:00:00Z",
                "adapter": {"name": "synthetic-record-replay", "version": "0.1.0"},
                "rawMetadata": {"uri": "synthetic://demo", "sha256": "aa".repeat(32)},
                "datasets": [
                    {"id": "Receipt", "nativeName": "Receipt", "kind": "document"},
                    {"id": "Inspection", "nativeName": "Inspection", "kind": "document"}
                ],
                "fields": [
                    {"dataset": "Receipt", "name": "id", "ordinal": 0, "nativeType": "Text", "typeCandidate": {"type": "string", "origin": "declared"}, "required": true},
                    {"dataset": "Receipt", "name": "received_qty", "ordinal": 1, "nativeType": "Decimal", "typeCandidate": {"type": "decimal", "origin": "declared"}, "required": true},
                    {"dataset": "Inspection", "name": "id", "ordinal": 0, "nativeType": "Text", "typeCandidate": {"type": "string", "origin": "declared"}, "required": true},
                    {"dataset": "Inspection", "name": "receipt_id", "ordinal": 1, "nativeType": "Text", "typeCandidate": {"type": "string", "origin": "declared"}, "required": true},
                    {"dataset": "Inspection", "name": "inspected_qty", "ordinal": 2, "nativeType": "Decimal", "typeCandidate": {"type": "decimal", "origin": "declared"}, "required": true},
                    {"dataset": "Inspection", "name": "occurred_at", "ordinal": 3, "nativeType": "Timestamp", "typeCandidate": {"type": "datetime", "origin": "declared"}, "required": true}
                ],
                "codeSets": [],
                "constraints": [
                    {"id": "pk_r", "kind": "primary_key", "dataset": "Receipt", "fields": ["id"], "origin": "declared"},
                    {"id": "pk_i", "kind": "primary_key", "dataset": "Inspection", "fields": ["id"], "origin": "declared"}
                ]
            }),
            json!([]),
        )
    }

    fn linked_law_mapping(source: &ArtifactEnvelope) -> ArtifactEnvelope {
        let pin = norn_checker::ArtifactRef::of(source).expect("source pin");
        envelope(
            "demo.linked.mapping",
            "mapping",
            "demo",
            json!({
                "format": "norn.mapping/1",
                "extensions": {"norn.mapping.generic/1.0.0": {"parity": "unqualified"}},
                "source": {"schemaId": pin.artifact_id, "version": pin.version, "sha256": pin.sha256},
                "interfaces": ["demo.Trackable.interface"],
                "maps": [
                    {
                        "id": "receipt",
                        "source": {"dataset": "Receipt"},
                        "target": {"entity": "demo.Receipt.entity", "interface": "demo.Trackable.interface"},
                        "subject": "input.id",
                        "sourceRecord": "'demo/Receipt/' + input.id",
                        "properties": [
                            {"property": "id", "value": "input.id"},
                            {"property": "received_qty", "value": "input.received_qty"}
                        ]
                    },
                    {
                        "id": "inspection",
                        "source": {"dataset": "Inspection"},
                        "target": {"entity": "demo.Inspection.entity", "interface": "demo.Trackable.interface"},
                        "subject": "input.id",
                        "sourceRecord": "'demo/Inspection/' + input.id",
                        "links": [{"link": "receipt", "key": "input.receipt_id", "targetMap": "receipt"}],
                        "properties": [
                            {"property": "id", "value": "input.id"},
                            {"property": "inspected_qty", "value": "input.inspected_qty"},
                            {"property": "occurred_at", "value": "timestamp(input.occurred_at)"}
                        ]
                    }
                ]
            }),
            json!([
                {"artifactId": "demo.linked.source", "kind": "source_schema", "required": true, "version": "0.1.0"},
                {"artifactId": "demo.Trackable.interface", "kind": "entity", "required": true, "version": "0.1.0"},
                {"artifactId": "demo.Receipt.entity", "kind": "entity", "required": true, "version": "0.1.0"},
                {"artifactId": "demo.Inspection.entity", "kind": "entity", "required": true, "version": "0.1.0"}
            ]),
        )
    }

    fn assert_cutoff_refused_unsupported(cell: &ObsCell, plan: &QuestionPlan, k1: KnowledgeCutoff) {
        let err = cell
            .store()
            .evaluate_question_known_at(plan, query_context(k1.recorded_at.as_millis()), k1, |_| {
                Ok(true)
            })
            .expect_err("linked LAW-LINK-1 finding recovery at cutoff must fail closed");
        assert!(
            matches!(
                err,
                CellError::ComputeRefused("core.compute.unsupported_engine")
            ),
            "expected core.compute.unsupported_engine, got {err:?}"
        );
    }

    fn assert_k3_to_k6_mutations_still_fail_closed(
        cell: &mut ObsCell,
        plan: &QuestionPlan,
        k1: KnowledgeCutoff,
    ) {
        // K3 edge replacement: re-pointing I1 to R2 must not alter K1's refusal.
        assert!(ingest(
            cell,
            "demo.linked.mapping",
            "Receipt",
            "R2",
            "ev-r2-1",
            "1",
            &json!({"id": "R2", "received_qty": "25.00"}),
        ));
        assert!(ingest(
            cell,
            "demo.linked.mapping",
            "Inspection",
            "I1",
            "ev-i1-2",
            "2",
            &json!({"id": "I1", "receipt_id": "R2", "inspected_qty": "15.00", "occurred_at": "2026-07-07T10:00:00Z"}),
        ));
        assert_cutoff_refused_unsupported(cell, plan, k1);

        // K4 link deletion, then K5 subject deletion + K6 recreation.
        let link_ver = cell
            .store()
            .read()
            .link_version(
                MAIN_WORLD,
                "demo.Inspection",
                "receipt",
                "I1",
                "demo.Receipt",
                "R2",
            )
            .expect("link version");
        cell.advance_to(LogicalTime::from_millis(CLOCK + 60_000));
        cell.commit(CellCommit {
            links: vec![LinkWrite {
                key: LinkKey::new(
                    FactKey::new("demo.Inspection", "I1"),
                    "receipt",
                    FactKey::new("demo.Receipt", "R2"),
                ),
                expected: FactVersion(link_ver),
                mutation: LinkMutation::Remove,
            }],
            ..CellCommit::empty()
        })
        .expect("unlink I1 -> R2");
        assert_cutoff_refused_unsupported(cell, plan, k1);

        let subj_ver = cell
            .store()
            .read()
            .version(MAIN_WORLD, "demo.Inspection", "I1")
            .expect("subject version");
        cell.advance_to(LogicalTime::from_millis(CLOCK + 70_000));
        cell.commit(CellCommit {
            writes: vec![FactWrite {
                key: FactKey::new("demo.Inspection", "I1"),
                expected: FactVersion(subj_ver),
                mutation: FactMutation::Delete,
            }],
            ..CellCommit::empty()
        })
        .expect("delete I1");
        assert!(ingest(
            cell,
            "demo.linked.mapping",
            "Inspection",
            "I1",
            "ev-i1-3",
            "3",
            &json!({"id": "I1", "receipt_id": "R2", "inspected_qty": "5.00", "occurred_at": "2026-07-07T10:00:00Z"}),
        ));
        assert_cutoff_refused_unsupported(cell, plan, k1);
    }

    #[test]
    fn linked_law_finding_recovery_at_cutoff_fails_closed_across_k2_mutations() {
        let artifacts = linked_law_artifacts();
        let plan = admit_weekly_sum(
            &artifacts,
            "demo.Inspection",
            "inspected_qty",
            "demo.inspected_week",
        );
        let (mut cell, _dir) = open_cell(&artifacts, "linked_asof");

        assert!(ingest(
            &mut cell,
            "demo.linked.mapping",
            "Receipt",
            "R1",
            "ev-r1-1",
            "1",
            &json!({"id": "R1", "received_qty": "10.00"}),
        ));
        assert!(ingest(
            &mut cell,
            "demo.linked.mapping",
            "Inspection",
            "I1",
            "ev-i1-1",
            "1",
            &json!({"id": "I1", "receipt_id": "R1", "inspected_qty": "15.00", "occurred_at": "2026-07-07T10:00:00Z"}),
        ));
        let k1 = last_cutoff(&cell);
        let live_k1 = cell
            .store()
            .evaluate_question(&plan, query_context(k1.recorded_at.as_millis()), |_| {
                Ok(true)
            })
            .expect("live at K1");
        assert!(live_k1.exclusions.contains_key("I1"), "{live_k1:?}");
        assert_eq!(live_k1.rows.len(), 0);
        // Before any K2 change: ASOF at K1 fails closed rather than consulting CurrentLinks.
        assert_cutoff_refused_unsupported(&cell, &plan, k1);

        // K2 target-only change: raising R1.received_qty resolves live I1, never K1.
        assert!(ingest(
            &mut cell,
            "demo.linked.mapping",
            "Receipt",
            "R1",
            "ev-r1-2",
            "2",
            &json!({"id": "R1", "received_qty": "20.00"}),
        ));
        let live_k2 = cell
            .store()
            .evaluate_question(&plan, query_context(CLOCK + 50_000), |_| Ok(true))
            .expect("live after K2");
        assert_eq!(live_k2.exclusions, BTreeMap::new());
        assert_eq!(live_k2.rows.len(), 1);
        assert_cutoff_refused_unsupported(&cell, &plan, k1);

        assert_k3_to_k6_mutations_still_fail_closed(&mut cell, &plan, k1);
    }

    fn pure_property_law_artifacts() -> Vec<ArtifactEnvelope> {
        let iface = envelope(
            "demo.Billable.interface",
            "entity",
            "demo",
            json!({"entityVersion": "2.0", "interface": {"name": "demo.Billable", "properties": [{"id": "id", "type": {"kind": "text"}}]}}),
            json!([]),
        );
        let invoice = envelope(
            "demo.Invoice.entity",
            "entity",
            "demo",
            json!({
                "entityVersion": "2.0",
                "objectType": {
                    "name": "demo.Invoice", "implements": ["demo.Billable"],
                    "properties": [
                        {"id": "id", "type": {"kind": "text"}},
                        {"id": "amount", "type": {"kind": "decimal", "scale": 2, "additivity": {"kind": "additive"}}},
                        {"id": "occurred_at", "type": {"kind": "timestamp", "timezone": "utc"}}
                    ]
                }
            }),
            json!([]),
        );
        let law = envelope(
            "demo.Invoice.non_negative.law",
            "law",
            "demo",
            json!({
                "lawVersion": "0.1", "id": "demo.Invoice.non_negative", "owner": "demo",
                "subject": "demo.Invoice",
                "scope": {"root": {"kind": "entity", "id": "demo.Invoice"}, "objectTypes": ["demo.Invoice"]},
                "severity": "error", "modality": {"kind": "always"},
                "response": {"kind": "predicate", "expression": "self.amount >= 0"},
                "rationale": "Invoice amounts must be non-negative.",
                "provenance": {"source": "authoring_sdk"}
            }),
            json!([{"artifactId": "demo.Invoice.entity", "kind": "entity", "required": true, "version": "0.1.0"}]),
        );
        let source = envelope(
            "demo.inv.source",
            "source_schema",
            "demo",
            json!({
                "format": "norn.source-schema/1",
                "source": {"system": "synthetic-demo", "instance": "demo", "version": "demo/1"},
                "observedAt": "2026-11-02T00:00:00Z",
                "adapter": {"name": "synthetic-record-replay", "version": "0.1.0"},
                "rawMetadata": {"uri": "synthetic://inv", "sha256": "bb".repeat(32)},
                "datasets": [{"id": "Invoice", "nativeName": "Invoice", "kind": "document"}],
                "fields": [
                    {"dataset": "Invoice", "name": "id", "ordinal": 0, "nativeType": "Text", "typeCandidate": {"type": "string", "origin": "declared"}, "required": true},
                    {"dataset": "Invoice", "name": "amount", "ordinal": 1, "nativeType": "Decimal", "typeCandidate": {"type": "decimal", "origin": "declared"}, "required": true},
                    {"dataset": "Invoice", "name": "occurred_at", "ordinal": 2, "nativeType": "Timestamp", "typeCandidate": {"type": "datetime", "origin": "declared"}, "required": true}
                ],
                "codeSets": [],
                "constraints": [{"id": "pk_inv", "kind": "primary_key", "dataset": "Invoice", "fields": ["id"], "origin": "declared"}]
            }),
            json!([]),
        );
        let pin = norn_checker::ArtifactRef::of(&source).expect("source pin");
        let mapping = envelope(
            "demo.inv.mapping",
            "mapping",
            "demo",
            json!({
                "format": "norn.mapping/1",
                "extensions": {"norn.mapping.generic/1.0.0": {"parity": "unqualified"}},
                "source": {"schemaId": pin.artifact_id, "version": pin.version, "sha256": pin.sha256},
                "interfaces": ["demo.Billable.interface"],
                "maps": [{
                    "id": "invoice",
                    "source": {"dataset": "Invoice"},
                    "target": {"entity": "demo.Invoice.entity", "interface": "demo.Billable.interface"},
                    "subject": "input.id",
                    "sourceRecord": "'demo/Invoice/' + input.id",
                    "properties": [
                        {"property": "id", "value": "input.id"},
                        {"property": "amount", "value": "input.amount"},
                        {"property": "occurred_at", "value": "timestamp(input.occurred_at)"}
                    ]
                }]
            }),
            json!([
                {"artifactId": "demo.inv.source", "kind": "source_schema", "required": true, "version": "0.1.0"},
                {"artifactId": "demo.Billable.interface", "kind": "entity", "required": true, "version": "0.1.0"},
                {"artifactId": "demo.Invoice.entity", "kind": "entity", "required": true, "version": "0.1.0"}
            ]),
        );
        vec![iface, invoice, law, source, mapping]
    }

    #[test]
    fn non_linked_finding_at_cutoff_survives_k2_deletion_and_recreation() {
        let artifacts = pure_property_law_artifacts();
        let plan = admit_weekly_sum(&artifacts, "demo.Invoice", "amount", "demo.invoice_week");
        let (mut cell, _dir) = open_cell(&artifacts, "pure_prop_asof");

        assert!(ingest(
            &mut cell,
            "demo.inv.mapping",
            "Invoice",
            "INV-BAD",
            "ev-bad-1",
            "1",
            &json!({"id": "INV-BAD", "amount": "-50.00", "occurred_at": "2026-07-07T10:00:00Z"}),
        ));
        assert!(ingest(
            &mut cell,
            "demo.inv.mapping",
            "Invoice",
            "INV-GOOD",
            "ev-good-1",
            "1",
            &json!({"id": "INV-GOOD", "amount": "100.00", "occurred_at": "2026-07-07T10:00:00Z"}),
        ));
        let k1 = last_cutoff(&cell);
        let before = cell
            .store()
            .evaluate_question_known_at(
                &plan,
                query_context(k1.recorded_at.as_millis()),
                k1,
                |_| Ok(true),
            )
            .expect("K1 before mutations");
        assert!(before.result.exclusions.contains_key("INV-BAD"));
        assert_eq!(
            before.result.rows.values().next().expect("bucket").totals[0].canonical(),
            "100"
        );

        // K2 delete INV-BAD, then K3 recreate INV-BAD with a valid amount.
        let bad_ver = cell
            .store()
            .read()
            .version(MAIN_WORLD, "demo.Invoice", "INV-BAD")
            .expect("version");
        cell.advance_to(LogicalTime::from_millis(CLOCK + 50_000));
        cell.commit(CellCommit {
            writes: vec![FactWrite {
                key: FactKey::new("demo.Invoice", "INV-BAD"),
                expected: FactVersion(bad_ver),
                mutation: FactMutation::Delete,
            }],
            ..CellCommit::empty()
        })
        .expect("delete INV-BAD");
        assert!(ingest(
            &mut cell,
            "demo.inv.mapping",
            "Invoice",
            "INV-BAD",
            "ev-bad-2",
            "2",
            &json!({"id": "INV-BAD", "amount": "40.00", "occurred_at": "2026-07-07T10:00:00Z"}),
        ));
        let live_after = cell
            .store()
            .evaluate_question(&plan, query_context(CLOCK + 90_000), |_| Ok(true))
            .expect("live after recreation");
        assert_eq!(live_after.exclusions.len(), 0);
        assert_eq!(
            live_after.rows.values().next().expect("bucket").totals[0].canonical(),
            "140"
        );

        // K1-after must equal K1-before identically.
        let after = cell
            .store()
            .evaluate_question_known_at(
                &plan,
                query_context(k1.recorded_at.as_millis()),
                k1,
                |_| Ok(true),
            )
            .expect("K1 after deletion and recreation");
        assert_eq!(after.result, before.result);
    }

    fn mixed_law_artifacts() -> Vec<ArtifactEnvelope> {
        let mut artifacts = linked_law_artifacts();
        artifacts.push(envelope(
            "demo.Inspection.non_negative.law",
            "law",
            "demo",
            json!({
                "lawVersion": "0.1", "id": "demo.Inspection.non_negative", "owner": "demo",
                "subject": "demo.Inspection",
                "scope": {"root": {"kind": "entity", "id": "demo.Inspection"}, "objectTypes": ["demo.Inspection"]},
                "severity": "error", "modality": {"kind": "always"},
                "response": {"kind": "predicate", "expression": "self.inspected_qty >= 0"},
                "rationale": "Inspected quantity must be non-negative.",
                "provenance": {"source": "authoring_sdk"}
            }),
            json!([{"artifactId": "demo.Inspection.entity", "kind": "entity", "required": true, "version": "0.1.0"}]),
        ));
        artifacts
    }

    #[test]
    fn pure_property_finding_on_mixed_law_entity_recovers_at_cutoff() {
        let artifacts = mixed_law_artifacts();
        let plan = admit_weekly_sum(
            &artifacts,
            "demo.Inspection",
            "inspected_qty",
            "demo.inspected_mixed_week",
        );
        let (mut cell, _dir) = open_cell(&artifacts, "mixed_law_asof");
        let r1 = json!({"id": "R1", "received_qty": "10.00"});
        let bad = json!({"id": "I-NEG", "receipt_id": "R1", "inspected_qty": "-5.00", "occurred_at": "2026-07-07T10:00:00Z"});
        let ok = json!({"id": "I-OK", "receipt_id": "R1", "inspected_qty": "8.00", "occurred_at": "2026-07-07T10:00:00Z"});
        assert!(ingest(
            &mut cell,
            "demo.linked.mapping",
            "Receipt",
            "R1",
            "ev-r1-1",
            "1",
            &r1
        ));
        // Violates ONLY `non_negative` (`-5.00 < 0`), while satisfying `within_receipt` (`-5 <= 10`).
        assert!(ingest(
            &mut cell,
            "demo.linked.mapping",
            "Inspection",
            "I-NEG",
            "ev-ineg-1",
            "1",
            &bad
        ));
        assert!(ingest(
            &mut cell,
            "demo.linked.mapping",
            "Inspection",
            "I-OK",
            "ev-iok-1",
            "1",
            &ok
        ));
        let k1 = last_cutoff(&cell);
        let before = cell
            .store()
            .evaluate_question_known_at(
                &plan,
                query_context(k1.recorded_at.as_millis()),
                k1,
                |_| Ok(true),
            )
            .expect("pure-property finding on mixed-law entity must not trip unrelated linked law");
        assert!(before.result.exclusions.contains_key("I-NEG"));
        assert_eq!(
            before.result.rows.values().next().expect("bucket").totals[0].canonical(),
            "8"
        );
        // K2 fix I-NEG -> live includes I-NEG (total 11), while K1 still reproduces 8.
        let fixed = json!({"id": "I-NEG", "receipt_id": "R1", "inspected_qty": "3.00", "occurred_at": "2026-07-07T10:00:00Z"});
        assert!(ingest(
            &mut cell,
            "demo.linked.mapping",
            "Inspection",
            "I-NEG",
            "ev-ineg-2",
            "2",
            &fixed
        ));
        let after = cell
            .store()
            .evaluate_question_known_at(
                &plan,
                query_context(k1.recorded_at.as_millis()),
                k1,
                |_| Ok(true),
            )
            .expect("K1 remains reproducible after K2 fix");
        assert_eq!(after.result, before.result);
    }

    #[test]
    fn linked_time_question_refuses_cutoff_in_evaluate_and_source_disagreements() {
        let types: [ObjectTypeDef; 2] = [
            serde_json::from_value(json!({"name": "demo.Event", "properties": [
                {"id": "occurred_at", "type": {"kind": "timestamp", "timezone": "utc"}}
            ]}))
            .expect("event"),
            serde_json::from_value(json!({"name": "demo.Entry", "properties": [
                {"id": "amount", "type": {"kind": "decimal", "scale": 2, "additivity": {"kind": "additive"}}}
            ], "links": [{"id": "event", "target": "demo.Event", "cardinality": {"to": "one", "from": "many"}}]}))
            .expect("entry"),
        ];
        let artifacts: Vec<ArtifactEnvelope> = types
            .iter()
            .map(|ot| {
                envelope(
                    &format!("{}.entity", ot.name.as_str()),
                    "entity",
                    "demo",
                    json!({"entityVersion": "2.0", "objectType": ot}),
                    json!([]),
                )
            })
            .collect();
        let space = derive_metric_space(
            &ScopedView::uniform(&artifacts, &BTreeSet::new()),
            &LawRoot::Entity("demo.Entry".to_owned()),
        )
        .expect("space");
        let metric = space
            .candidates()
            .iter()
            .find(|c| {
                c.definition
                    .norn
                    .time_dimension
                    .as_ref()
                    .is_some_and(|t| t.links.len() == 1)
            })
            .expect("linked-time sum");
        let question: QuestionIR = serde_json::from_value(json!({
            "irVersion": "0.1", "name": "demo.entry_week", "objectSet": "demo.Entry",
            "aggregate": [{"metric": metric.definition.name}],
            "groupBy": [{"path": {"links": ["event"], "property": "occurred_at"}, "grain": "week"}],
            "window": {"start": "2026-07-06T00:00:00Z", "end": "2026-08-10T00:00:00Z"},
            "freshness": {"kind": "live", "maxAgeMs": 5000},
        }))
        .expect("question");
        let plan = space.admit(&question, 100).expect("plan");
        let model = Model::from_definitions(&types, &[]).expect("model");
        let store = CellStore::open_in_memory(model).expect("store");
        let cutoff = KnowledgeCutoff {
            recorded_at: LogicalTime::from_millis(CLOCK),
            frontier: 0,
        };
        let eval_err = store
            .evaluate_question_known_at(&plan, query_context(CLOCK), cutoff, |_| Ok(true))
            .expect_err("linked-time evaluate_question_known_at must fail closed");
        assert!(matches!(
            eval_err,
            CellError::ComputeRefused("core.compute.unsupported_engine")
        ));
        let disagree_err = store
            .answer_source_disagreements(
                &plan,
                &query_context(CLOCK),
                (0, Some(cutoff)),
                &mut |_| Ok(true),
            )
            .expect_err("linked-time answer_source_disagreements at cutoff must fail closed");
        assert!(matches!(
            disagree_err,
            CellError::ComputeRefused("core.compute.unsupported_engine")
        ));
    }
}
