//! FEED-MODE-1 end-to-end regressions (Lane B4):
//!
//! 1. Pull `FeedMode::Patch` preserves absent JSON keys while keeping explicit
//!    `null`, stamps `"feed_mode": "patch"` on sealed observations, and leaves
//!    `FeedMode::FullSnapshot` wire envelopes byte-identical.
//! 2. Pull `VersionKind::Uint64` and `VersionKind::Rfc3339` compute exact typed
//!    watermarks (`"100" > "99"`, timezone-offset instants) and refuse
//!    `VersionKind::Opaque`, malformed versions, and regressed keyset versions.
//! 3. End-to-end cell ingestion of `FullSnapshot` followed by `Patch` updates
//!    preserves omitted properties & links, clears explicit `null` properties
//!    and nullable links, quarantines incomplete patches on brand-new objects,
//!    and fails closed on partially supplied multi-field expressions.
//! 4. Differential check: end-to-end cell object state agrees with pure
//!    `norn_ir::feed::reduce_object` / `compose_object_patches`.

#[cfg(not(target_arch = "wasm32"))]
#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "FEED-MODE-1 integration regressions"
)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;

    use norn_api::host::DevHost;
    use norn_api::pull::{
        Feed, Limits, Method, Paging, Pull, PullError, PullProfile, RequestTemplate, Watermark,
        observation,
    };
    use norn_api::scope::CellScope;
    use norn_cell::engine::NativeEngine;
    use norn_cell::mapping_bundle::MappingInput;
    use norn_cell::{CellStore, InstalledDefinitionInput, MAIN_WORLD, MappingOutcome, Model};
    use norn_checker::EntityArtifactSetContext;
    use norn_checker::mapping::{SourceDocument, compile_mapping_artifact};
    use norn_checker::receipt::ReceiptSources;
    use norn_ir::feed::{FeedMode, FieldPatch, VersionKind, compose_object_patches, reduce_object};
    use norn_ir::{ArtifactEnvelope, ArtifactKind, EntityRoot, ResourceLimitProfile, RuntimeValue};
    use norn_ui_checker::check_artifact_set_with_receipt;
    use serde_json::{Value, json};

    const RECORDED_AT: &str = "2026-11-12T12:00:00Z";
    const T0: u64 = 1_794_484_800_000;

    fn root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn load_artifact(path: &str) -> ArtifactEnvelope {
        serde_json::from_str(&std::fs::read_to_string(root().join(path)).expect(path)).expect(path)
    }

    fn erpnext_artifacts() -> Vec<ArtifactEnvelope> {
        let mut set: Vec<ArtifactEnvelope> = serde_json::from_str(
            &std::fs::read_to_string(
                root().join("fixtures/demo/explorer/three-systems.definitions.json"),
            )
            .expect("retained catalog"),
        )
        .expect("artifacts");
        for kind in ["source", "mapping", "connector"] {
            set.push(load_artifact(&format!(
                "fixtures/packs/erpnext-rest/artifacts/erpnext.rest.{kind}.json"
            )));
        }
        set
    }

    fn open_host(artifacts: &[ArtifactEnvelope]) -> DevHost<NativeEngine> {
        let (base, candidate): (Vec<_>, Vec<_>) = artifacts
            .iter()
            .cloned()
            .partition(|a| a.artifact_id.starts_with("core.") || a.artifact_id.starts_with("std."));
        let to_rv = |set: &[ArtifactEnvelope]| -> Vec<RuntimeValue> {
            set.iter()
                .map(|a| {
                    RuntimeValue::from_json(serde_json::to_value(a).expect("wire")).expect("rv")
                })
                .collect()
        };
        let sources = ReceiptSources::new(&to_rv(&candidate), &to_rv(&base)).expect("sources");
        let checked = check_artifact_set_with_receipt(
            &sources,
            &EntityArtifactSetContext {
                registry: &BTreeSet::new(),
                base: &base,
                tenant: None,
            },
            &ResourceLimitProfile::norn_limits_v1(),
        )
        .expect("checked");
        assert!(checked.receipt.admitted, "{:#?}", checked.diagnostics);
        let (mut objects, mut interfaces, mut properties) = (Vec::new(), Vec::new(), Vec::new());
        for artifact in artifacts
            .iter()
            .filter(|a| a.kind() == ArtifactKind::Entity)
        {
            let root: EntityRoot = serde_json::from_value(
                serde_json::to_value(artifact.payload.to_json_value()).expect("payload"),
            )
            .expect("entity root");
            match root {
                EntityRoot::V2(root) => objects.push(root.object_type),
                EntityRoot::V2Interface(root) => interfaces.push(root.interface),
                EntityRoot::V2PropertyTypes(root) => properties.extend(root.property_types),
                EntityRoot::V2Extension(_) | EntityRoot::V2Markings(_) | EntityRoot::V1(_) => {
                    panic!("unexpected entity root")
                }
            }
        }
        let model = Model::from_definitions_with_interfaces(&objects, &properties, &interfaces)
            .expect("model")
            .with_governing_laws(artifacts)
            .expect("laws");
        let input = InstalledDefinitionInput {
            canonical_set: String::from_utf8(
                RuntimeValue::from_json(serde_json::to_value(artifacts).expect("set"))
                    .expect("rv")
                    .canonical_json()
                    .expect("canonical"),
            )
            .expect("utf8"),
            registry: vec![],
            receipt_sha256: checked.receipt.content_sha256().expect("digest"),
            admission_receipt: Some(
                String::from_utf8(checked.receipt.canonical_json().expect("receipt"))
                    .expect("utf8"),
            ),
            readmission: None,
            diff: json!({}),
            installed_at: 1,
            layout: None,
        };
        let scope = CellScope {
            org_id: "org_demo".into(),
            environment_id: "dev".into(),
        };
        let mut store = CellStore::open_in_memory(model.clone()).expect("store");
        store
            .install_definitions(model, input, Some(0), &scope.key())
            .expect("install");
        DevHost::from_store(store, scope, |_| BTreeMap::new()).expect("host")
    }

    fn holding_profile(mode: FeedMode, version_kind: VersionKind) -> PullProfile {
        let connector =
            load_artifact("fixtures/packs/erpnext-rest/artifacts/erpnext.rest.connector.json");
        let payload = serde_json::to_value(connector.payload.to_json_value()).expect("payload");
        let mut profile = PullProfile::from_connector(&payload)
            .expect("valid")
            .expect("pull profile");
        let mut feed = profile
            .feeds
            .into_iter()
            .find(|f| f.dataset == "Warehouse Lot Balance")
            .expect("holding feed");
        feed.mode = mode;
        feed.version_kind = version_kind;
        if version_kind == VersionKind::Uint64 {
            feed.initial.version = "0".into();
        } else if version_kind == VersionKind::Rfc3339 {
            feed.initial.version = "1970-01-01T00:00:00Z".into();
        }
        profile.feeds = vec![feed];
        profile
    }

    fn mapping_input(dataset: &str) -> MappingInput {
        MappingInput {
            mapping: "erpnext.rest.mapping".into(),
            dataset: dataset.into(),
            connector_instance: "sandbox".into(),
            partition: dataset.into(),
            recorded_at: norn_ir::provenance::UtcTimestamp(RECORDED_AT.into()),
        }
    }

    #[test]
    fn pull_patch_mode_preserves_absent_keys_and_explicit_nulls() {
        let patch_profile = holding_profile(FeedMode::Patch, VersionKind::Lexicographical);
        let mut pull = Pull::new(patch_profile.clone(), BTreeMap::new(), Limits::default());
        let _req = pull.next_request().expect("request");
        let body = json!({
            "data": [{
                "name": "WLB-00001",
                "modified": "2026-11-12 18:00:00.000000",
                "custom_norn_ref": "demo/holding/owner/north/gold",
                "quantity": 125000.0,
                "valuation_method": null
            }]
        });
        let rows = pull
            .accept(200, body.to_string().as_bytes())
            .expect("accept patch page");
        assert_eq!(rows.len(), 1);
        let RuntimeValue::Object(ref fields) = rows[0].payload else {
            panic!("expected payload object");
        };
        assert!(fields.contains_key("quantity"));
        assert_eq!(fields.get("valuation_method"), Some(&RuntimeValue::Null));
        assert!(
            !fields.contains_key("location"),
            "omitted keys must stay absent in Patch mode"
        );
        assert!(
            !fields.contains_key("cost_currency"),
            "omitted keys must stay absent in Patch mode"
        );
        let obs = observation(&patch_profile, &rows[0], T0).expect("observation");
        assert_eq!(
            obs.input.get("feed_mode").and_then(RuntimeValue::as_str),
            Some("patch")
        );

        let full_profile = holding_profile(FeedMode::FullSnapshot, VersionKind::Lexicographical);
        let mut full_pull = Pull::new(full_profile.clone(), BTreeMap::new(), Limits::default());
        let _req = full_pull.next_request().expect("request");
        let full_rows = full_pull
            .accept(200, body.to_string().as_bytes())
            .expect("accept full page");
        let RuntimeValue::Object(ref full_fields) = full_rows[0].payload else {
            panic!("expected payload object");
        };
        assert_eq!(
            full_fields.get("location"),
            Some(&RuntimeValue::Null),
            "FullSnapshot keeps legacy null projection for absent pointers"
        );
        let full_obs = observation(&full_profile, &full_rows[0], T0).expect("full obs");
        assert!(full_obs.input.get("feed_mode").is_none());
    }

    #[test]
    fn pull_typed_version_kind_orders_uint64_rfc3339_and_refuses_opaque_or_regression() {
        let mut uint_profile = holding_profile(FeedMode::Patch, VersionKind::Uint64);
        uint_profile.feeds[0].paging = Paging::Cursor {
            size: 10,
            next_cursor_path: "/next".into(),
            has_next_path: "/more".into(),
        };
        uint_profile.validate().expect("valid uint64 profile");
        let mut pull = Pull::new(uint_profile, BTreeMap::new(), Limits::default());
        let _req = pull.next_request().expect("req");
        let body = json!({
            "data": [
                {"name": "WLB-1", "modified": 100, "custom_norn_ref": "demo/h/1", "quantity": 10},
                {"name": "WLB-2", "modified": "99", "custom_norn_ref": "demo/h/2", "quantity": 20}
            ],
            "more": false
        });
        let rows = pull
            .accept(200, body.to_string().as_bytes())
            .expect("uint64");
        assert_eq!(rows.len(), 2);
        assert_eq!(
            pull.watermarks()
                .get("Warehouse Lot Balance")
                .map(|w| w.version.as_str()),
            Some("100"),
            "uint64 greatest version must pick 100 over 99"
        );

        let mut rfc_profile = holding_profile(FeedMode::Patch, VersionKind::Rfc3339);
        rfc_profile.feeds[0].paging = Paging::Cursor {
            size: 10,
            next_cursor_path: "/next".into(),
            has_next_path: "/more".into(),
        };
        rfc_profile.validate().expect("valid rfc3339 profile");
        let mut rfc_pull = Pull::new(rfc_profile, BTreeMap::new(), Limits::default());
        let _req = rfc_pull.next_request().expect("req");
        let rfc_body = json!({
            "data": [
                {"name": "WLB-1", "modified": "2026-11-12T10:00:00+02:00", "custom_norn_ref": "demo/h/1"},
                {"name": "WLB-2", "modified": "2026-11-12T08:30:00Z", "custom_norn_ref": "demo/h/2"}
            ],
            "more": false
        });
        rfc_pull
            .accept(200, rfc_body.to_string().as_bytes())
            .expect("rfc3339");
        assert_eq!(
            rfc_pull
                .watermarks()
                .get("Warehouse Lot Balance")
                .map(|w| w.version.as_str()),
            Some("2026-11-12T08:30:00Z"),
            "08:30Z is later than 10:00+02:00 (=08:00Z)"
        );
    }

    #[test]
    fn pull_typed_version_refuses_opaque_regression_and_bad_patch_text() {
        // Opaque refuses validation on ordered feeds.
        let mut opaque_profile = holding_profile(FeedMode::Patch, VersionKind::Opaque);
        opaque_profile.feeds[0].initial.version = "tok-0".into();
        assert!(matches!(
            opaque_profile.validate(),
            Err(PullError::Profile(_))
        ));

        // Keyset version moving backward (either against saved watermark or
        // intra-page `[101, 100]` after `99`) refuses with PullError::Bound.
        let keyset_uint = holding_profile(FeedMode::Patch, VersionKind::Uint64);
        let initial_wm = BTreeMap::from([(
            "Warehouse Lot Balance".to_owned(),
            Watermark {
                version: "99".into(),
                id: Value::from("WLB-0"),
            },
        )]);
        let mut reg_pull = Pull::new(keyset_uint.clone(), initial_wm.clone(), Limits::default());
        let _req = reg_pull.next_request().expect("req");
        let reg_body = json!({
            "data": [
                {"name": "WLB-1", "modified": "101", "custom_norn_ref": "demo/h/1"},
                {"name": "WLB-2", "modified": "100", "custom_norn_ref": "demo/h/2"}
            ]
        });
        assert!(matches!(
            reg_pull.accept(200, reg_body.to_string().as_bytes()),
            Err(PullError::Bound { .. })
        ));

        // Patch `text:` projection refuses non-null non-text values (empty string,
        // bool, object) instead of converting them into explicit null clears.
        let mut text_profile = keyset_uint;
        text_profile.feeds[0]
            .fields
            .insert("valuation_method".into(), "text:/valuation_method".into());
        for bad_val in [json!(""), json!(true), json!({"nested": 1})] {
            let mut p = Pull::new(text_profile.clone(), initial_wm.clone(), Limits::default());
            let _req = p.next_request().expect("req");
            let bad_body = json!({
                "data": [{
                    "name": "WLB-1", "modified": "100",
                    "custom_norn_ref": "demo/h/1", "valuation_method": bad_val
                }]
            });
            assert!(
                matches!(
                    p.accept(200, bad_body.to_string().as_bytes()),
                    Err(PullError::Shape { .. })
                ),
                "non-null non-text value {bad_val} must refuse under text: in Patch mode"
            );
        }
    }

    fn seed_full_erpnext_snapshot(host: &DevHost<NativeEngine>) {
        let recorded: Value = serde_json::from_str(
            &std::fs::read_to_string(
                root().join("fixtures/connectors/rest/erpnext.responses.json"),
            )
            .expect("responses"),
        )
        .expect("json");
        let connector =
            load_artifact("fixtures/packs/erpnext-rest/artifacts/erpnext.rest.connector.json");
        let full_profile = PullProfile::from_connector(
            &serde_json::to_value(connector.payload.to_json_value()).expect("payload"),
        )
        .expect("valid")
        .expect("profile");
        let mut full_pull = Pull::new(full_profile.clone(), BTreeMap::new(), Limits::default());
        let pages: BTreeMap<String, Value> = recorded["pages"]
            .as_array()
            .expect("pages")
            .iter()
            .map(|p| {
                (
                    p["dataset"].as_str().expect("ds").to_owned(),
                    p["body"].clone(),
                )
            })
            .collect();
        while let Some(req) = full_pull.next_request() {
            let body = pages.get(&req.dataset).expect("page");
            let rows = full_pull
                .accept(200, body.to_string().as_bytes())
                .expect("accept");
            let batch: Vec<_> = rows
                .iter()
                .map(|row| {
                    (
                        observation(&full_profile, row, T0).expect("obs"),
                        mapping_input(&row.dataset),
                    )
                })
                .collect();
            let out = host.ingest(&batch).expect("ingest full snapshot");
            assert!(
                out.iter().all(|r| r.outcome == MappingOutcome::Landed),
                "initial snapshot must land: {out:?}"
            );
        }
    }

    fn ingest_patch_row(
        host: &DevHost<NativeEngine>,
        profile: &PullProfile,
        row_json: &Value,
        now_ms: u64,
    ) -> MappingOutcome {
        let mut pull = Pull::new(profile.clone(), BTreeMap::new(), Limits::default());
        let _req = pull.next_request().expect("req");
        let body = json!({ "data": [row_json] });
        let rows = pull
            .accept(200, body.to_string().as_bytes())
            .expect("accept patch");
        let out = host
            .ingest(&[(
                observation(profile, &rows[0], now_ms).expect("obs"),
                mapping_input("Warehouse Lot Balance"),
            )])
            .expect("ingest patch");
        out[0].outcome.clone()
    }

    #[test]
    fn cell_ingest_full_snapshot_then_tri_state_patches_and_reducer_equivalence() {
        let host = open_host(&erpnext_artifacts());
        seed_full_erpnext_snapshot(&host);
        let holding_key = "demo/holding/owner/north/gold";
        let before = host
            .host_object("canon.Holding", holding_key)
            .expect("read")
            .expect("holding exists");
        assert_eq!(
            before.get("valuation_method"),
            Some(&RuntimeValue::from("moving_average"))
        );

        let patch_profile = holding_profile(FeedMode::Patch, VersionKind::Lexicographical);
        // Patch 1: update ONLY quantity; omit location, valuation_method, cost_currency, links.
        let p1 = json!({
            "name": "WLB-00001", "modified": "2026-11-12 18:00:00.000000",
            "custom_norn_ref": holding_key, "quantity": 125_000.0
        });
        assert_eq!(
            ingest_patch_row(&host, &patch_profile, &p1, T0.saturating_add(1)),
            MappingOutcome::Landed
        );
        let after_p1 = host
            .host_object("canon.Holding", holding_key)
            .expect("read")
            .expect("holding");
        assert_eq!(after_p1.get("quantity"), Some(&RuntimeValue::from(125_000)));
        assert_eq!(after_p1.get("location"), Some(&RuntimeValue::from("north")));
        assert_eq!(
            after_p1.get("valuation_method"),
            Some(&RuntimeValue::from("moving_average"))
        );

        // Patch 2: explicit null clears valuation_method and nullable owner link only.
        let p2 = json!({
            "name": "WLB-00001", "modified": "2026-11-12 19:00:00.000000",
            "custom_norn_ref": holding_key, "valuation_method": null, "owner_ref": null
        });
        assert_eq!(
            ingest_patch_row(&host, &patch_profile, &p2, T0.saturating_add(2)),
            MappingOutcome::Landed
        );
        let after_p2 = host
            .host_object("canon.Holding", holding_key)
            .expect("read")
            .expect("holding");
        assert_eq!(after_p2.get("valuation_method"), Some(&RuntimeValue::Null));

        // Item link survived both patches while owner link was explicitly cleared.
        let cell = host.cell().expect("cell");
        let reader = cell.store().read();
        assert_eq!(
            reader
                .links_from(MAIN_WORLD, "canon.Holding", "item", holding_key)
                .expect("item links"),
            vec!["demo/item/gold".to_owned()]
        );
        let owners = reader
            .links_from(MAIN_WORLD, "canon.Holding", "owner", holding_key)
            .expect("owner links");
        assert!(
            owners.is_empty(),
            "explicit owner_ref: null must clear owner link: {owners:?}"
        );
        drop(cell);

        // Differential check against pure norn_ir::feed::reduce_object / compose_object_patches.
        let fp1 = BTreeMap::from([
            (
                "quantity".to_owned(),
                FieldPatch::Value(RuntimeValue::from(125_000)),
            ),
            ("valuation_method".to_owned(), FieldPatch::Absent),
            ("location".to_owned(), FieldPatch::Absent),
        ]);
        let fp2 = BTreeMap::from([
            ("quantity".to_owned(), FieldPatch::Absent),
            ("valuation_method".to_owned(), FieldPatch::ExplicitNull),
            ("location".to_owned(), FieldPatch::Absent),
        ]);
        let expected = reduce_object(Some(&before), &compose_object_patches(&fp1, &fp2));
        for field in ["quantity", "valuation_method", "location", "cost_currency"] {
            assert_eq!(after_p2.get(field), expected.get(field), "field {field}");
        }
    }

    #[test]
    fn patch_creating_incomplete_brand_new_object_quarantines() {
        let host = open_host(&erpnext_artifacts());
        let patch_profile = holding_profile(FeedMode::Patch, VersionKind::Lexicographical);
        let incomplete = json!({
            "name": "WLB-BRAND-NEW", "modified": "2026-11-12 20:00:00.000000",
            "custom_norn_ref": "demo/holding/brand/new", "quantity": 5.0
        });
        let outcome = ingest_patch_row(&host, &patch_profile, &incomplete, T0.saturating_add(3));
        assert!(
            matches!(outcome, MappingOutcome::Quarantined(_)),
            "incomplete patch creating brand-new object must quarantine: {outcome:?}"
        );
    }

    #[test]
    fn patch_omitting_one_operand_of_multi_field_expression_fails_closed() {
        let artifacts = erpnext_artifacts();
        let source =
            load_artifact("fixtures/packs/erpnext-rest/artifacts/erpnext.rest.source.json");
        let mut mapping_doc = serde_json::to_value(load_artifact(
            "fixtures/packs/erpnext-rest/artifacts/erpnext.rest.mapping.json",
        ))
        .expect("mapping json");
        // Make `resource_code` read BOTH `input.resource_code` and `input.location`.
        let maps = mapping_doc["payload"]["maps"].as_array_mut().expect("maps");
        let holding_map = maps
            .iter_mut()
            .find(|m| m["source"]["dataset"] == "Warehouse Lot Balance")
            .expect("holding map");
        let props = holding_map["properties"].as_array_mut().expect("props");
        let rc = props
            .iter_mut()
            .find(|p| p["property"] == "resource_code")
            .expect("resource_code");
        rc["value"] = json!("input.resource_code + '-' + input.location");
        let mapping: ArtifactEnvelope = serde_json::from_value(mapping_doc).expect("mapping");

        let compiled = compile_mapping_artifact(&mapping, &source, &artifacts)
            .expect("compile multi-field mapping");
        let partial_doc = SourceDocument::from_json(
            "Warehouse Lot Balance",
            &json!({
                "name": "WLB-00001",
                "custom_norn_ref": "demo/holding/owner/north/gold",
                "resource_code": "GOLD"
            })
            .to_string(),
            None,
        )
        .expect("doc")
        .with_mode(FeedMode::Patch);
        let result = compiled.apply(
            &[partial_doc],
            &norn_ir::provenance::UtcTimestamp(RECORDED_AT.into()),
        );
        assert!(
            result.is_err(),
            "patch supplying only one operand of multi-field expression must fail closed"
        );
    }

    #[test]
    fn uint64_version_100_patch_supersedes_version_99_link_claim_and_clears_link() {
        let host = open_host(&erpnext_artifacts());
        seed_full_erpnext_snapshot(&host);
        let uint_full = holding_profile(FeedMode::FullSnapshot, VersionKind::Uint64);
        let uint_patch = holding_profile(FeedMode::Patch, VersionKind::Uint64);
        let key = "demo/holding/uint-link-test";
        let v99 = json!({
            "name": "WLB-UINT-1", "modified": "99", "custom_norn_ref": key,
            "resource_code": "GOLD", "quantity": 10.0, "unit": "g", "location": "north",
            "item_ref": "demo/item/gold", "owner_ref": "demo/party/owner",
            "custodian_ref": "demo/party/custodian", "stock_cost": 100.0,
            "cost_currency": "USD", "valuation_method": "moving_average"
        });
        assert_eq!(
            ingest_patch_row(&host, &uint_full, &v99, T0.saturating_add(1)),
            MappingOutcome::Landed
        );
        let v100_clear = json!({
            "name": "WLB-UINT-1", "modified": "100",
            "custom_norn_ref": key, "owner_ref": null
        });
        assert_eq!(
            ingest_patch_row(&host, &uint_patch, &v100_clear, T0.saturating_add(2)),
            MappingOutcome::Landed
        );
        let cell = host.cell().expect("cell");
        let owners = cell
            .store()
            .read()
            .links_from(MAIN_WORLD, "canon.Holding", "owner", key)
            .expect("owner links");
        assert!(
            owners.is_empty(),
            "uint64 version 100 clear must supersede version 99 link claim: {owners:?}"
        );
    }

    #[test]
    fn patch_omitting_nullable_guard_input_fails_closed() {
        let artifacts = erpnext_artifacts();
        let source =
            load_artifact("fixtures/packs/erpnext-rest/artifacts/erpnext.rest.source.json");
        let mut mapping_doc = serde_json::to_value(load_artifact(
            "fixtures/packs/erpnext-rest/artifacts/erpnext.rest.mapping.json",
        ))
        .expect("mapping json");
        let maps = mapping_doc["payload"]["maps"].as_array_mut().expect("maps");
        let holding_map = maps
            .iter_mut()
            .find(|m| m["source"]["dataset"] == "Warehouse Lot Balance")
            .expect("holding map");
        // Guard reads nullable `valuation_method`; omitting it on a Patch must
        // refuse rather than evaluating `input.valuation_method == null` as true.
        holding_map["when"] = json!("input.valuation_method == null");
        let mapping: ArtifactEnvelope = serde_json::from_value(mapping_doc).expect("mapping");
        let compiled =
            compile_mapping_artifact(&mapping, &source, &artifacts).expect("compile guarded map");
        let omitted_guard_doc = SourceDocument::from_json(
            "Warehouse Lot Balance",
            &json!({
                "name": "WLB-00001",
                "custom_norn_ref": "demo/holding/owner/north/gold",
                "quantity": 10.0
            })
            .to_string(),
            None,
        )
        .expect("doc")
        .with_mode(FeedMode::Patch);
        let result = compiled.apply(
            &[omitted_guard_doc],
            &norn_ir::provenance::UtcTimestamp(RECORDED_AT.into()),
        );
        assert!(
            result.is_err(),
            "patch omitting a nullable guard field must fail closed instead of defaulting to null"
        );
    }

    #[test]
    fn pull_feed_struct_defaults_to_full_snapshot_and_lexicographical() {
        let feed = Feed {
            dataset: "Item".into(),
            request: RequestTemplate {
                method: Method::Get,
                path: "/items".into(),
                query: BTreeMap::new(),
                headers: BTreeMap::new(),
                body: None,
            },
            items_path: "/data".into(),
            paging: Paging::Keyset { size: 10 },
            id_path: "/id".into(),
            version_path: "/updated".into(),
            fields: BTreeMap::from([("id".into(), "/id".into())]),
            initial: Watermark {
                version: "1970-01-01".into(),
                id: Value::from(""),
            },
            mode: FeedMode::FullSnapshot,
            version_kind: VersionKind::Lexicographical,
        };
        let encoded = serde_json::to_value(&feed).expect("encode");
        assert!(encoded.get("mode").is_none());
        assert!(encoded.get("versionKind").is_none());
    }
}
