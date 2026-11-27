//! Exhaustive small-world disclosure noninterference regressions (B2).
//!
//! Compares pairs of cells `W0` vs `Wk` that differ strictly in withheld,
//! masked, dynamically `_norn_markings`-tagged, or restricted fields across
//! every sealed `Disclosed*` projection surface:
//! - Objects & grid queries (`get_object`, `get_object_page`,
//!   `get_queried_object`, `get_object_query`)
//! - Review & links (`get_object_history`, `get_object_links`,
//!   `get_object_lineage`, `get_object_lineage_at`)
//! - Question answers & traces (D53 bucket suppression, keyed handles,
//!   `fact_digest` & `source` redaction)
//! - Transforms (`get_transform_rows`, `get_transform_row_lineage`)
//! - Exports (`export_objects` sealed `DisclosedExportPermit` fail-closed gate)

#[cfg(not(target_arch = "wasm32"))]
#[path = "../../norn-cell/tests/support/governed.rs"]
mod governed;

#[cfg(not(target_arch = "wasm32"))]
#[path = "support/hosted_read.rs"]
mod hosted_read;

#[cfg(not(target_arch = "wasm32"))]
#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "small-world disclosure noninterference suite"
)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use norn_api::analytics_routes::{
        EngineAnswer, EngineCall, ExportQuery, Transport, export_objects,
    };
    use norn_api::authz::{AccessRequest, Authorizer, ReadCapability, ReadRequest};
    use norn_api::host::DevHost;
    use norn_api::object_links::{ObjectLinkParams, get_object_lineage, get_object_links};
    use norn_api::object_query::ObjectQueryParams;
    use norn_api::object_review::{get_object_history, get_object_lineage_at};
    use norn_api::object_routes::{
        get_object, get_object_page, get_object_query, get_queried_object,
    };
    use norn_api::privacy::{DisclosedDocument, serialize_disclosed};
    use norn_api::question_routes::{
        QuestionRuntime, QuestionSnapshot, TraceQuery, get_answer, get_numeric_trace,
    };
    use norn_api::refusal::Refusal;
    use norn_api::scope::{CellScope, ScopeQuery};
    use norn_api::transform_routes::{
        TransformRowsParams, get_transform_row_lineage, get_transform_rows,
    };
    use norn_cell::compute::QueryContext;
    use norn_cell::engine::NativeEngine;
    use norn_cell::{CellStore, CommitBatch, FactWriteRow, LinkChange, MAIN_WORLD};
    use norn_checker::EffectiveCatalog;
    use norn_checker::metric::compute::{QuestionPlan, admit};
    use norn_ir::{MetricDefinition, ObjectTypeDef, QuestionIR, RuntimeValue as V};
    use norn_kernel::LogicalTime;
    use serde_json::{Value as Json, json};

    use super::governed::GovernedCommit as _;
    use super::hosted_read::Mapping;

    const SECRET_KEY: [u8; 32] = [0x5A; 32];
    const CLOCK: u64 = 1_793_702_400_000;
    const CUSTOMER: &str = "shop.Customer";
    const ORDER: &str = "shop.Order";
    const TRANSFORM: &str = "shop.customer_summary";
    const QUESTION: &str = "shop.order_week";
    const WEEK: &str = "\"2026-10-26T00:00:00Z\"";

    fn physical() -> CellScope {
        CellScope {
            org_id: "org_ni".into(),
            environment_id: "dev".into(),
        }
    }

    fn scope() -> ScopeQuery {
        ScopeQuery {
            org_id: Some("org_ni".into()),
            environment_id: Some("dev".into()),
        }
    }

    struct WorldPolicy;

    impl Authorizer for WorldPolicy {
        fn authorize(&self, _: &AccessRequest<'_>) -> Result<(), Refusal> {
            Ok(())
        }

        fn read(&self, request: &ReadRequest<'_>) -> Result<(), Refusal> {
            let actor = request.access.principal().subject_id.as_str();
            let id = request.access.resource().id.as_str();
            let allowed = match actor {
                "viewer" => match id {
                    "c-hidden" => false,
                    "c-withheld" | "o-secret" => request.capability == ReadCapability::Population,
                    _ => true,
                },
                "pop_only" => request.capability == ReadCapability::Population,
                _ => false,
            };
            if allowed {
                Ok(())
            } else {
                Err(Refusal::invalid("policy denied"))
            }
        }
    }

    fn json_of(doc: &impl DisclosedDocument) -> String {
        let sealed = serialize_disclosed(doc).expect("serialize disclosed");
        let direct = serde_json::to_string(doc).expect("serde transparent");
        assert_eq!(sealed, direct, "sealed serializer must match wire JSON");
        sealed
    }

    fn refusal_json(err: &Refusal) -> String {
        serde_json::to_string(err).expect("refusal json")
    }

    fn artifact(id: &str, kind: &str, deps: Json, payload: Json) -> Json {
        json!({
            "irVersion": "0.1", "artifactId": id, "kind": kind,
            "namespace": "shop", "name": id, "version": "0.1.0", "lifecycle": "draft",
            "scope": {"orgId": "org_ni", "environmentId": "dev"},
            "dependencies": deps, "provenance": {"source": "authoring_sdk"},
            "payload": payload
        })
    }

    fn customer_model_and_defs(mask: &Json) -> (norn_cell::Model, Json) {
        let customer: ObjectTypeDef = serde_json::from_value(json!({
            "name": CUSTOMER,
            "properties": [
                {"id": "region", "type": {"kind": "text"}},
                {"id": "email", "type": {"kind": "text"}, "constraints": {"nullable": true}, "markings": ["pii.contact"]},
                {"id": "note", "type": {"kind": "text"}, "constraints": {"nullable": true}, "markings": ["pii.secret"]}
            ],
            "links": [
                {"id": "referred", "target": CUSTOMER, "cardinality": {"to": "many", "from": "many"}}
            ]
        }))
        .expect("customer type");
        let model = norn_cell::Model::from_definitions(std::slice::from_ref(&customer), &[])
            .expect("model");
        let pin = json!({"artifactId": "shop.Customer.entity", "kind": "entity", "version": "0.1.0", "required": true});
        let transform = artifact(
            TRANSFORM,
            "derived",
            json!([pin]),
            json!({
                "viewVersion": "0.1", "viewId": TRANSFORM,
                "inputs": [{"artifactId": "shop.Customer.entity", "kind": "entity", "version": "0.1.0", "alias": "c"}],
                "calc": {"kind": "relational", "output": "proj", "nodes": {
                    "scan_c": {"op": "scan", "input": "c"},
                    "proj": {"op": "project", "input": "scan_c", "columns": [
                        {"name": "id", "expression": "input.id"},
                        {"name": "region", "expression": "input.region"},
                        {"name": "email", "expression": "input.email"},
                        {"name": "note", "expression": "input.note"}
                    ]}
                }},
                "outputSchema": {}, "maintenance": "recompute_on_read"
            }),
        );
        let artifacts = json!([
            artifact(
                "shop.Customer.entity",
                "entity",
                json!([]),
                json!({"entityVersion": "2.0", "objectType": customer})
            ),
            artifact(
                "shop.markings.entity",
                "entity",
                json!([]),
                json!({
                    "entityVersion": "2.0",
                    "markings": [
                        {"id": "pii.contact", "label": "Contact", "severity": "high", "category": "pii.contact", "mask": mask},
                        {"id": "pii.secret", "label": "Secret", "severity": "high", "category": "pii.secret", "mask": {"kind": "refuse"}}
                    ]
                })
            ),
            transform
        ]);
        (model, artifacts)
    }

    struct ObjectWorldSpec<'a> {
        mask: &'a Json,
        secret_email: &'a str,
        secret_note: &'a str,
        secret_note_v2: Option<&'a str>,
        withheld_updates: &'a [(&'a str, &'a str)],
        hidden_link: bool,
    }

    fn open_object_world(spec: &ObjectWorldSpec<'_>) -> (DevHost<NativeEngine>, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cell.sqlite");
        let (model, artifacts) = customer_model_and_defs(spec.mask);
        let scope_key = physical().key();
        {
            let mut store =
                CellStore::open(&path, model.clone(), norn_cell::StoreOptions::default())
                    .expect("store");
            store
                .install_definitions(
                    model.clone(),
                    norn_cell::InstalledDefinitionInput {
                        canonical_set: serde_json::to_string(&artifacts).expect("defs"),
                        registry: Vec::new(),
                        receipt_sha256: "ni-defs".into(),
                        admission_receipt: None,
                        readmission: None,
                        diff: json!({}),
                        installed_at: 1,
                        layout: None,
                    },
                    Some(0),
                    &scope_key,
                )
                .expect("install");
            seed_customer_commits(&mut store, &scope_key, spec);
        }
        let store =
            CellStore::open(&path, model, norn_cell::StoreOptions::default()).expect("open");
        let host = DevHost::from_store(store, physical(), |cmd| cmd.payload.clone())
            .expect("host")
            .with_authorizer(WorldPolicy)
            .with_pseudonym_secret(Some(&SECRET_KEY));
        (host, dir)
    }

    fn seed_customer_commits(store: &mut CellStore, scope_key: &str, spec: &ObjectWorldSpec<'_>) {
        let mut writes = vec![
            FactWriteRow::Put {
                object_type: CUSTOMER.into(),
                key: "c1".into(),
                values: BTreeMap::from([
                    ("region".into(), V::from("north")),
                    ("email".into(), V::from(spec.secret_email)),
                    ("note".into(), V::from(spec.secret_note)),
                ]),
            },
            FactWriteRow::Put {
                object_type: CUSTOMER.into(),
                key: "c2".into(),
                values: BTreeMap::from([
                    ("region".into(), V::from("south")),
                    ("email".into(), V::from("public-peer@example.test")),
                    ("note".into(), V::from("peer-secret")),
                ]),
            },
        ];
        let mut links = vec![LinkChange::Add {
            object_type: CUSTOMER.into(),
            link: "referred".into(),
            source: "c1".into(),
            target: "c2".into(),
            values: BTreeMap::new(),
            target_type: Some(CUSTOMER.into()),
        }];
        if spec.hidden_link {
            writes.push(FactWriteRow::Put {
                object_type: CUSTOMER.into(),
                key: "c-hidden".into(),
                values: BTreeMap::from([
                    ("region".into(), V::from("hidden-region")),
                    ("email".into(), V::from(spec.secret_email)),
                    ("note".into(), V::from(spec.secret_note)),
                ]),
            });
            links.push(LinkChange::Add {
                object_type: CUSTOMER.into(),
                link: "referred".into(),
                source: "c1".into(),
                target: "c-hidden".into(),
                values: BTreeMap::new(),
                target_type: Some(CUSTOMER.into()),
            });
        }
        store
            .commit(&CommitBatch {
                world: MAIN_WORLD.into(),
                scope: scope_key.to_owned(),
                logical_time: 10,
                writes,
                links,
                ..CommitBatch::default()
            })
            .expect("seed commit");
        if let Some(note_v2) = spec.secret_note_v2 {
            store
                .commit(&CommitBatch {
                    world: MAIN_WORLD.into(),
                    scope: scope_key.to_owned(),
                    logical_time: 15,
                    writes: vec![FactWriteRow::Put {
                        object_type: CUSTOMER.into(),
                        key: "c1".into(),
                        values: BTreeMap::from([
                            ("region".into(), V::from("north")),
                            ("email".into(), V::from(spec.secret_email)),
                            ("note".into(), V::from(note_v2)),
                        ]),
                    }],
                    ..CommitBatch::default()
                })
                .expect("hidden-only note update");
        }
        for (idx, (region, email)) in spec.withheld_updates.iter().enumerate() {
            store
                .commit(&CommitBatch {
                    world: MAIN_WORLD.into(),
                    scope: scope_key.to_owned(),
                    logical_time: 20_u64.saturating_add(u64::try_from(idx).unwrap_or(0)),
                    writes: vec![FactWriteRow::Put {
                        object_type: CUSTOMER.into(),
                        key: "c-withheld".into(),
                        values: BTreeMap::from([
                            ("region".into(), V::from(*region)),
                            ("email".into(), V::from(*email)),
                            ("note".into(), V::from(spec.secret_note)),
                        ]),
                    }],
                    ..CommitBatch::default()
                })
                .expect("withheld write");
        }
    }

    fn capture_object_world_outputs(host: &DevHost<NativeEngine>) -> Vec<String> {
        let obj = json_of(
            &get_object(host, &scope(), Some("viewer"), CUSTOMER, "c1", &Mapping)
                .expect("get_object"),
        );
        let page = json_of(
            &get_object_page(host, &scope(), Some("viewer"), CUSTOMER, None, 10, &Mapping)
                .expect("get_object_page"),
        );
        let queried_c1 = json_of(
            &get_queried_object(host, &scope(), Some("viewer"), CUSTOMER, "c1", &Mapping)
                .expect("get_queried_object c1"),
        );
        let queried_withheld = json_of(
            &get_queried_object(
                host,
                &scope(),
                Some("viewer"),
                CUSTOMER,
                "c-withheld",
                &Mapping,
            )
            .expect("get_queried_object withheld"),
        );
        let grid = json_of(
            &get_object_query(
                host,
                &scope(),
                Some("viewer"),
                CUSTOMER,
                &ObjectQueryParams::default(),
                &Mapping,
            )
            .expect("get_object_query"),
        );
        let history = json_of(
            &get_object_history(host, &scope(), Some("viewer"), CUSTOMER, "c1", &Mapping)
                .expect("get_object_history"),
        );
        let links = json_of(
            &get_object_links(
                host,
                &scope(),
                Some("viewer"),
                CUSTOMER,
                "c1",
                &ObjectLinkParams::default(),
                &Mapping,
            )
            .expect("get_object_links"),
        );
        let xform = json_of(
            &get_transform_rows(
                host,
                &scope(),
                Some("viewer"),
                TRANSFORM,
                &TransformRowsParams::default(),
                &Mapping,
                LogicalTime::from_millis(CLOCK),
            )
            .expect("get_transform_rows"),
        );
        let row_id = serde_json::from_str::<Json>(&xform).expect("json")["rows"]
            .as_array()
            .expect("rows")
            .iter()
            .find(|row| row["values"]["id"] == "c1")
            .expect("c1 row")["rowId"]
            .as_str()
            .expect("rowId")
            .to_owned();
        let xform_lineage = json_of(
            &get_transform_row_lineage(
                host,
                &scope(),
                Some("viewer"),
                TRANSFORM,
                &row_id,
                &Mapping,
                LogicalTime::from_millis(CLOCK),
            )
            .expect("get_transform_row_lineage"),
        );
        vec![
            obj,
            page,
            queried_c1,
            queried_withheld,
            grid,
            history,
            links,
            xform,
            xform_lineage,
        ]
    }

    #[test]
    fn small_world_object_query_history_links_lineage_and_transform_noninterference() {
        let masks = [json!({"kind": "redacted"}), json!({"kind": "null"})];
        let cases: [(&str, &str, Option<&str>, &[(&str, &str)], bool); 3] = [
            (
                "alpha@secret.test",
                "memo-1",
                None,
                &[("east", "w1@x.test")],
                false,
            ),
            (
                "omega-very-long-secret@corp.example",
                "top-secret-memo-99999",
                None,
                &[
                    ("west", "w1@x.test"),
                    ("north", "w2@x.test"),
                    ("south", "w3@x.test"),
                ],
                true,
            ),
            (
                "z@q.io",
                "another-classified-note",
                None,
                &[("central", "z@x.test"), ("central", "z2@x.test")],
                true,
            ),
        ];
        for mask in &masks {
            let mut baseline: Option<Vec<String>> = None;
            for (email, note, note_v2, withheld_writes, hidden_link) in cases {
                let (host, _dir) = open_object_world(&ObjectWorldSpec {
                    mask,
                    secret_email: email,
                    secret_note: note,
                    secret_note_v2: note_v2,
                    withheld_updates: withheld_writes,
                    hidden_link,
                });
                let absent_refusal = refusal_json(
                    &get_queried_object(
                        &host,
                        &scope(),
                        Some("viewer"),
                        CUSTOMER,
                        "c-does-not-exist",
                        &Mapping,
                    )
                    .expect_err("absent"),
                );
                let hidden_refusal = refusal_json(
                    &get_queried_object(
                        &host,
                        &scope(),
                        Some("viewer"),
                        CUSTOMER,
                        "c-hidden",
                        &Mapping,
                    )
                    .expect_err("hidden"),
                );
                assert_eq!(absent_refusal, hidden_refusal);
                assert_eq!(
                    refusal_json(
                        &get_object_lineage(
                            &host,
                            &scope(),
                            Some("viewer"),
                            CUSTOMER,
                            "c1",
                            &Mapping
                        )
                        .expect_err("marked lineage refuses"),
                    ),
                    absent_refusal
                );
                assert_eq!(
                    refusal_json(
                        &get_object_lineage_at(
                            &host,
                            &scope(),
                            Some("viewer"),
                            (CUSTOMER, "c1", 1),
                            &Mapping,
                        )
                        .expect_err("marked lineage_at refuses"),
                    ),
                    absent_refusal
                );
                let outputs = capture_object_world_outputs(&host);
                for text in &outputs {
                    assert!(!text.contains(email), "leaked email {email} in {text}");
                    assert!(!text.contains(note), "leaked note {note} in {text}");
                }
                if let Some(expected) = &baseline {
                    assert_eq!(&outputs, expected, "worlds diverged under {mask}");
                } else {
                    baseline = Some(outputs);
                }
            }
        }
    }

    #[test]
    fn hidden_only_marked_write_collapses_identically_in_sealed_object_history() {
        let mask = json!({"kind": "redacted"});
        let (w0, _d0) = open_object_world(&ObjectWorldSpec {
            mask: &mask,
            secret_email: "a@secret.test",
            secret_note: "secret-v1",
            secret_note_v2: None,
            withheld_updates: &[],
            hidden_link: false,
        });
        let (w1, _d1) = open_object_world(&ObjectWorldSpec {
            mask: &mask,
            secret_email: "a@secret.test",
            secret_note: "secret-v1",
            secret_note_v2: Some("secret-v2-only-refused-field-changed"),
            withheld_updates: &[],
            hidden_link: false,
        });
        let h0 = json_of(
            &get_object_history(&w0, &scope(), Some("viewer"), CUSTOMER, "c1", &Mapping)
                .expect("history w0"),
        );
        let h1 = json_of(
            &get_object_history(&w1, &scope(), Some("viewer"), CUSTOMER, "c1", &Mapping)
                .expect("history w1"),
        );
        assert_eq!(h0, h1, "hidden-only write must collapse identically");
    }

    struct OrderRuntime {
        plan: QuestionPlan,
        min_disclosed: usize,
    }

    impl QuestionRuntime<NativeEngine> for OrderRuntime {
        fn plan(&self, question: &str) -> Result<QuestionPlan, Refusal> {
            (question == QUESTION)
                .then(|| self.plan.clone())
                .ok_or_else(|| Refusal::invalid("unknown question"))
        }

        fn snapshot(
            &self,
            _store: &CellStore<NativeEngine>,
            _plan: &QuestionPlan,
            _actor: &str,
        ) -> Result<QuestionSnapshot, Refusal> {
            Ok(QuestionSnapshot {
                context: QueryContext {
                    world: MAIN_WORLD.into(),
                    scope: physical().key(),
                    population_pin: "ni".into(),
                    authorization_epoch: 1,
                    mapping_pin: "ni:1".into(),
                    now_ms: CLOCK,
                    source_cursors: BTreeMap::new(),
                    watermarks: BTreeMap::new(),
                },
                cutoff: None,
            })
        }

        fn access(
            &self,
            scope: &CellScope,
            actor: &str,
            question: &str,
            capability: ReadCapability,
            row: Option<&norn_cell::ObjectRecord>,
        ) -> Result<norn_kernel::authz::AccessRequest, Refusal> {
            let scope_key = norn_kernel::authz::ScopeKey {
                org: scope.org_id.clone(),
                environment: Some(scope.environment_id.clone()),
                tenant: None,
                workspace: None,
                business_unit: None,
            };
            Ok(norn_kernel::authz::AccessRequest::new(
                &norn_kernel::authz::VerifiedPrincipal::new(
                    actor,
                    norn_kernel::authz::ActorKind::User,
                    None,
                    vec![],
                    BTreeMap::new(),
                ),
                Some(scope_key.clone()),
                format!("q.{capability:?}"),
                "norn.read",
                norn_kernel::authz::ResourceInput {
                    object_type: row.map_or(ORDER, |r| r.object_type.as_str()).into(),
                    id: row.map_or(question, |r| r.id.as_str()).into(),
                    scope: scope_key,
                    attributes: BTreeMap::new(),
                },
                BTreeMap::new(),
                norn_kernel::authz::Binding::Unbound(
                    norn_kernel::authz::UnboundProfile::HostedRead,
                ),
            ))
        }

        fn min_disclosed_bucket(&self) -> usize {
            self.min_disclosed
        }

        fn handle_key(&self) -> &[u8] {
            &SECRET_KEY
        }
    }

    fn order_schema_and_plan() -> (ObjectTypeDef, norn_cell::Model, QuestionPlan) {
        let order: ObjectTypeDef = serde_json::from_value(json!({
            "name": ORDER,
            "properties": [
                {"id": "occurred_at", "type": {"kind": "timestamp", "timezone": "utc"}},
                {"id": "amount", "type": {"kind": "decimal", "scale": 2, "additivity": {"kind": "additive"}}},
                {"id": "memo", "type": {"kind": "text"}, "constraints": {"nullable": true}, "markings": ["pii.memo"]}
            ]
        }))
        .expect("order type");
        let model =
            norn_cell::Model::from_definitions(std::slice::from_ref(&order), &[]).expect("model");
        let metric: MetricDefinition = serde_json::from_value(json!({
            "name": "order_sum", "label": "Order sum", "type": "simple",
            "type_params": {"expr": "amount", "metric_aggregation_params": {
                "semantic_model": ORDER, "agg": "sum", "agg_time_dimension": "occurred_at"
            }},
            "norn": {"additivity": {"kind": "additive"}, "timeDimension": {"links": [], "property": "occurred_at"}}
        }))
        .expect("metric");
        let question: QuestionIR = serde_json::from_value(json!({
            "irVersion": "0.1", "name": QUESTION, "objectSet": ORDER,
            "aggregate": [{"metric": "order_sum"}],
            "groupBy": [{"path": {"links": [], "property": "occurred_at"}, "grain": "week"}],
            "window": {"start": "2026-10-26T00:00:00Z", "end": "2026-11-02T00:00:00Z"},
            "freshness": {"kind": "live", "maxAgeMs": 5000}
        }))
        .expect("question");
        let catalog = EffectiveCatalog::from_definitions(
            std::slice::from_ref(&order),
            &[],
            &[],
            &BTreeSet::new(),
        )
        .expect("catalog");
        let plan = admit(std::slice::from_ref(&metric), &question, &catalog, 100).expect("plan");
        (order, model, plan)
    }

    fn order_world(
        secret_amount: &str,
        secret_memo: &str,
    ) -> (DevHost<NativeEngine>, OrderRuntime, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("orders.sqlite");
        let (order, model, plan) = order_schema_and_plan();
        let artifacts = json!([
            artifact(
                "shop.Order.entity",
                "entity",
                json!([]),
                json!({"entityVersion": "2.0", "objectType": order})
            ),
            artifact(
                "shop.markings.entity",
                "entity",
                json!([]),
                json!({
                    "entityVersion": "2.0",
                    "markings": [{"id": "pii.memo", "label": "Memo", "severity": "high", "category": "pii", "mask": {"kind": "redacted"}}]
                })
            )
        ]);
        let scope_key = physical().key();
        {
            let mut store =
                CellStore::open(&path, model.clone(), norn_cell::StoreOptions::default())
                    .expect("store");
            store
                .install_definitions(
                    model.clone(),
                    norn_cell::InstalledDefinitionInput {
                        canonical_set: serde_json::to_string(&artifacts).expect("defs"),
                        registry: Vec::new(),
                        receipt_sha256: "ni-order-defs".into(),
                        admission_receipt: None,
                        readmission: None,
                        diff: json!({}),
                        installed_at: 1,
                        layout: None,
                    },
                    Some(0),
                    &scope_key,
                )
                .expect("install");
            store
                .commit(&CommitBatch {
                    world: MAIN_WORLD.into(),
                    scope: scope_key,
                    logical_time: 10,
                    writes: vec![
                        FactWriteRow::Put {
                            object_type: ORDER.into(),
                            key: "o-open".into(),
                            values: BTreeMap::from([
                                ("occurred_at".into(), V::from("2026-10-27T10:00:00Z")),
                                ("amount".into(), V::from_json_text("100.00").expect("dec")),
                                ("memo".into(), V::from(secret_memo)),
                            ]),
                        },
                        FactWriteRow::Put {
                            object_type: ORDER.into(),
                            key: "o-secret".into(),
                            values: BTreeMap::from([
                                ("occurred_at".into(), V::from("2026-10-27T11:00:00Z")),
                                (
                                    "amount".into(),
                                    V::from_json_text(secret_amount).expect("dec"),
                                ),
                                ("memo".into(), V::from(secret_memo)),
                            ]),
                        },
                    ],
                    ..CommitBatch::default()
                })
                .expect("seed orders");
        }
        let store =
            CellStore::open(&path, model, norn_cell::StoreOptions::default()).expect("open");
        let host = DevHost::from_store(store, physical(), |cmd| cmd.payload.clone())
            .expect("host")
            .with_authorizer(WorldPolicy)
            .with_pseudonym_secret(Some(&SECRET_KEY));
        (
            host,
            OrderRuntime {
                plan,
                min_disclosed: 3,
            },
            dir,
        )
    }

    #[test]
    fn small_world_d53_answer_and_trace_noninterference() {
        let mut baseline_rows: Option<String> = None;
        let mut baseline_contribs: Option<Vec<Json>> = None;
        for (secret_amount, secret_memo) in [
            ("11.11", "classified-alpha"),
            ("4321.99", "classified-beta-longer"),
            ("987654.32", "classified-gamma-secret"),
        ] {
            let (host, runtime, _dir) = order_world(secret_amount, secret_memo);
            let answer =
                get_answer(&host, &scope(), Some("viewer"), QUESTION, &runtime).expect("answer");
            let answer_text = json_of(&answer);
            assert!(!answer_text.contains(secret_amount));
            assert!(!answer_text.contains(secret_memo));
            let live = answer.live().expect("live");
            let raw_head = host
                .cell()
                .expect("cell")
                .store()
                .read()
                .verified_head()
                .expect("head")
                .hash;
            assert_ne!(live.revision.hash, raw_head);
            let rows_json = serde_json::to_string(&live.rows).expect("rows json");
            let bucket = &live.rows[WEEK];
            assert!(bucket.suppressed && bucket.totals.is_empty() && bucket.value.is_none());
            if let Some(expected) = &baseline_rows {
                assert_eq!(&rows_json, expected);
            } else {
                baseline_rows = Some(rows_json);
            }
            let trace = get_numeric_trace(
                &host,
                &TraceQuery {
                    org_id: Some("org_ni".into()),
                    environment_id: Some("dev".into()),
                    lineage_ref: Some(live.lineage_ref.clone()),
                    bucket: Some(WEEK.into()),
                    known_as_of: None,
                    now_ms: None,
                },
                Some("viewer"),
                QUESTION,
                &runtime,
            )
            .expect("trace");
            let trace_text = json_of(&trace);
            assert!(!trace_text.contains(secret_amount));
            assert!(!trace_text.contains(secret_memo));
            let normalized: Vec<Json> = trace
                .contributions
                .iter()
                .map(|c| {
                    assert!(c.fact_digest.is_none());
                    assert!(matches!(
                        c.source,
                        norn_cell::compute::lineage::ContributionSource::Restricted
                    ));
                    assert!(c.amounts.is_none());
                    let mut val = serde_json::to_value(c).expect("contrib json");
                    if let Some(obj) = val.as_object_mut() {
                        obj.remove("receiptHash");
                    }
                    val
                })
                .collect();
            if let Some(expected) = &baseline_contribs {
                assert_eq!(&normalized, expected);
            } else {
                baseline_contribs = Some(normalized);
            }
        }
    }

    struct CountingTransport(AtomicUsize);

    impl Transport for CountingTransport {
        fn capabilities(&self) -> norn_cell::analytics::EngineCapabilities {
            norn_cell::analytics::EngineCapabilities::DUCKDB
        }

        fn resumed(&self) -> Option<EngineAnswer> {
            None
        }

        fn call(&self, call: &EngineCall) -> Option<EngineAnswer> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Some(EngineAnswer {
                op: call.op.clone(),
                digest: call.digest.clone(),
                status: 200,
                body: String::new(),
            })
        }
    }

    #[test]
    fn export_refuses_marked_type_identically_to_unauthorized_without_touching_engine() {
        let mask = json!({"kind": "redacted"});
        let (host, _dir) = open_object_world(&ObjectWorldSpec {
            mask: &mask,
            secret_email: "secret@example.test",
            secret_note: "note",
            secret_note_v2: None,
            withheld_updates: &[],
            hidden_link: false,
        });
        let transport = CountingTransport(AtomicUsize::new(0));
        let query = ExportQuery {
            org_id: Some("org_ni".into()),
            environment_id: Some("dev".into()),
            columns: "region".into(),
            format: Some("jsonl".into()),
        };
        let marked_refusal = refusal_json(
            &export_objects(
                &host,
                &query,
                Some("viewer"),
                CUSTOMER,
                &Mapping,
                &transport,
            )
            .expect_err("marked export must refuse"),
        );
        let denied_refusal = refusal_json(
            &export_objects(
                &host,
                &query,
                Some("denied"),
                CUSTOMER,
                &Mapping,
                &transport,
            )
            .expect_err("unauthorized export must refuse"),
        );
        assert_eq!(marked_refusal, denied_refusal);
        assert_eq!(transport.0.load(Ordering::SeqCst), 0);
    }
}
