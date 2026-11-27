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
#[path = "support/hosted_read.rs"]
mod hosted_read;

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

    use super::hosted_read::Mapping;
    use norn_api::authz::{AccessRequest, Authorizer, ReadRequest};
    use norn_api::cell_definitions::plan_definitions;
    use norn_api::definition_routes::DefinitionKinds;
    use norn_api::host::DevHost;
    use norn_api::kernel_set::admit_kernel_set_json;
    use norn_api::object_links::{ObjectLinkParams, get_object_links};
    use norn_api::provision::{TrustedBase, admit_submission};
    use norn_api::pull::{
        Feed, Limits, Method, Paging, Pull, PullError, PullProfile, RequestTemplate, Watermark,
        observation,
    };
    use norn_api::refusal::Refusal;
    use norn_api::scope::{CellScope, ScopeQuery};
    use norn_api::sources::{Sources, Step, Trigger};
    use norn_cell::engine::NativeEngine;
    use norn_cell::mapping_bundle::MappingInput;
    use norn_cell::{CellStore, InstalledDefinitionInput, MappingOutcome, Model};
    use norn_checker::EntityArtifactSetContext;
    use norn_checker::mapping::{SourceDocument, compile_mapping_artifact};
    use norn_checker::receipt::ReceiptSources;
    use norn_ir::feed::{FeedMode, FieldPatch, VersionKind, compose_object_patches, reduce_object};
    use norn_ir::{ArtifactEnvelope, ArtifactKind, EntityRoot, ResourceLimitProfile, RuntimeValue};
    use norn_ui_checker::check_artifact_set_with_receipt;
    use serde_json::{Value, json};

    const RECORDED_AT: &str = "2026-11-12T12:00:00Z";
    const T0: u64 = 1_794_484_800_000;

    struct AllowAll;
    impl Authorizer for AllowAll {
        fn authorize(&self, _: &AccessRequest<'_>) -> Result<(), Refusal> {
            Ok(())
        }
        fn read(&self, _: &ReadRequest<'_>) -> Result<(), Refusal> {
            Ok(())
        }
    }

    fn scope_query() -> ScopeQuery {
        ScopeQuery {
            org_id: Some("org_demo".into()),
            environment_id: Some("dev".into()),
        }
    }

    fn holding_links(host: &DevHost<NativeEngine>, key: &str, role: &str) -> Vec<String> {
        let params = ObjectLinkParams {
            direction: Some("out".into()),
            link: Some(role.into()),
            limit: None,
            after: None,
        };
        get_object_links(
            host,
            &scope_query(),
            Some("admin"),
            "canon.Holding",
            key,
            &params,
            &Mapping,
        )
        .expect("get_object_links")
        .items
        .iter()
        .map(|edge| edge.target.clone())
        .collect()
    }

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

    /// `ERPNext` artifact set with nullable `valuation_method` and `owner_ref`
    /// on `Warehouse Lot Balance` (with `erpnext.rest.mapping` repinned to the
    /// mutated source schema's SHA-256) so explicit `null` clears and nullable
    /// guards conform to the source schema.
    fn erpnext_patch_artifacts() -> Vec<ArtifactEnvelope> {
        let mut set = erpnext_artifacts();
        let source_sha = {
            let source = set
                .iter_mut()
                .find(|a| a.artifact_id == "erpnext.rest.source")
                .expect("source");
            let mut wire = serde_json::to_value(&*source).expect("wire");
            for field in wire["payload"]["fields"].as_array_mut().expect("fields") {
                if field["dataset"] == "Warehouse Lot Balance"
                    && matches!(
                        field["name"].as_str(),
                        Some("valuation_method" | "owner_ref")
                    )
                {
                    field["required"] = json!(false);
                }
            }
            *source = serde_json::from_value(wire).expect("source");
            norn_checker::ArtifactRef::of(source)
                .expect("source ref")
                .sha256
        };
        let mapping = set
            .iter_mut()
            .find(|a| a.artifact_id == "erpnext.rest.mapping")
            .expect("mapping");
        let mut map_wire = serde_json::to_value(&*mapping).expect("mapping wire");
        map_wire["payload"]["source"]["sha256"] = json!(source_sha);
        *mapping = serde_json::from_value(map_wire).expect("mapping");
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
        DevHost::from_store(store, scope, |_| BTreeMap::new())
            .expect("host")
            .with_authorizer(AllowAll)
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
        feed.mode = Some(mode);
        feed.version_kind = Some(version_kind);
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
                "quantity": 125_000.0,
                "valuation_method": null
            }]
        });
        let accepted = pull
            .accept(200, body.to_string().as_bytes())
            .expect("accept patch page");
        let rows = accepted.rows;
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
            .expect("accept full page")
            .rows;
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
        let page = pull
            .accept(200, body.to_string().as_bytes())
            .expect("uint64");
        assert_eq!(page.rows.len(), 2);
        pull.confirm(&page.advance);
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
        let rfc_page = rfc_pull
            .accept(200, rfc_body.to_string().as_bytes())
            .expect("rfc3339");
        rfc_pull.confirm(&rfc_page.advance);
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
                id: RuntimeValue::from("WLB-0"),
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

    fn seed_erpnext_datasets(host: &DevHost<NativeEngine>, skip: Option<&str>) {
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
        let mut pages: BTreeMap<String, Value> = recorded["pages"]
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
            let body = pages
                .remove(&req.dataset)
                .unwrap_or_else(|| json!({ "data": [] }));
            let page = full_pull
                .accept(200, body.to_string().as_bytes())
                .expect("accept");
            full_pull.confirm(&page.advance);
            let rows = page.rows;
            if skip == Some(req.dataset.as_str()) || rows.is_empty() {
                continue;
            }
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

    fn seed_full_erpnext_snapshot(host: &DevHost<NativeEngine>) {
        seed_erpnext_datasets(host, None);
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
            .expect("accept patch")
            .rows;
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
        let host = open_host(&erpnext_patch_artifacts());
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
        assert_eq!(
            holding_links(&host, holding_key, "item"),
            vec!["demo/item/gold".to_owned()]
        );
        let owners = holding_links(&host, holding_key, "owner");
        assert!(
            owners.is_empty(),
            "explicit owner_ref: null must clear owner link: {owners:?}"
        );

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

        let compiled = compile_mapping_artifact(&mapping, &artifacts, &[])
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
        let host = open_host(&erpnext_patch_artifacts());
        seed_full_erpnext_snapshot(&host);
        let mut uint_full = holding_profile(FeedMode::FullSnapshot, VersionKind::Uint64);
        let mut uint_patch = holding_profile(FeedMode::Patch, VersionKind::Uint64);
        uint_full.feeds[0].version_path = "/seq".into();
        uint_patch.feeds[0].version_path = "/seq".into();
        let key = "demo/holding/uint-link-test";
        let v99 = json!({
            "name": "WLB-UINT-1", "seq": 99,
            "modified": "2026-11-12 18:00:00.000000", "custom_norn_ref": key,
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
            "name": "WLB-UINT-1", "seq": 100,
            "custom_norn_ref": key, "owner_ref": null
        });
        assert_eq!(
            ingest_patch_row(&host, &uint_patch, &v100_clear, T0.saturating_add(2)),
            MappingOutcome::Landed
        );
        let owners = holding_links(&host, key, "owner");
        assert!(
            owners.is_empty(),
            "uint64 version 100 clear must supersede version 99 link claim: {owners:?}"
        );
    }

    #[test]
    fn patch_omitting_nullable_guard_input_fails_closed() {
        let artifacts = erpnext_patch_artifacts();
        let mut mapping_doc = serde_json::to_value(
            artifacts
                .iter()
                .find(|a| a.artifact_id == "erpnext.rest.mapping")
                .expect("repinned mapping"),
        )
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
            compile_mapping_artifact(&mapping, &artifacts, &[]).expect("compile guarded map");
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
                id: RuntimeValue::from(""),
            },
            mode: None,
            version_kind: None,
        };
        let encoded = serde_json::to_value(&feed).expect("encode");
        assert!(encoded.get("mode").is_none());
        assert!(encoded.get("versionKind").is_none());
    }

    #[test]
    fn mapping_declared_feed_binds_pull_and_refuses_unbound_or_contradictory_delivery() {
        let mut artifacts = erpnext_patch_artifacts();
        let mapping = artifacts
            .iter_mut()
            .find(|a| a.artifact_id == "erpnext.rest.mapping")
            .expect("mapping");
        let decl_json = json!({
            "feeds": {
                "Warehouse Lot Balance": {
                    "mode": "patch",
                    "versionKind": "uint64"
                }
            }
        });
        let decl: norn_ir::feed::FeedDeclaration =
            serde_json::from_value(decl_json.clone()).expect("decl");
        let mut wire = serde_json::to_value(&*mapping).expect("wire");
        wire["payload"]["extensions"] = json!({ "norn.feed/1": decl_json });
        *mapping = serde_json::from_value(wire).expect("mapping");
        let host = open_host(&artifacts);
        seed_erpnext_datasets(&host, Some("Warehouse Lot Balance"));

        let key = "demo/holding/declared-uint64";
        let v99 = json!({
            "name": "WLB-DECL-1", "seq": "99",
            "modified": "2026-11-12 18:00:00.000000", "custom_norn_ref": key,
            "resource_code": "GOLD", "quantity": 10.0, "unit": "g", "location": "north",
            "item_ref": "demo/item/gold", "owner_ref": "demo/party/owner",
            "custodian_ref": "demo/party/custodian", "stock_cost": 100.0,
            "cost_currency": "USD", "valuation_method": "moving_average"
        });
        // Unbound snapshot/lexical profile omits `feed_mode` and `version_kind`:
        // cell MUST refuse so pull-fabricated nulls never enter a Patch mapping.
        let mut unbound = holding_profile(FeedMode::FullSnapshot, VersionKind::Lexicographical);
        unbound.feeds[0].version_path = "/seq".into();
        let mut unbound_pull = Pull::new(unbound.clone(), BTreeMap::new(), Limits::default());
        let _req = unbound_pull.next_request().expect("req");
        let unbound_rows = unbound_pull
            .accept(200, json!({"data": [v99]}).to_string().as_bytes())
            .expect("accept")
            .rows;
        let unbound_obs = observation(&unbound, &unbound_rows[0], T0.saturating_add(1)).expect("o");
        assert!(
            host.ingest(&[(unbound_obs, mapping_input("Warehouse Lot Balance"))])
                .is_err(),
            "unbound snapshot/lexical envelope must refuse under Patch/Uint64 mapping"
        );
        // Explicit `full_snapshot` / `lexicographical` on connector MUST refuse binding against Patch/Uint64.
        let mut explicit_contradiction = unbound;
        explicit_contradiction.feeds[0].initial.version = "0".into();
        assert!(
            explicit_contradiction.bind_feed_declaration(&decl).is_err(),
            "explicit full_snapshot/lexicographical must not be silently overridden by norn.feed/1"
        );

        // Omitted connector mode/versionKind (`None`) adopts mapping `norn.feed/1` BEFORE pull projection:
        let mut bound = holding_profile(FeedMode::FullSnapshot, VersionKind::Lexicographical);
        bound.feeds[0].mode = None;
        bound.feeds[0].version_kind = None;
        bound.feeds[0].version_path = "/seq".into();
        bound.feeds[0].initial.version = "0".into();
        bound
            .bind_feed_declaration(&decl)
            .expect("bind norn.feed/1 onto omitted connector defaults");
        assert_eq!(bound.feed_mode("Warehouse Lot Balance"), FeedMode::Patch);
        assert_eq!(
            bound.version_kind("Warehouse Lot Balance"),
            VersionKind::Uint64
        );
        assert_eq!(
            ingest_patch_row(&host, &bound, &v99, T0.saturating_add(2)),
            MappingOutcome::Landed
        );
        let v100 = json!({
            "name": "WLB-DECL-1", "seq": 100,
            "custom_norn_ref": key, "owner_ref": null
        });
        assert_eq!(
            ingest_patch_row(&host, &bound, &v100, T0.saturating_add(3)),
            MappingOutcome::Landed
        );
        assert!(
            holding_links(&host, key, "owner").is_empty(),
            "bound uint64 patch 100 must supersede 99 and clear owner link"
        );
    }

    #[test]
    fn keyset_decimal_id_and_rfc3339_equivalent_spelling_bounds() {
        // Fractional numeric id 10 -> 2.5 at a tied version moves backward.
        let keyset_uint = holding_profile(FeedMode::Patch, VersionKind::Uint64);
        let mut dec = Pull::new(keyset_uint.clone(), BTreeMap::new(), Limits::default());
        let _req = dec.next_request().expect("req");
        let bad_ids = json!({
            "data": [
                {"name": 10, "modified": "100", "custom_norn_ref": "demo/h/1"},
                {"name": 2.5, "modified": "100", "custom_norn_ref": "demo/h/2"}
            ]
        });
        assert!(matches!(
            dec.accept(200, bad_ids.to_string().as_bytes()),
            Err(PullError::Bound { .. })
        ));

        // High-precision decimals above 2^53 stay exact across watermark, next request, and advance,
        // while arbitrary fixed JSON literals like `1e100` stay intact (never turned into `null`).
        let mut exact_profile = keyset_uint;
        exact_profile.feeds[0].paging = Paging::Keyset { size: 1 };
        exact_profile.feeds[0].request.method = Method::Post;
        exact_profile.feeds[0].request.body = Some(json!({ "afterId": "{{id}}", "huge": 1e100 }));
        let mut exact = Pull::new(exact_profile, BTreeMap::new(), Limits::default());
        let _req = exact.next_request().expect("req1");
        let p1 = exact
            .accept(
                200,
                br#"{"data":[{"name":9007199254740992.1,"modified":"100","custom_norn_ref":"demo/h/1"}]}"#,
            )
            .expect("accept 9007199254740992.1");
        exact.confirm(&p1.advance);
        let req2 = exact.next_request().expect("req2");
        let req2_body = req2.body.as_deref().expect("body");
        assert!(
            req2_body.contains(r#""afterId":9007199254740992.1"#),
            "next request must preserve exact decimal id without binary64 rounding: {req2_body}"
        );
        let parsed_req2: Value = serde_json::from_str(req2_body).expect("valid json");
        assert_eq!(
            parsed_req2["huge"],
            json!(1e100),
            "fixed out-of-Decimal JSON literal 1e100 must stay intact, never null"
        );
        let p2 = exact
            .accept(
                200,
                br#"{"data":[{"name":9007199254740992.2,"modified":"100","custom_norn_ref":"demo/h/2"}]}"#,
            )
            .expect("9007199254740992.2 advances past 9007199254740992.1");
        exact.confirm(&p2.advance);
        assert_eq!(
            exact.watermarks()["Warehouse Lot Balance"].id,
            RuntimeValue::from_json_text("9007199254740992.2").expect("exact")
        );

        // Equivalent RFC 3339 spelling (+02:00 vs Z) with the same id did not advance.
        let keyset_rfc = holding_profile(FeedMode::Patch, VersionKind::Rfc3339);
        let saved = BTreeMap::from([(
            "Warehouse Lot Balance".to_owned(),
            Watermark {
                version: "2026-11-12T10:00:00+02:00".into(),
                id: RuntimeValue::from("WLB-1"),
            },
        )]);
        let mut rfc = Pull::new(keyset_rfc, saved, Limits::default());
        let _req = rfc.next_request().expect("req");
        let same_instant = json!({
            "data": [{
                "name": "WLB-1",
                "modified": "2026-11-12T08:00:00Z",
                "custom_norn_ref": "demo/h/1"
            }]
        });
        assert!(matches!(
            rfc.accept(200, same_instant.to_string().as_bytes()),
            Err(PullError::Bound { .. })
        ));
    }

    #[test]
    fn connector_initial_watermark_precision_and_null_id() {
        // Bare lossy JSON numbers refuse fail-closed (including Serde-rounded
        // `1000000000000000.1`), explicit `"id": null` stays valid, and exact
        // >53-bit initial IDs use `initial.idDecimal`.
        for bad in [
            r#"{"version":"0","id":9007199254740992.1}"#,
            r#"{"version":"0","id":1000000000000000.1}"#,
            r#"{"version":"0","id":null,"idDecimal":"1"}"#,
            r#"{"version":"0","id":null,"idDecimal":null}"#,
        ] {
            let conn = format!(
                r#"{{"extensions":{{"norn.pull/1":{{"source":"s","pipeline":"p/1","feeds":[{{"dataset":"D","request":{{"method":"GET","path":"/d"}},"itemsPath":"/data","paging":{{"kind":"keyset","size":10}},"idPath":"/id","versionPath":"/v","fields":{{"id":"/id"}},"initial":{bad}}}]}}}}}}"#
            );
            assert!(
                PullProfile::from_connector_json(&conn).is_err(),
                "must refuse invalid initial watermark {bad}"
            );
        }
        let null_conn = r#"{"extensions":{"norn.pull/1":{"source":"s","pipeline":"p/1","feeds":[{"dataset":"D","request":{"method":"GET","path":"/d"},"itemsPath":"/data","paging":{"kind":"keyset","size":10},"idPath":"/id","versionPath":"/v","fields":{"id":"/id"},"initial":{"version":"0","id":null}}]}}}"#;
        assert_eq!(
            PullProfile::from_connector_json(null_conn)
                .expect("null id valid")
                .expect("profile")
                .feeds[0]
                .initial
                .id,
            RuntimeValue::Null
        );
        let exact_conn = r#"{"extensions":{"norn.pull/1":{"source":"s","pipeline":"p/1","feeds":[{"dataset":"D","request":{"method":"GET","path":"/d"},"itemsPath":"/data","paging":{"kind":"keyset","size":10},"idPath":"/id","versionPath":"/v","fields":{"id":"/id"},"initial":{"version":"0","idDecimal":"9007199254740992.1"}}]}}}"#;
        let loaded = PullProfile::from_connector_json(exact_conn)
            .expect("valid")
            .expect("profile");
        assert_eq!(
            loaded.feeds[0].initial.id,
            RuntimeValue::from_json_text("9007199254740992.1").expect("exact initial id")
        );
    }

    #[test]
    fn cursor_watermark_never_regresses_and_keyset_checks_lexical_and_id_order() {
        // Cursor feed with saved watermark "100" receiving only older row "99"
        // must keep watermark "100".
        let mut cursor_uint = holding_profile(FeedMode::Patch, VersionKind::Uint64);
        cursor_uint.feeds[0].paging = Paging::Cursor {
            size: 10,
            next_cursor_path: "/next".into(),
            has_next_path: "/more".into(),
        };
        let saved = BTreeMap::from([(
            "Warehouse Lot Balance".to_owned(),
            Watermark {
                version: "100".into(),
                id: RuntimeValue::from("WLB-1"),
            },
        )]);
        let mut pull = Pull::new(cursor_uint, saved, Limits::default());
        let _req = pull.next_request().expect("req");
        let older = json!({
            "data": [{"name": "WLB-0", "modified": "99", "custom_norn_ref": "demo/h/0"}],
            "more": false
        });
        let page = pull
            .accept(200, older.to_string().as_bytes())
            .expect("accept");
        pull.confirm(&page.advance);
        assert_eq!(
            pull.watermarks()["Warehouse Lot Balance"].version,
            "100",
            "cursor watermark must not regress from 100 to 99"
        );

        // Keyset tied versions with backward id refuse; lexical regression refuses.
        let keyset_uint = holding_profile(FeedMode::Patch, VersionKind::Uint64);
        let mut tied = Pull::new(keyset_uint, BTreeMap::new(), Limits::default());
        let _req = tied.next_request().expect("req");
        let backward_id = json!({
            "data": [
                {"name": "WLB-B", "modified": "100", "custom_norn_ref": "demo/h/b"},
                {"name": "WLB-A", "modified": "100", "custom_norn_ref": "demo/h/a"}
            ]
        });
        assert!(matches!(
            tied.accept(200, backward_id.to_string().as_bytes()),
            Err(PullError::Bound { .. })
        ));
        let keyset_lex = holding_profile(FeedMode::Patch, VersionKind::Lexicographical);
        let mut lex = Pull::new(keyset_lex, BTreeMap::new(), Limits::default());
        let _req = lex.next_request().expect("req");
        let lex_back = json!({
            "data": [
                {"name": "WLB-1", "modified": "2026-11-12 10:00:00", "custom_norn_ref": "demo/h/1"},
                {"name": "WLB-2", "modified": "2026-11-12 09:00:00", "custom_norn_ref": "demo/h/2"}
            ]
        });
        assert!(matches!(
            lex.accept(200, lex_back.to_string().as_bytes()),
            Err(PullError::Bound { .. })
        ));
    }

    #[test]
    fn differing_feed_mode_on_same_source_event_raises_conflict() {
        let host = open_host(&erpnext_artifacts());
        seed_full_erpnext_snapshot(&host);
        let full_profile = holding_profile(FeedMode::FullSnapshot, VersionKind::Lexicographical);
        let patch_profile = holding_profile(FeedMode::Patch, VersionKind::Lexicographical);
        let row = json!({
            "name": "WLB-CONF-1", "modified": "2026-11-12 21:00:00.000000",
            "custom_norn_ref": "demo/holding/conflict-mode",
            "resource_code": "GOLD", "quantity": 10.0, "unit": "g", "location": "north",
            "item_ref": "demo/item/gold", "owner_ref": "demo/party/owner",
            "custodian_ref": "demo/party/custodian", "stock_cost": 100.0,
            "cost_currency": "USD", "valuation_method": "moving_average"
        });
        assert_eq!(
            ingest_patch_row(&host, &full_profile, &row, T0.saturating_add(1)),
            MappingOutcome::Landed
        );
        // Repeating the exact same event with explicit default `"feed_mode": "full_snapshot"`
        // matches canonically without raising a spurious conflict.
        let mut pull = Pull::new(full_profile.clone(), BTreeMap::new(), Limits::default());
        let _req = pull.next_request().expect("req");
        let rows = pull
            .accept(200, json!({ "data": [&row] }).to_string().as_bytes())
            .expect("accept")
            .rows;
        let mut explicit_default =
            observation(&full_profile, &rows[0], T0.saturating_add(1)).expect("obs");
        if let RuntimeValue::Object(ref mut map) = explicit_default.input {
            map.insert(
                "feed_mode".into(),
                RuntimeValue::String("full_snapshot".into()),
            );
        }
        let replay = host
            .ingest(&[(explicit_default, mapping_input("Warehouse Lot Balance"))])
            .expect("explicit default replay");
        assert_eq!(
            replay[0].outcome,
            MappingOutcome::Landed,
            "explicit default full_snapshot must not spuriously conflict with omitted default"
        );
        let second = ingest_patch_row(&host, &patch_profile, &row, T0.saturating_add(2));
        assert!(
            matches!(second, MappingOutcome::Quarantined(_)),
            "same event_id/payload with different feed_mode must conflict and quarantine: {second:?}"
        );

        // Equivalent typed version spellings (`"99"` vs `"099"` under Uint64)
        // share one canonical event_id so differing payloads trigger FEED-CONFLICT.
        let mut uint_profile = holding_profile(FeedMode::FullSnapshot, VersionKind::Uint64);
        uint_profile.feeds[0].version_path = "/seq".into();
        let row_99 = json!({
            "name": "WLB-CANON-1", "seq": "99",
            "modified": "2026-11-12 21:00:00.000000",
            "custom_norn_ref": "demo/holding/conflict-canon-ver",
            "resource_code": "GOLD", "quantity": 10.0, "unit": "g", "location": "north",
            "item_ref": "demo/item/gold", "owner_ref": "demo/party/owner",
            "custodian_ref": "demo/party/custodian", "stock_cost": 100.0,
            "cost_currency": "USD", "valuation_method": "moving_average"
        });
        assert_eq!(
            ingest_patch_row(&host, &uint_profile, &row_99, T0.saturating_add(3)),
            MappingOutcome::Landed
        );
        let row_099_diff = json!({
            "name": "WLB-CANON-1", "seq": "099",
            "modified": "2026-11-12 21:00:00.000000",
            "custom_norn_ref": "demo/holding/conflict-canon-ver",
            "resource_code": "GOLD", "quantity": 99.0, "unit": "g", "location": "north",
            "item_ref": "demo/item/gold", "owner_ref": "demo/party/owner",
            "custodian_ref": "demo/party/custodian", "stock_cost": 100.0,
            "cost_currency": "USD", "valuation_method": "moving_average"
        });
        let clash = ingest_patch_row(&host, &uint_profile, &row_099_diff, T0.saturating_add(4));
        assert!(
            matches!(clash, MappingOutcome::Quarantined(_)),
            "equivalent Uint64 spellings 99 vs 099 with differing payloads must conflict: {clash:?}"
        );
    }

    fn erpnext_with_sources_raw_json(connector_raw: &str) -> (Vec<String>, Vec<String>) {
        let mut set = erpnext_artifacts();
        for name in [
            "source.Synced.interface",
            "source.Connection.entity",
            "source.SyncRun.entity",
            "source.Upload.entity",
            "norn.connect.source",
            "norn.connect.mapping",
        ] {
            set.push(load_artifact(&format!(
                "registry/packs/sources/artifacts/{name}.json"
            )));
        }
        let mut base_json = Vec::new();
        let mut tenant_json = Vec::new();
        for artifact in set {
            if artifact.artifact_id == "erpnext.rest.connector" {
                tenant_json.push(connector_raw.to_owned());
            } else if artifact.artifact_id.starts_with("core.")
                || artifact.artifact_id.starts_with("std.")
            {
                base_json.push(serde_json::to_string(&artifact).expect("base json"));
            } else {
                tenant_json.push(serde_json::to_string(&artifact).expect("tenant json"));
            }
        }
        (base_json, tenant_json)
    }

    fn install_admitted(
        admitted: norn_api::kernel_set::AdmittedSet,
        scope: &CellScope,
    ) -> DevHost<NativeEngine> {
        let input = InstalledDefinitionInput {
            canonical_set: admitted.canonical_set,
            registry: vec![],
            receipt_sha256: admitted.receipt_sha256,
            admission_receipt: Some(admitted.receipt),
            readmission: None,
            diff: json!({}),
            installed_at: 1,
            layout: None,
        };
        let mut store = CellStore::open_in_memory(admitted.model.clone()).expect("store");
        store
            .install_definitions(admitted.model, input, Some(0), &scope.key())
            .expect("install");
        DevHost::from_store(store, scope.clone(), |_| BTreeMap::new())
            .expect("host")
            .with_authorizer(AllowAll)
    }

    fn assert_first_pull_url_contains(
        host: &DevHost<NativeEngine>,
        scope: &CellScope,
        needle: &str,
    ) {
        let config = r#"[{"id":"erpnext.rest","system":"erpnext","label":"ERPNext REST","connector":"erpnext.rest.connector","mapping":"erpnext.rest.mapping","instance":"sandbox","baseUrl":"http://erpnext-rest:8000","orgId":"org_demo","environmentId":"dev","secretRefs":["erpnext_rest_token"],"headers":{"X-Frappe-Site-Name":"frontend"}}]"#;
        let secrets = BTreeMap::from([(
            "NORN_SECRET_ERPNEXT_REST_TOKEN".to_owned(),
            "key:secret".to_owned(),
        )]);
        let mut sources =
            Sources::with_allowed_hosts(scope, config, &secrets, "erpnext-rest").expect("sources");
        sources
            .claim("erpnext.rest", Trigger::Manual, "user:owner@demo", T0)
            .expect("claim");
        let Step::Request(first_req) = sources.step(host, "erpnext.rest", None, T0) else {
            panic!("expected first pull HTTP request");
        };
        assert!(
            first_req.url.contains(needle),
            "first HTTP request must carry {needle}: {}",
            first_req.url
        );
    }

    #[test]
    fn installed_connector_preserves_exact_decimal_initial_id_in_first_request() {
        let base_connector = std::fs::read_to_string(
            root().join("fixtures/packs/erpnext-rest/artifacts/erpnext.rest.connector.json"),
        )
        .expect("connector json");
        let lossy_connector =
            base_connector.replacen(r#""id": """#, r#""id": 9007199254740992.1"#, 1);
        let conn_v1 =
            base_connector.replacen(r#""id": """#, r#""idDecimal": "9007199254740992""#, 1);
        let conn_v2 =
            base_connector.replacen(r#""id": """#, r#""idDecimal": "9007199254740992.1""#, 1);
        let scope = CellScope {
            org_id: "org_demo".into(),
            environment_id: "dev".into(),
        };
        let join_all = |b: &[String], t: &[String]| {
            format!(
                "[{}]",
                b.iter().chain(t).cloned().collect::<Vec<_>>().join(",")
            )
        };

        // 1. Bare lossy JSON numbers (including Serde-rounded 1000000000000000.1) refuse at admission.
        let serde_lossy = base_connector.replacen(r#""id": """#, r#""id": 1000000000000000.1"#, 1);
        let (base_json, lossy_tenant) = erpnext_with_sources_raw_json(&lossy_connector);
        let (_, serde_lossy_tenant) = erpnext_with_sources_raw_json(&serde_lossy);
        assert!(admit_kernel_set_json(&join_all(&base_json, &lossy_tenant)).is_err());
        assert!(admit_kernel_set_json(&join_all(&base_json, &serde_lossy_tenant)).is_err());
        let base_refs: Vec<&str> = base_json.iter().map(String::as_str).collect();
        let trusted_base = TrustedBase::from_sources(&base_refs).expect("trusted base");
        let lossy_sub = format!(
            r#"{{"baseSha256":"{}","artifacts":[{}]}}"#,
            trusted_base.sha256(),
            lossy_tenant.join(",")
        );
        assert!(admit_submission(&trusted_base, &scope, &lossy_sub).is_err());

        // 2. Two versions differing only in `idDecimal` get distinct receipts and plan diffs.
        let (_, tenant_v1) = erpnext_with_sources_raw_json(&conn_v1);
        let (_, tenant_v2) = erpnext_with_sources_raw_json(&conn_v2);
        let admitted_v1 = admit_kernel_set_json(&join_all(&base_json, &tenant_v1)).expect("v1");
        let sub_v2 = format!(
            r#"{{"baseSha256":"{}","artifacts":[{}]}}"#,
            trusted_base.sha256(),
            tenant_v2.join(",")
        );
        let admitted_v2 = admit_submission(&trusted_base, &scope, &sub_v2).expect("v2 provision");
        assert_ne!(
            admitted_v1.receipt_sha256, admitted_v2.receipt_sha256,
            "receipt_sha256 must distinguish 9007199254740992 from 9007199254740992.1"
        );
        assert_ne!(admitted_v1.canonical_set, admitted_v2.canonical_set);

        let host_v1 = install_admitted(admitted_v1.clone(), &scope);
        let plan_body = format!(r#"{{"artifacts":[{conn_v2}]}}"#);
        let plan = plan_definitions(
            &host_v1,
            &scope_query(),
            Some("admin"),
            &DefinitionKinds::default(),
            &plan_body,
        )
        .expect("plan v1 -> v2");
        assert!(!plan.unchanged, "v1 -> v2 must not report unchanged");
        assert_eq!(plan.changed.len(), 1);
        assert_eq!(plan.changed[0].artifact_id, "erpnext.rest.connector");
        assert!(
            plan.same.is_empty(),
            "v1 -> v2 must not report connector as unchanged: {:?}",
            plan.same
        );
        assert_ne!(plan.receipt_sha256, admitted_v1.receipt_sha256);
        let lossy_plan_body = format!(r#"{{"artifacts":[{lossy_connector}]}}"#);
        assert!(
            plan_definitions(
                &host_v1,
                &scope_query(),
                Some("admin"),
                &DefinitionKinds::default(),
                &lossy_plan_body,
            )
            .is_err(),
            "definitions:plan must also refuse bare lossy binary64 numbers"
        );

        // 3. Installed v2 -> admit_over -> Sources::step emits exact 9007199254740992.1 in first request.
        let host = install_admitted(admitted_v2, &scope);
        host.install_artifacts(&[], T0)
            .expect("admit_over re-install must preserve idDecimal");
        assert_first_pull_url_contains(&host, &scope, "9007199254740992.1");
    }
}
