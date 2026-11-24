//! Unit and store-level tests for the D136/#1661 [`AnalyticalEngine`] seam.

use super::*;

#[test]
fn engine_ids_serialize_exact_lowercase_datafusion_and_duckdb() {
    assert_eq!(
        serde_json::to_string(&AnalyticalEngineId::DataFusion)
            .ok()
            .as_deref(),
        Some("\"datafusion\"")
    );
    assert_eq!(
        serde_json::to_string(&AnalyticalEngineId::DuckDb)
            .ok()
            .as_deref(),
        Some("\"duckdb\"")
    );
    assert_eq!(
        serde_json::from_str::<AnalyticalEngineId>("\"datafusion\"").ok(),
        Some(AnalyticalEngineId::DataFusion)
    );
    assert_eq!(
        serde_json::from_str::<AnalyticalEngineId>("\"duckdb\"").ok(),
        Some(AnalyticalEngineId::DuckDb)
    );
    assert!(serde_json::from_str::<AnalyticalEngineId>("\"data_fusion\"").is_err());
    assert_eq!(
        AnalyticalEngineId::DataFusion.into_compute_engine(),
        ComputeEngine::DataFusion
    );
    assert_eq!(
        AnalyticalEngineId::try_from(ComputeEngine::DataFusion),
        Ok(AnalyticalEngineId::DataFusion)
    );
    assert!(AnalyticalEngineId::try_from(ComputeEngine::Cell).is_err());
}

#[test]
fn capabilities_declare_all_six_d136_flags_and_fail_closed() {
    let none = EngineCapabilities::none();
    for cap in Capability::ALL {
        assert!(!none.supports(cap));
        assert!(matches!(
            none.require_capability(cap),
            Err(AnalyticalRefusal::UnsupportedCapability { capability, .. }) if capability == cap
        ));
    }

    let duckdb = EngineCapabilities::duckdb_default();
    assert!(duckdb.exact_bitemporal());
    assert!(!duckdb.live_sub_second());
    assert!(!duckdb.vector());
    assert!(!duckdb.geo());
    assert!(!duckdb.iceberg_zero_copy());
    assert!(!duckdb.markings_enforced());
    assert!(
        duckdb
            .require_capability(Capability::ExactBitemporal)
            .is_ok()
    );

    let encoded = serde_json::to_string(&duckdb).ok();
    assert_eq!(
        encoded.as_deref(),
        Some(
            "{\"exactBitemporal\":true,\"liveSubSecond\":false,\"vector\":false,\"geo\":false,\"icebergZeroCopy\":false,\"markingsEnforced\":false}"
        )
    );
    assert_eq!(
        encoded
            .as_deref()
            .and_then(|raw| serde_json::from_str::<EngineCapabilities>(raw).ok()),
        Some(duckdb)
    );
    assert!(
        serde_json::from_str::<EngineCapabilities>("{\"exactBitemporal\":true,\"extra\":false}")
            .is_err()
    );
    assert!(serde_json::from_str::<EngineCapabilities>("{\"exactBitemporal\":true}").is_err());

    let required_vector = EngineCapabilities::none().with(Capability::Vector, true);
    let refused = duckdb.require(required_vector);
    assert!(matches!(
        refused,
        Err(AnalyticalRefusal::UnsupportedCapability {
            capability: Capability::Vector,
            ..
        })
    ));
    if let Err(err) = refused {
        assert_eq!(err.code(), UNSUPPORTED_ENGINE);
        assert!(matches!(
            err.to_cell_error(),
            CellError::ComputeRefused(code) if code == UNSUPPORTED_ENGINE
        ));
    }
}

#[test]
fn snapshot_provenance_verifies_tenant_world_model_schema_frontier_and_dataset_load() {
    let tenant = TenantId::of_scope("org_demo/dev");
    let other_tenant = TenantId::of_scope("org_other/dev");
    let mirror = Source::Mirror {
        world: "main".to_owned(),
        table: "demo.Order".to_owned(),
        frontier: 7,
        scope: "org_demo/dev".to_owned(),
        excluded: Vec::new(),
        model: Some("model-v1".to_owned()),
    };
    let good = SnapshotProvenance::new(
        AnalyticalEngineId::DataFusion,
        tenant.clone(),
        ReadSnapshot::from_source_with_pins(
            &mirror,
            Some("model-v1".to_owned()),
            Some("pin-v1".to_owned()),
        ),
    )
    .with_freshness(Freshness::Exact);
    assert!(
        good.verify_against(&tenant, &mirror, Some("model-v1"), Some("pin-v1"))
            .is_ok()
    );
    assert!(matches!(
        good.verify_against(&tenant, &mirror, Some("model-v1"), Some("pin-v2")),
        Err(AnalyticalRefusal::StaleRead { .. })
    ));
    assert!(matches!(
        good.verify_against(&tenant, &mirror, Some("model-v2"), Some("pin-v1")),
        Err(AnalyticalRefusal::StaleRead { .. })
    ));
    assert!(matches!(
        good.verify_against(&other_tenant, &mirror, Some("model-v1"), Some("pin-v1")),
        Err(AnalyticalRefusal::StaleRead { .. })
    ));

    let moved_frontier = Source::Mirror {
        world: "main".to_owned(),
        table: "demo.Order".to_owned(),
        frontier: 8,
        scope: "org_demo/dev".to_owned(),
        excluded: Vec::new(),
        model: Some("model-v1".to_owned()),
    };
    assert!(matches!(
        good.verify_against(&tenant, &moved_frontier, Some("model-v1"), Some("pin-v1")),
        Err(AnalyticalRefusal::StaleRead { .. })
    ));

    let moved_model = Source::Mirror {
        world: "main".to_owned(),
        table: "demo.Order".to_owned(),
        frontier: 7,
        scope: "org_demo/dev".to_owned(),
        excluded: Vec::new(),
        model: Some("model-v2".to_owned()),
    };
    assert!(matches!(
        good.verify_against(&tenant, &moved_model, Some("model-v2"), Some("pin-v1")),
        Err(AnalyticalRefusal::StaleRead { .. })
    ));
}

#[test]
fn snapshot_provenance_enforces_live_model_pin_schema_pin_and_dataset_load() {
    let tenant = TenantId::of_scope("org_demo/dev");
    let live_mirror = Source::Mirror {
        world: "main".to_owned(),
        table: "demo.Order".to_owned(),
        frontier: 7,
        scope: "org_demo/dev".to_owned(),
        excluded: Vec::new(),
        model: None,
    };
    let live_missing_model_pin = SnapshotProvenance::new(
        AnalyticalEngineId::DataFusion,
        tenant.clone(),
        ReadSnapshot::from_source_with_schema(&live_mirror, Some("pin-v1".to_owned())),
    )
    .with_freshness(Freshness::Exact);
    assert!(matches!(
        live_missing_model_pin.verify_against(
            &tenant,
            &live_mirror,
            Some("model-v1"),
            Some("pin-v1")
        ),
        Err(AnalyticalRefusal::StaleRead { .. })
    ));

    let missing_schema_pin = SnapshotProvenance::new(
        AnalyticalEngineId::DataFusion,
        tenant.clone(),
        ReadSnapshot::from_source(&live_mirror),
    )
    .with_freshness(Freshness::Exact);
    assert!(matches!(
        missing_schema_pin.verify_against(&tenant, &live_mirror, Some("model-v1"), Some("pin-v1")),
        Err(AnalyticalRefusal::StaleRead { .. })
    ));

    let dataset = Source::Dataset {
        table: "demo.Order".to_owned(),
        load: "load-1".to_owned(),
    };
    assert!(matches!(
        live_missing_model_pin.verify_against(&tenant, &dataset, None, None),
        Err(AnalyticalRefusal::StaleRead { .. })
    ));
    let ds_prov = SnapshotProvenance::new(
        AnalyticalEngineId::DuckDb,
        tenant,
        ReadSnapshot::from_source_with_pins(&dataset, Some("model-v1".to_owned()), None),
    )
    .with_freshness(Freshness::Exact);
    assert!(
        ds_prov
            .verify_against(
                &TenantId::of_scope("org_demo/dev"),
                &dataset,
                Some("model-v1"),
                None
            )
            .is_ok()
    );
    assert!(matches!(
        ds_prov.verify_against(
            &TenantId::of_scope("org_demo/dev"),
            &dataset,
            Some("model-v2"),
            None
        ),
        Err(AnalyticalRefusal::StaleRead { .. })
    ));
}

#[test]
fn verify_profile_provenance_requires_explicit_exact_freshness() {
    let tenant = TenantId::of_scope("org_demo/dev");
    let dataset = Source::Dataset {
        table: "demo.Order".to_owned(),
        load: "load-1".to_owned(),
    };
    let ds_prov = SnapshotProvenance::new(
        AnalyticalEngineId::DuckDb,
        tenant.clone(),
        ReadSnapshot::from_source_with_pins(&dataset, Some("model-v1".to_owned()), None),
    )
    .with_freshness(Freshness::Exact);
    let profile = ProfileResponse {
        rows: 1,
        quarantined: 0,
        columns: Vec::new(),
        provenance: Some(ds_prov),
    };
    assert!(verify_profile_provenance(&profile, &tenant, &dataset, Some("model-v1"), None).is_ok());
    assert!(matches!(
        verify_profile_provenance(&profile, &tenant, &dataset, Some("model-v2"), None),
        Err(AnalyticalRefusal::StaleRead { .. })
    ));

    for non_exact in [
        None,
        Some(Freshness::Fresh),
        Some(Freshness::Stale),
        Some(Freshness::Incomplete),
    ] {
        let mut bad_prov = SnapshotProvenance::new(
            AnalyticalEngineId::DuckDb,
            tenant.clone(),
            ReadSnapshot::from_source_with_pins(&dataset, Some("model-v1".to_owned()), None),
        );
        bad_prov.freshness = non_exact;
        let bad_profile = ProfileResponse {
            rows: 1,
            quarantined: 0,
            columns: Vec::new(),
            provenance: Some(bad_prov),
        };
        assert!(matches!(
            verify_profile_provenance(&bad_profile, &tenant, &dataset, Some("model-v1"), None),
            Err(AnalyticalRefusal::StaleRead { .. })
        ));
    }
}

#[test]
fn strict_law_and_markings_fail_closed_including_missing_engine_freshness() {
    let tenant = TenantId::of_scope("org_demo/dev");
    let mirror = Source::Mirror {
        world: "main".to_owned(),
        table: "demo.Order".to_owned(),
        frontier: 3,
        scope: "org_demo/dev".to_owned(),
        excluded: Vec::new(),
        model: None,
    };
    let exact = SnapshotProvenance::new(
        AnalyticalEngineId::DataFusion,
        tenant.clone(),
        ReadSnapshot::from_source_with_schema(&mirror, Some("pin-1".to_owned())),
    )
    .with_freshness(Freshness::Exact);
    assert!(exact.verify_strict_law_read(Freshness::Exact, true).is_ok());
    assert!(matches!(
        exact.verify_strict_law_read(Freshness::Stale, true),
        Err(AnalyticalRefusal::InsufficientProtection {
            reason: ProtectionReason::StrictLawRequiresExact,
            ..
        })
    ));
    assert!(matches!(
        exact.verify_strict_law_read(Freshness::Exact, false),
        Err(AnalyticalRefusal::InsufficientProtection {
            reason: ProtectionReason::StrictLawRequiresExact,
            ..
        })
    ));

    // Missing freshness (`None`) MUST fail closed on strict law reads.
    let missing_freshness = SnapshotProvenance::new(
        AnalyticalEngineId::DataFusion,
        tenant.clone(),
        ReadSnapshot::from_source_with_schema(&mirror, Some("pin-1".to_owned())),
    );
    assert!(matches!(
        missing_freshness.verify_strict_law_read(Freshness::Exact, true),
        Err(AnalyticalRefusal::InsufficientProtection {
            reason: ProtectionReason::StrictLawRequiresExact,
            ..
        })
    ));

    let dataset_prov = SnapshotProvenance::new(
        AnalyticalEngineId::DuckDb,
        tenant,
        ReadSnapshot::Dataset {
            table: "demo.Order".to_owned(),
            load: "load-1".to_owned(),
            model_pin: None,
        },
    )
    .with_freshness(Freshness::Exact);
    assert!(matches!(
        dataset_prov.verify_strict_law_read(Freshness::Exact, true),
        Err(AnalyticalRefusal::InsufficientProtection {
            reason: ProtectionReason::UnverifiedDatasetForStrictLaw,
            ..
        })
    ));

    let duckdb = EngineCapabilities::duckdb_default();
    assert!(duckdb.verify_markings_protection(false).is_ok());
    assert!(matches!(
        duckdb.verify_markings_protection(true),
        Err(AnalyticalRefusal::InsufficientProtection {
            reason: ProtectionReason::MarkingsNotEnforced,
            ..
        })
    ));
}

#[cfg(feature = "native")]
mod store_tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use norn_checker::EffectiveCatalog;
    use norn_checker::metric::compute::UNSUPPORTED_ENGINE;
    use norn_checker::transform::TransformPlan;
    use norn_checker::transform::eval::{TransformInput, TransformOutput};
    use norn_ir::{
        ArtifactEnvelope, Decimal, MetricDefinition, ObjectTypeDef, QuestionIR, RuntimeValue,
    };
    use serde_json::json;

    use super::*;
    use crate::compute::transform::RowAccess;
    use crate::compute::{QueryContext, Watermark};
    use crate::test_door::TestCommit as _;
    use crate::{CellError, CellStore, CommitBatch, FactWriteRow, MAIN_WORLD, Model};

    struct PassThroughTransform;

    impl AdmittedTransform for PassThroughTransform {
        fn execute(
            &self,
            plan: &TransformPlan,
            input: &TransformInput,
        ) -> Result<TransformOutput, CellError> {
            norn_checker::transform::eval::evaluate(plan, input)
                .map_err(|_err| CellError::ComputeRefused(UNSUPPORTED_ENGINE))
        }
    }

    struct StubAnalyticalEngine {
        id: AnalyticalEngineId,
        capabilities: EngineCapabilities,
        admit_transforms: bool,
        evaluate_calls: AtomicUsize,
    }

    impl StubAnalyticalEngine {
        fn new(
            id: AnalyticalEngineId,
            capabilities: EngineCapabilities,
            admit_transforms: bool,
        ) -> Self {
            Self {
                id,
                capabilities,
                admit_transforms,
                evaluate_calls: AtomicUsize::new(0),
            }
        }

        fn unsupported() -> EngineRefusal {
            EngineRefusal {
                code: UNSUPPORTED_ENGINE.to_owned(),
                detail: "stub engine".to_owned(),
            }
        }
    }

    impl AnalyticalEngine for StubAnalyticalEngine {
        fn engine_id(&self) -> AnalyticalEngineId {
            self.id
        }

        fn capabilities(&self) -> EngineCapabilities {
            self.capabilities
        }

        fn mirror_frontier(&self, _request: &FrontierRequest) -> EngineOutcome<FrontierResponse> {
            Err(Self::unsupported())
        }

        fn mirror_sync(&self, _request: &SyncRequest) -> EngineOutcome<FrontierResponse> {
            Err(Self::unsupported())
        }

        fn evaluate_question(&self, request: &EvaluateRequest) -> EngineOutcome<EvaluateResponse> {
            self.evaluate_calls.fetch_add(1, Ordering::SeqCst);
            Ok(EvaluateResponse {
                population: 0,
                excluded: Vec::new(),
                buckets: Vec::new(),
                unlinked: 0,
                provenance: Some(
                    SnapshotProvenance::new(
                        self.id,
                        request.tenant.clone(),
                        ReadSnapshot::from_source_with_pins(
                            &request.source,
                            request.model_pin.clone(),
                            None,
                        ),
                    )
                    .with_freshness(Freshness::Exact),
                ),
            })
        }

        fn query_members(&self, _request: &MembersRequest) -> EngineOutcome<MembersResponse> {
            Err(Self::unsupported())
        }

        fn load_dataset(
            &self,
            _request: &LoadRequest,
            _file: &Path,
        ) -> EngineOutcome<LoadResponse> {
            Err(Self::unsupported())
        }

        fn land_dataset(
            &self,
            _request: &LandRequest,
            _fetch: &mut dyn FnMut(std::fs::File) -> EngineOutcome<()>,
        ) -> EngineOutcome<LandResponse> {
            Err(Self::unsupported())
        }

        fn settle_dataset(&self, _request: &SettleRequest) -> EngineOutcome<LandResponse> {
            Err(Self::unsupported())
        }

        fn store_dataset(
            &self,
            _request: &StoreRequest,
            _send: &mut dyn FnMut(&Path, &Path) -> EngineOutcome<()>,
        ) -> EngineOutcome<LandResponse> {
            Err(Self::unsupported())
        }

        fn store_dataset_json(
            &self,
            _body: &StoreBody,
            _send: &mut dyn FnMut(&Path, &Path) -> EngineOutcome<()>,
        ) -> EngineOutcome<Box<RawValue>> {
            Err(Self::unsupported())
        }

        fn query_lineage(&self, _request: &LineageRequest) -> EngineOutcome<LineageResponse> {
            Err(Self::unsupported())
        }

        fn query_page(&self, _request: &PageRequest) -> EngineOutcome<PageResponse> {
            Err(Self::unsupported())
        }

        fn profile_objects(&self, _request: &ProfileRequest) -> EngineOutcome<ProfileResponse> {
            Err(Self::unsupported())
        }

        fn export_dataset(&self, _request: &ExportRequest) -> EngineOutcome<PathBuf> {
            Err(Self::unsupported())
        }

        fn admit_transform(
            &self,
            _plan: &TransformPlan,
        ) -> Result<Box<dyn AdmittedTransform>, CellError> {
            if self.admit_transforms {
                Ok(Box::new(PassThroughTransform))
            } else {
                Err(CellError::ComputeRefused(UNSUPPORTED_ENGINE))
            }
        }
    }

    fn order_entity_artifact()
    -> Result<(ObjectTypeDef, ArtifactEnvelope), Box<dyn std::error::Error>> {
        let object: ObjectTypeDef = serde_json::from_value(json!({
            "name": "demo.Order",
            "properties": [
                {"id": "amount", "type": {"kind": "decimal", "scale": 2, "additivity": {"kind": "additive"}}},
                {"id": "occurred_at", "type": {"kind": "timestamp", "timezone": "utc"}}
            ]
        }))?;
        let artifact: ArtifactEnvelope = serde_json::from_value(json!({
            "irVersion": "0.1",
            "artifactId": "demo.Order",
            "kind": "entity",
            "namespace": "demo",
            "name": "demo.Order",
            "version": "1.0.0",
            "lifecycle": "active",
            "scope": {"orgId": "org_demo", "environmentId": "dev"},
            "dependencies": [],
            "provenance": {"source": "authoring_sdk"},
            "payload": {"entityVersion": "2.0", "objectType": object}
        }))?;
        Ok((object, artifact))
    }

    #[test]
    fn evaluate_transform_on_admits_before_reading_seeded_snapshot_and_reports_engine_id()
    -> Result<(), Box<dyn std::error::Error>> {
        let (object, entity) = order_entity_artifact()?;
        let view: ArtifactEnvelope = serde_json::from_value(json!({
            "irVersion": "0.1",
            "artifactId": "demo.OrderScan",
            "kind": "derived",
            "namespace": "demo",
            "name": "demo.OrderScan",
            "version": "1.0.0",
            "lifecycle": "active",
            "scope": {"orgId": "org_demo", "environmentId": "dev"},
            "dependencies": [
                {"artifactId": "demo.Order", "kind": "entity", "version": "1.0.0", "required": true}
            ],
            "provenance": {"source": "authoring_sdk"},
            "payload": {
                "viewVersion": "0.1",
                "viewId": "demo.OrderScan",
                "maintenance": "recompute_on_read",
                "inputs": [
                    {"artifactId": "demo.Order", "kind": "entity", "version": "1.0.0", "alias": "o"}
                ],
                "outputSchema": {},
                "calc": {
                    "kind": "relational",
                    "output": "s",
                    "nodes": {
                        "s": {"op": "scan", "input": "o"}
                    }
                }
            }
        }))?;
        let pool = [entity, view.clone()];
        let plan = norn_checker::transform::admit(&view, &pool, &BTreeSet::new())
            .map_err(|diags| format!("admit failed: {diags:?}"))?;
        let model = Model::from_definitions(&[object], &[])?;
        let mut store = CellStore::open_in_memory(model)?;
        // Seed a readable object so an early snapshot scan would visibly increment `reads`.
        store.commit(&CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "org_demo/dev".to_owned(),
            logical_time: 10_000,
            writes: vec![FactWriteRow::Put {
                object_type: "demo.Order".to_owned(),
                key: "ord-1".to_owned(),
                values: BTreeMap::from([
                    (
                        "amount".to_owned(),
                        RuntimeValue::Number(Decimal::parse("42.00")?),
                    ),
                    (
                        "occurred_at".to_owned(),
                        RuntimeValue::String("2026-02-01T00:00:00Z".to_owned()),
                    ),
                ]),
            }],
            ..CommitBatch::default()
        })?;

        let reads = AtomicUsize::new(0);
        let refusing = StubAnalyticalEngine::new(
            AnalyticalEngineId::DataFusion,
            EngineCapabilities::none(),
            false,
        );
        let refused =
            store.evaluate_transform_on(&refusing, &plan, MAIN_WORLD, "org_demo/dev", |_| {
                reads.fetch_add(1, Ordering::SeqCst);
                Ok(RowAccess::Read)
            });
        assert!(matches!(
            refused,
            Err(CellError::ComputeRefused(code)) if code == UNSUPPORTED_ENGINE
        ));
        assert_eq!(reads.load(Ordering::SeqCst), 0);

        let accepting = StubAnalyticalEngine::new(
            AnalyticalEngineId::DataFusion,
            EngineCapabilities::none(),
            true,
        );
        let result =
            store.evaluate_transform_on(&accepting, &plan, MAIN_WORLD, "org_demo/dev", |_| {
                reads.fetch_add(1, Ordering::SeqCst);
                Ok(RowAccess::Read)
            })?;
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        assert_eq!(result.engine, ComputeEngine::DataFusion);
        Ok(())
    }

    fn order_metric_fixture()
    -> Result<(CellStore, norn_checker::metric::compute::QuestionPlan), Box<dyn std::error::Error>>
    {
        let (object, entity) = order_entity_artifact()?;
        let catalog = EffectiveCatalog::from_artifacts(&[entity]);
        let metric: MetricDefinition = serde_json::from_value(json!({
            "name": "demo.total_amount",
            "label": "Total amount",
            "type": "simple",
            "type_params": {
                "expr": "amount",
                "metric_aggregation_params": {
                    "semantic_model": "demo.Order",
                    "agg": "sum",
                    "agg_time_dimension": "occurred_at"
                }
            },
            "x-norn": {
                "irVersion": "0.1",
                "additivity": {"kind": "additive"},
                "timeDimension": {"property": "occurred_at"},
                "owner": "finance",
                "certified": true
            }
        }))?;
        let question: QuestionIR = serde_json::from_value(json!({
            "irVersion": "0.1",
            "name": "demo.order_total",
            "objectSet": "demo.Order",
            "aggregate": [{"metric": "demo.total_amount"}],
            "window": {
                "start": "2026-01-01T00:00:00Z",
                "end": "2026-12-31T00:00:00Z"
            },
            "freshness": {"kind": "live", "maxAgeMs": 5000},
            "calendar": "iso_utc"
        }))?;
        let plan = norn_checker::metric::compute::admit(&[metric], &question, &catalog, 100)
            .map_err(|diags| format!("admit failed: {diags:?}"))?;
        let model = Model::from_definitions(&[object], &[])?;
        let store = CellStore::open_in_memory(model)?;
        Ok((store, plan))
    }

    fn empty_eval_response(provenance: Option<SnapshotProvenance>) -> EvaluateResponse {
        EvaluateResponse {
            population: 0,
            excluded: Vec::new(),
            buckets: Vec::new(),
            unlinked: 0,
            provenance,
        }
    }

    #[test]
    fn assemble_verifies_provenance_schema_pin_freshness_and_propagates_engine_id()
    -> Result<(), Box<dyn std::error::Error>> {
        let (store, plan) = order_metric_fixture()?;
        let tenant = TenantId::of_scope("org_demo/dev");
        let exact_context = QueryContext {
            world: MAIN_WORLD.to_owned(),
            scope: "org_demo/dev".to_owned(),
            population_pin: "test".to_owned(),
            authorization_epoch: 1,
            mapping_pin: "test".to_owned(),
            now_ms: 10_000,
            source_cursors: BTreeMap::new(),
            watermarks: BTreeMap::new(),
        };

        // Missing provenance refuses fail-closed.
        let prepared = store.analytics_prepare(&plan, exact_context.clone(), &tenant)?;
        assert!(matches!(
            store.analytics_answer(prepared, empty_eval_response(None)),
            Err(CellError::ComputeRefused(code)) if code == STALE_INPUT_CODE
        ));

        // Missing model pin on live mirror provenance refuses fail-closed.
        let prepared = store.analytics_prepare(&plan, exact_context.clone(), &tenant)?;
        assert_eq!(
            prepared.request().model_pin.as_deref(),
            Some(store.model().model_hash())
        );
        let no_model_pin = SnapshotProvenance::new(
            AnalyticalEngineId::DataFusion,
            tenant.clone(),
            ReadSnapshot::from_source_with_schema(
                &prepared.request().source,
                prepared.schema_pin().map(str::to_owned),
            ),
        )
        .with_freshness(Freshness::Exact);
        assert!(matches!(
            store.analytics_answer(prepared, empty_eval_response(Some(no_model_pin))),
            Err(CellError::ComputeRefused(code)) if code == STALE_INPUT_CODE
        ));

        // Mismatched model pin on live mirror provenance refuses fail-closed.
        let prepared = store.analytics_prepare(&plan, exact_context.clone(), &tenant)?;
        let wrong_model_pin = SnapshotProvenance::new(
            AnalyticalEngineId::DataFusion,
            tenant.clone(),
            ReadSnapshot::from_source_with_pins(
                &prepared.request().source,
                Some("stale-model-hash".to_owned()),
                prepared.schema_pin().map(str::to_owned),
            ),
        )
        .with_freshness(Freshness::Exact);
        assert!(matches!(
            store.analytics_answer(prepared, empty_eval_response(Some(wrong_model_pin))),
            Err(CellError::ComputeRefused(code)) if code == STALE_INPUT_CODE
        ));

        // Missing schema pin on mirror provenance refuses fail-closed.
        let prepared = store.analytics_prepare(&plan, exact_context.clone(), &tenant)?;
        let no_schema_pin = SnapshotProvenance::new(
            AnalyticalEngineId::DataFusion,
            tenant.clone(),
            ReadSnapshot::from_source_with_pins(
                &prepared.request().source,
                prepared.request().model_pin.clone(),
                None,
            ),
        )
        .with_freshness(Freshness::Exact);
        assert!(matches!(
            store.analytics_answer(prepared, empty_eval_response(Some(no_schema_pin))),
            Err(CellError::ComputeRefused(code)) if code == STALE_INPUT_CODE
        ));

        // Mismatched tenant in provenance refuses fail-closed.
        let prepared = store.analytics_prepare(&plan, exact_context, &tenant)?;
        let wrong_tenant = SnapshotProvenance::new(
            AnalyticalEngineId::DataFusion,
            TenantId::of_scope("org_other/dev"),
            ReadSnapshot::from_source_with_pins(
                &prepared.request().source,
                prepared.request().model_pin.clone(),
                prepared.schema_pin().map(str::to_owned),
            ),
        )
        .with_freshness(Freshness::Exact);
        assert!(matches!(
            store.analytics_answer(prepared, empty_eval_response(Some(wrong_tenant))),
            Err(CellError::ComputeRefused(code)) if code == STALE_INPUT_CODE
        ));
        Ok(())
    }

    #[test]
    fn assemble_strict_and_empty_authorized_answers_propagate_engine_id()
    -> Result<(), Box<dyn std::error::Error>> {
        let (store, plan) = order_metric_fixture()?;
        let tenant = TenantId::of_scope("org_demo/dev");
        let exact_context = QueryContext {
            world: MAIN_WORLD.to_owned(),
            scope: "org_demo/dev".to_owned(),
            population_pin: "test".to_owned(),
            authorization_epoch: 1,
            mapping_pin: "test".to_owned(),
            now_ms: 10_000,
            source_cursors: BTreeMap::new(),
            watermarks: BTreeMap::new(),
        };

        // Strict law read refuses missing engine freshness (`freshness: None`).
        let prepared = store.analytics_prepare(&plan, exact_context.clone(), &tenant)?;
        let missing_freshness = SnapshotProvenance::new(
            AnalyticalEngineId::DataFusion,
            tenant.clone(),
            ReadSnapshot::from_source_with_pins(
                &prepared.request().source,
                prepared.request().model_pin.clone(),
                prepared.schema_pin().map(str::to_owned),
            ),
        );
        assert!(matches!(
            store.analytics_answer_strict(prepared, empty_eval_response(Some(missing_freshness))),
            Err(CellError::ComputeRefused(code)) if code == STALE_INPUT_CODE
        ));

        // Valid DataFusion provenance with exact freshness propagates Engine::DataFusion.
        let prepared = store.analytics_prepare(&plan, exact_context.clone(), &tenant)?;
        let valid_prov = SnapshotProvenance::new(
            AnalyticalEngineId::DataFusion,
            tenant.clone(),
            ReadSnapshot::from_source_with_pins(
                &prepared.request().source,
                prepared.request().model_pin.clone(),
                prepared.schema_pin().map(str::to_owned),
            ),
        )
        .with_freshness(Freshness::Exact);
        let result =
            store.analytics_answer_strict(prepared, empty_eval_response(Some(valid_prov)))?;
        assert_eq!(result.engine, ComputeEngine::DataFusion);
        assert_eq!(result.freshness, Freshness::Exact);

        // Population-denied path uses explicit cell-produced empty answer (Engine::Cell).
        let prepared = store.analytics_prepare(&plan, exact_context, &tenant)?;
        let empty_answer = store.analytics_empty_authorized_answer(prepared)?;
        assert_eq!(empty_answer.engine, ComputeEngine::Cell);
        assert!(empty_answer.rows.is_empty());
        Ok(())
    }

    #[test]
    fn dispatch_evaluate_enforces_exact_bitemporal_before_executing_historical_read()
    -> Result<(), Box<dyn std::error::Error>> {
        let (store, plan) = order_metric_fixture()?;
        let tenant = TenantId::of_scope("org_demo/dev");
        let context = QueryContext {
            world: MAIN_WORLD.to_owned(),
            scope: "org_demo/dev".to_owned(),
            population_pin: "test".to_owned(),
            authorization_epoch: 1,
            mapping_pin: "test".to_owned(),
            now_ms: 10_000,
            source_cursors: BTreeMap::new(),
            watermarks: BTreeMap::new(),
        };
        let cutoff = crate::KnowledgeCutoff {
            frontier: 0,
            recorded_at: norn_kernel::LogicalTime::from_millis(10_000),
        };
        let prepared = store.analytics_prepare_known_at(&plan, context, &tenant, cutoff)?;

        // Engine lacking `exact_bitemporal` refuses BEFORE executing `evaluate_question`.
        let incapable = StubAnalyticalEngine::new(
            AnalyticalEngineId::DataFusion,
            EngineCapabilities::none(),
            true,
        );
        let refused = dispatch_evaluate(&incapable, prepared.request());
        assert!(matches!(
            refused,
            Err(EngineRefusal { ref code, .. }) if code == UNSUPPORTED_ENGINE
        ));
        assert_eq!(incapable.evaluate_calls.load(Ordering::SeqCst), 0);

        // Engine qualifying for `exact_bitemporal` dispatches normally.
        let capable = StubAnalyticalEngine::new(
            AnalyticalEngineId::DataFusion,
            EngineCapabilities::none().with(Capability::ExactBitemporal, true),
            true,
        );
        let answered = dispatch_evaluate(&capable, prepared.request())
            .map_err(|err| format!("unexpected refusal: {err:?}"))?;
        assert_eq!(capable.evaluate_calls.load(Ordering::SeqCst), 1);
        assert!(answered.provenance.is_some());
        Ok(())
    }

    #[test]
    fn assemble_strict_law_read_refuses_stale_watermark() -> Result<(), Box<dyn std::error::Error>>
    {
        let (store, plan) = order_metric_fixture()?;
        let tenant = TenantId::of_scope("org_demo/dev");
        let stale_context = QueryContext {
            world: MAIN_WORLD.to_owned(),
            scope: "org_demo/dev".to_owned(),
            population_pin: "test".to_owned(),
            authorization_epoch: 1,
            mapping_pin: "test".to_owned(),
            now_ms: 50_000,
            source_cursors: BTreeMap::from([(
                "erp".to_owned(),
                BTreeMap::from([("orders".to_owned(), "c1".to_owned())]),
            )]),
            watermarks: BTreeMap::from([(
                "erp".to_owned(),
                BTreeMap::from([(
                    "orders".to_owned(),
                    Watermark {
                        observed: "c1".to_owned(),
                        heartbeat_ms: Some(1_000),
                        complete: true,
                    },
                )]),
            )]),
        };
        let prepared = store.analytics_prepare(&plan, stale_context, &tenant)?;
        let prov = SnapshotProvenance::new(
            AnalyticalEngineId::DataFusion,
            tenant,
            ReadSnapshot::from_source_with_pins(
                &prepared.request().source,
                prepared.request().model_pin.clone(),
                prepared.schema_pin().map(str::to_owned),
            ),
        )
        .with_freshness(Freshness::Exact);
        assert!(matches!(
            store.analytics_answer_strict(prepared, empty_eval_response(Some(prov))),
            Err(CellError::ComputeRefused(code)) if code == STALE_INPUT_CODE
        ));
        Ok(())
    }
}
