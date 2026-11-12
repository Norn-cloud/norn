//! #588 / #343 (L-provenance): scanned documents through the real mapping path.
//!
//! The admitted `registry/packs/documents` pack (with the core
//! `norn.SourceEvidence` interface) is installed in a cell; the extracted rows
//! of `fixtures/documents/scans.snapshot.json` are captured as raw observations
//! (`inject_mapping_bundle`) and mapped (`process_mapping_bundle`), exactly as a
//! connector delivers them. Then:
//!
//! - CAUGHT AT THE SOURCE: invoice INV-0042 prints lines 30.00 + 40.00 + 2.00,
//!   but the seeded legacy-OCR import reads its total as 720. LAW A
//!   (`docs.ScannedDocument.total_matches_lines`) records a finding pinned to
//!   the law and quarantines the document; the raw read keeps the source truth,
//!   and the evidence of where the total was read survives.
//! - SLIPPED THROUGH: expense report RPT-0117 prints no lines, so no law
//!   applies, and its 72.0 read as 720 is admitted with field lineage.
//! - CORRECTED: the governed correction patches the total, records the reason,
//!   the evidence object and the verified actor (never caller input), and runs
//!   the laws on the corrected values: an inconsistent correction is refused
//!   and commits nothing; a consistent one resolves the finding and releases
//!   the quarantine, while the original version and its lineage stay readable.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "end-to-end scenario assertions"
)]

use std::collections::BTreeMap;

use crate::actions::{
    ActionAnswer, ActionQuery, ActionReceipt, call_action, decode_action_request,
};
use crate::authz::{
    AccessRequest, Authorizer, DenyAll, Operation, ReadCapability, ReadRequest,
    RefusalDetailRequest, read_denied,
};
use crate::host::DevHost;
use crate::question_routes::{QuestionRuntime, QuestionSnapshot};
use crate::refusal::Refusal;
use crate::scope::CellScope;
use norn_cell::engine::NativeEngine;
use norn_cell::mapping_bundle::MappingInput;
use norn_cell::reactive::{DecodeBudget, ReactiveLimits};
use norn_cell::{CellStore, MAIN_WORLD, ObjectRecord, SqliteCell};
use norn_checker::metric::compute::QuestionPlan;
use norn_ir::{DataQualityFindingStatus, DataQualityRuleRef, FactRef, RuntimeObject, RuntimeValue};
use norn_kernel::authz::{ActorKind, Delegation, PrincipalInput, ResourceInput, ScopeKey};
use norn_kernel::{
    ActionAuthzRequest, ActionDenial, ActionReadAuthzRequest, ActionReadTypeAuthzRequest,
    LogicalTime, ObservationEvent, ObservationId, Observed, Provenance,
};
use serde_json::Value;

const DOCUMENT: &str = "docs.ScannedDocument";
const EVIDENCE: &str = "docs.FieldEvidence";
const CORRECT: &str = "docs.scanned_document.correct";
const LAW: &str = "docs.ScannedDocument.total_matches_lines.law";
const STEWARD: &str = "user:steward";
const EXTRACTED_AT_MS: u64 = 1_793_620_800_000;

fn root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn artifact(path: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(root().join(path)).expect(path)).expect(path)
}

/// The core artifacts the pack needs (the evidence interface, and the
/// observed link claims mapped links record) and every artifact of the pack.
fn documents() -> Vec<Value> {
    let mut documents = vec![
        artifact("registry/core/artifacts/norn.SourceEvidence.interface.json"),
        artifact("registry/core/artifacts/norn.ObservedLinkClaim.entity.json"),
    ];
    let directory = root().join("registry/packs/documents/artifacts");
    let mut paths: Vec<_> = std::fs::read_dir(&directory)
        .expect("pack artifacts")
        .map(|entry| entry.expect("entry").path())
        .collect();
    paths.sort();
    // Interfaces and types before the artifacts that name them.
    paths.sort_by_key(|path| {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if name.ends_with(".interface.json") {
            0
        } else if name.ends_with(".entity.json") {
            1
        } else {
            2
        }
    });
    documents.extend(paths.iter().map(|path| {
        serde_json::from_str::<Value>(&std::fs::read_to_string(path).expect("artifact"))
            .expect("artifact JSON")
    }));
    documents
}

/// A provisioned `org_demo/dev` store whose installed, receipt-verified
/// definitions are `documents`, admitted by the checker, with the kernel's
/// own action types registered (as `tests/support/action_cell.rs` does for
/// the route tests; that file is already a module of this crate's tests).
fn admitted_store(documents: &[Value]) -> CellStore<NativeEngine> {
    let mut types = Vec::new();
    let mut properties = Vec::new();
    let mut interfaces = Vec::new();
    for document in documents {
        let payload = &document["payload"];
        if let Some(object_type) = payload.get("objectType") {
            types.push(serde_json::from_value(object_type.clone()).unwrap());
        }
        if let Some(interface) = payload.get("interface") {
            interfaces.push(serde_json::from_value(interface.clone()).unwrap());
        }
        for property_type in payload
            .get("propertyTypes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            properties.push(serde_json::from_value(property_type.clone()).unwrap());
        }
    }
    let artifacts: Vec<norn_ir::ArtifactEnvelope> =
        serde_json::from_value(Value::Array(documents.to_vec())).unwrap();
    let model =
        norn_cell::Model::from_definitions_with_interfaces(&types, &properties, &interfaces)
            .unwrap()
            .with_governing_laws(&artifacts)
            .unwrap();
    let model = crate::action_admission::prepare_action_model(&model).unwrap();
    let sources: Vec<RuntimeValue> = documents
        .iter()
        .cloned()
        .map(RuntimeValue::from_json)
        .collect::<Result<_, _>>()
        .unwrap();
    let checked = norn_checker::check_artifact_set_with_receipt(
        &norn_checker::ReceiptSources::new(&sources, &[]).unwrap(),
        &norn_checker::EntityArtifactSetContext {
            registry: &std::collections::BTreeSet::new(),
            base: &[],
            tenant: None,
        },
        &norn_ir::ResourceLimitProfile::norn_limits_v1(),
    )
    .unwrap();
    assert!(checked.receipt.admitted, "{:?}", checked.diagnostics);
    let input = norn_cell::InstalledDefinitionInput {
        canonical_set: serde_json::to_string(documents).unwrap(),
        registry: vec![],
        receipt_sha256: checked.receipt.content_sha256().unwrap(),
        admission_receipt: Some(
            String::from_utf8(checked.receipt.canonical_json().unwrap()).unwrap(),
        ),
        readmission: None,
        diff: serde_json::json!({}),
        installed_at: 1,
        layout: None,
    };
    CellStore::provision_definitions(
        NativeEngine::open_in_memory().unwrap(),
        model,
        &input,
        "org_demo/dev",
    )
    .unwrap()
}

/// The snapshot's rows, read exactly (decimals keep their scale).
fn snapshot_rows() -> Vec<(String, RuntimeValue)> {
    let text = std::fs::read_to_string(root().join("fixtures/documents/scans.snapshot.json"))
        .expect("snapshot");
    let document = RuntimeValue::from_json_text(text).expect("exact snapshot");
    let RuntimeValue::Array(rows) = document.get("snapshot").expect("rows").clone() else {
        panic!("snapshot rows are a list");
    };
    rows.into_iter()
        .map(|row| {
            let dataset = row
                .get("dataset")
                .and_then(RuntimeValue::as_str)
                .expect("dataset")
                .to_owned();
            (dataset, row.get("record").expect("record").clone())
        })
        .collect()
}

/// One extracted row as the sealed raw envelope a connector delivers.
fn raw(record: &RuntimeValue) -> Observed<RuntimeValue> {
    let id = record
        .get("name")
        .and_then(RuntimeValue::as_str)
        .expect("row identity")
        .to_owned();
    let digest = norn_ir::canonical::HashProfile::Norn1
        .hash_json(serde_json::to_string(record).expect("payload text"))
        .expect("payload digest");
    Observed {
        id: ObservationId(format!("synthetic-scans/{id}")),
        provenance: Provenance {
            source: "synthetic-scans".into(),
            source_id: id.clone(),
            mapping_version: "docs.scans.mapping".into(),
            observed_at: LogicalTime::from_millis(EXTRACTED_AT_MS),
        },
        input: norn_ir::runtime_object! {
            "contract" => "norn.raw_observation/v1",
            "source" => "synthetic-scans",
            "source_id" => id,
            "event_id" => "snapshot",
            "source_version" => "1",
            "pipeline_version" => "docs.scans.mapping",
            "kind" => "upsert",
            "extracted_at" => EXTRACTED_AT_MS,
            "payload" => record.clone(),
            "payload_digest" => digest,
        },
    }
}

/// Install the pack, then capture and map every extracted row.
fn loaded() -> CellStore<NativeEngine> {
    let mut store = admitted_store(&documents());
    let mut captured = Vec::new();
    for (dataset, record) in snapshot_rows() {
        let input = MappingInput {
            mapping: "docs.scans.mapping".into(),
            dataset,
            connector_instance: "demo".into(),
            partition: "snapshot".into(),
            recorded_at: norn_ir::provenance::UtcTimestamp("2026-11-12T12:00:00Z".into()),
        };
        let (id, new) = store
            .inject_mapping_bundle(&raw(&record), &input)
            .expect("captured");
        assert!(new, "each row is captured once");
        captured.push(id);
    }
    let mut cell: SqliteCell<ObservationEvent, RuntimeObject, RuntimeObject, NativeEngine> =
        SqliteCell::new(store, "org_demo/dev", |_| BTreeMap::new()).expect("cell");
    for id in &captured {
        assert!(cell.process_mapping_bundle(id).expect("mapped"), "{id}");
    }
    cell.into_store()
}

/// Server policy keyed on the verified subject only: the steward may call and read.
struct Policy;
impl Authorizer for Policy {
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Refusal> {
        match (&request.operation, request.actor) {
            (Operation::CallAction(_), Some(STEWARD)) => Ok(()),
            _ => DenyAll.authorize(request),
        }
    }
    fn read(&self, request: &ReadRequest<'_>) -> Result<(), Refusal> {
        if request.access.principal.subject_id == STEWARD {
            Ok(())
        } else {
            Err(read_denied())
        }
    }
    fn action(&self, request: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        if request.actor == STEWARD {
            Ok(())
        } else {
            Err(ActionDenial::Invisible)
        }
    }
    fn action_read(&self, request: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        if request.actor == STEWARD {
            Ok(())
        } else {
            Err(ActionDenial::Invisible)
        }
    }
    fn action_read_type(
        &self,
        request: &ActionReadTypeAuthzRequest<'_>,
    ) -> Result<(), ActionDenial> {
        if request.actor == STEWARD {
            Ok(())
        } else {
            Err(ActionDenial::Invisible)
        }
    }
    fn disclose_refusal_detail(&self, request: &RefusalDetailRequest<'_>) -> bool {
        request.actor == STEWARD
    }
}

/// The cell's disclosure runtime: a handle key and collection-grant mapping only.
struct Disclosure;
impl QuestionRuntime<NativeEngine> for Disclosure {
    fn plan(&self, _question: &str) -> Result<QuestionPlan, Refusal> {
        Err(read_denied())
    }
    fn snapshot(
        &self,
        _store: &CellStore<NativeEngine>,
        _plan: &QuestionPlan,
        _actor: &str,
    ) -> Result<QuestionSnapshot, Refusal> {
        Err(read_denied())
    }
    fn access(
        &self,
        _scope: &CellScope,
        _actor: &str,
        _question: &str,
        _capability: ReadCapability,
        _row: Option<&ObjectRecord>,
    ) -> Result<norn_kernel::authz::AccessRequest, Refusal> {
        Err(read_denied())
    }
    fn min_disclosed_bucket(&self) -> usize {
        3
    }
    fn handle_key(&self) -> &[u8] {
        &[7; 32]
    }
    fn collection_access(
        &self,
        scope: &CellScope,
        actor: &str,
        _capability: ReadCapability,
        object_type: &str,
    ) -> Result<norn_kernel::authz::AccessRequest, Refusal> {
        let key = ScopeKey {
            org: scope.org_id.clone(),
            environment: Some(scope.environment_id.clone()),
            tenant: None,
            workspace: None,
            business_unit: None,
        };
        Ok(norn_kernel::authz::AccessRequest {
            principal: PrincipalInput {
                subject_id: actor.into(),
                actor_kind: ActorKind::User,
                agent_session_id: None,
                roles: vec![],
                attributes: BTreeMap::new(),
            },
            scope: Some(key.clone()),
            capability: "norn.read.values".into(),
            action_id: "norn.read".into(),
            resource: ResourceInput {
                object_type: object_type.into(),
                id: object_type.into(),
                scope: key,
                attributes: BTreeMap::new(),
            },
            context: BTreeMap::new(),
            binding: None,
            delegation: Delegation::Direct,
        })
    }
}

/// The host's read mapping: the verified caller and the exact row or collection.
struct Access;
impl crate::object_routes::ObjectAccess for Access {
    fn access(
        &self,
        scope: &CellScope,
        actor: &str,
        capability: ReadCapability,
        object_type: &str,
        row: Option<&ObjectRecord>,
    ) -> Result<norn_kernel::authz::AccessRequest, Refusal> {
        let mut request = Disclosure.collection_access(scope, actor, capability, object_type)?;
        if let Some(row) = row {
            request.resource.object_type.clone_from(&row.object_type);
            request.resource.id.clone_from(&row.id);
        }
        Ok(request)
    }
}

fn scope_query() -> crate::scope::ScopeQuery {
    crate::scope::ScopeQuery {
        org_id: Some("org_demo".into()),
        environment_id: Some("dev".into()),
    }
}

fn host(store: CellStore<NativeEngine>) -> DevHost<NativeEngine> {
    let scope = CellScope {
        org_id: "org_demo".into(),
        environment_id: "dev".into(),
    };
    DevHost::from_store(store, scope, |command| command.payload.clone())
        .expect("host")
        .with_authorizer(Policy)
}

fn correct(
    host: &DevHost<NativeEngine>,
    actor: &str,
    body: &str,
) -> Result<ActionReceipt, Refusal> {
    let query = ActionQuery {
        org_id: Some("org_demo".into()),
        environment_id: Some("dev".into()),
        version: None,
    };
    let request = decode_action_request(body)?;
    match call_action(
        host,
        &query,
        Some(actor),
        CORRECT,
        &request,
        &Disclosure,
        LogicalTime::from_millis(1_800_000_000_000),
    )? {
        ActionAnswer::Receipt(receipt) => Ok(receipt),
        ActionAnswer::Pending(_) => Err(Refusal::host("no approval gates the correction")),
    }
}

/// A stored value as text. Decimals are stored by value (`72.00` is `72`);
/// the property's declared scale formats them for display.
fn text(record: &ObjectRecord, property: &str) -> String {
    match record.values.get(property) {
        Some(RuntimeValue::String(text)) => text.clone(),
        Some(RuntimeValue::Number(number)) => number.to_string(),
        Some(RuntimeValue::Null) | None => "null".into(),
        Some(other) => format!("{other:?}"),
    }
}

#[test]
fn a_dropped_decimal_is_caught_at_the_source_with_its_evidence() {
    let store = loaded();
    let reader = store.read();

    let clean = reader
        .authoritative_object(MAIN_WORLD, DOCUMENT, "SCAN-INV-0041")
        .expect("read")
        .expect("a consistent invoice is admitted");
    assert_eq!(text(&clean, "total"), "30.5");
    assert_eq!(text(&clean, "lines_total"), "30.5");

    let caught = reader
        .object(MAIN_WORLD, DOCUMENT, "SCAN-INV-0042")
        .expect("read")
        .expect("raw reads keep the source truth");
    assert_eq!(text(&caught, "total"), "720");
    assert_eq!(text(&caught, "lines_total"), "72");
    assert!(
        reader
            .observation_is_quarantined(MAIN_WORLD, &caught)
            .expect("quality")
    );
    assert!(
        reader
            .authoritative_object(MAIN_WORLD, DOCUMENT, "SCAN-INV-0042")
            .expect("read")
            .is_none(),
        "a quarantined document stays out of authoritative reads"
    );
    let findings = reader
        .observation_findings(MAIN_WORLD, DOCUMENT, "SCAN-INV-0042")
        .expect("findings");
    assert_eq!(findings.len(), 1, "{findings:#?}");
    let finding = &findings[0];
    assert_eq!(finding.status, DataQualityFindingStatus::Open);
    match &finding.rule_ref {
        DataQualityRuleRef::Law { artifact_id, .. } => assert_eq!(artifact_id, LAW),
        other @ (DataQualityRuleRef::Invariant { .. } | DataQualityRuleRef::Builtin { .. }) => {
            panic!("the finding is pinned to LAW A, not {other:?}")
        }
    }

    // Where the total was read survives the quarantine of the document.
    let evidence = reader
        .object(MAIN_WORLD, EVIDENCE, "SCAN-INV-0042~total")
        .expect("read")
        .expect("total evidence");
    assert_eq!(text(&evidence, "subject_property"), "total");
    assert_eq!(text(&evidence, "extractor"), "legacy-ocr");
    assert!(text(&evidence, "document_uri").starts_with("s3://"));

    // The lines are mapped objects of their own.
    for (line, amount) in [("1", "30"), ("2", "40"), ("3", "2")] {
        let row = reader
            .object(
                MAIN_WORLD,
                "docs.DocumentLine",
                &format!("SCAN-INV-0042~line-{line}"),
            )
            .expect("read")
            .expect("line");
        assert_eq!(text(&row, "amount"), amount);
    }
}

#[test]
fn a_slipped_value_is_admitted_with_field_lineage_and_corrected_by_a_governed_action() {
    let host = host(loaded());
    let slipped = {
        let cell = host.cell().expect("cell");
        let reader = cell.store().read();
        let record = reader
            .authoritative_object(MAIN_WORLD, DOCUMENT, "SCAN-RPT-0117")
            .expect("read")
            .expect("no law applies to a document without lines");
        assert_eq!(text(&record, "total"), "720");
        assert_eq!(text(&record, "lines_total"), "null");
        let mut budget = DecodeBudget::new(&ReactiveLimits::from_profile(
            &norn_ir::ResourceLimitProfile::norn_limits_v1(),
        ));
        let lineage = reader
            .instance_source_provenance(
                MAIN_WORLD,
                &FactRef::new(DOCUMENT, "SCAN-RPT-0117", record.version),
                &mut budget,
                |_| Ok(true),
            )
            .expect("lineage")
            .expect("mapped lineage");
        let total = lineage
            .field_provenance
            .iter()
            .find(|(property, _)| property.as_str() == "total")
            .map(|(_, provenance)| provenance)
            .expect("total lineage");
        assert!(total.source_fields.iter().any(|field| field == "total"));
        assert_eq!(lineage.mapping_pin.artifact_id, "docs.scans.mapping");
        record.version
    };

    // Who corrects is the verified actor; the caller cannot supply it.
    let forged = correct(
        &host,
        STEWARD,
        r#"{"idempotencyKey":"forged","input":{"id":"SCAN-RPT-0117","currency":"USD","total":"72.00","correction_reason":"r","correction_evidence":"SCAN-RPT-0117~total","actor":"user:someone-else"}}"#,
    );
    assert!(forged.is_err(), "a caller-supplied actor is refused");
    // Nor can it offer the actor-derived field itself (ACTION-ACTOR-1)...
    let offered = correct(
        &host,
        STEWARD,
        r#"{"idempotencyKey":"offered","input":{"id":"SCAN-RPT-0117","currency":"USD","total":"72.00","correction_reason":"r","correction_evidence":"SCAN-RPT-0117~total","corrected_by":"user:someone-else"}}"#,
    )
    .expect_err("a caller-supplied corrected_by is refused");
    assert!((400..500).contains(&offered.status), "{offered:?}");
    // ...and the form the catalog publishes never shows it.
    let catalog = crate::action_catalog::list_actions(
        &host,
        &crate::action_catalog::ActionCatalogQuery {
            org_id: Some("org_demo".into()),
            environment_id: Some("dev".into()),
            object_kind: None,
        },
        Some(STEWARD),
    )
    .expect("catalog");
    let form = catalog
        .items
        .iter()
        .find(|item| item.action_id == CORRECT)
        .expect("the steward may correct");
    let fields = form.input_schema["properties"]
        .as_object()
        .expect("properties");
    assert!(!fields.contains_key("corrected_by"), "{fields:?}");
    assert!(fields.contains_key("correction_evidence"));
    // The cited evidence must exist: it is read in the same commit.
    let uncited = correct(
        &host,
        STEWARD,
        r#"{"idempotencyKey":"uncited","input":{"id":"SCAN-RPT-0117","currency":"USD","total":"72.00","correction_reason":"r","correction_evidence":"SCAN-RPT-0117~nowhere"}}"#,
    );
    assert!(
        uncited.is_err(),
        "a correction citing no evidence object is refused"
    );

    // A reason is required.
    let unexplained = correct(
        &host,
        STEWARD,
        r#"{"idempotencyKey":"no-reason","input":{"id":"SCAN-RPT-0117","currency":"USD","total":"72.00","correction_reason":"","correction_evidence":"SCAN-RPT-0117~total"}}"#,
    );
    assert!(
        unexplained.is_err(),
        "a correction without a reason is refused"
    );

    correct(
        &host,
        STEWARD,
        r#"{"idempotencyKey":"rpt-0117","input":{"id":"SCAN-RPT-0117","currency":"USD","total":"72.00","correction_reason":"The scan reads 72.0; the import dropped the decimal point.","correction_evidence":"SCAN-RPT-0117~total"}}"#,
    )
    .expect("a consistent, explained correction commits");

    corrected_with_history(&host, slipped);
}

/// After the RPT-0117 correction: the corrected values, the original version
/// kept as history, and the history read naming what wrote each version.
fn corrected_with_history(host: &DevHost<NativeEngine>, slipped: u64) {
    use crate::object_review::NornVersionWriter::{Action, Mapping, Other};
    {
        let cell = host.cell().expect("cell");
        let reader = cell.store().read();
        let corrected = reader
            .authoritative_object(MAIN_WORLD, DOCUMENT, "SCAN-RPT-0117")
            .expect("read")
            .expect("corrected");
        assert_eq!(text(&corrected, "total"), "72");
        assert_eq!(text(&corrected, "corrected_by"), STEWARD);
        assert_eq!(
            text(&corrected, "correction_evidence"),
            "SCAN-RPT-0117~total"
        );
        assert_eq!(
            text(&corrected, "extractor"),
            "legacy-ocr",
            "a patch keeps the other values"
        );
        assert!(corrected.version > slipped);
        // The original version is history, not overwritten.
        let original = reader
            .object_at_version(MAIN_WORLD, DOCUMENT, "SCAN-RPT-0117", slipped)
            .expect("read")
            .expect("original version");
        assert_eq!(text(&original, "total"), "720");
    }
    let history = crate::object_review::get_object_history(
        host,
        &scope_query(),
        Some(STEWARD),
        DOCUMENT,
        "SCAN-RPT-0117",
        &Access,
    )
    .expect("history");
    assert_eq!(history.versions.len(), 2, "{history:#?}");
    let (newest, oldest) = (&history.versions[0], &history.versions[1]);
    match &newest.writer {
        Action {
            action_id,
            actor,
            input,
        } => {
            assert_eq!(action_id, CORRECT);
            assert_eq!(actor, STEWARD);
            assert_eq!(
                input
                    .get("correction_reason")
                    .and_then(RuntimeValue::as_str),
                Some("The scan reads 72.0; the import dropped the decimal point.")
            );
        }
        other @ (Mapping { .. } | Other) => panic!("the correction is the action's: {other:?}"),
    }
    match &oldest.writer {
        Mapping { lineage } => assert_eq!(lineage.mapping_pin.artifact_id, "docs.scans.mapping"),
        other @ (Action { .. } | Other) => panic!("the import is the mapping's: {other:?}"),
    }
    let value = |version: &crate::object_review::NornObjectVersion| match version
        .values
        .as_ref()
        .and_then(|values| values.get("total"))
    {
        Some(RuntimeValue::Number(number)) => number.to_string(),
        other => format!("{other:?}"),
    };
    assert_eq!(
        (value(newest).as_str(), value(oldest).as_str()),
        ("72", "720")
    );
    crate::object_review::get_object_lineage_at(
        host,
        &scope_query(),
        Some(STEWARD),
        (DOCUMENT, "SCAN-RPT-0117", slipped),
        &Access,
    )
    .expect("the imported version keeps its lineage");
}

#[test]
fn a_correction_must_satisfy_the_law_and_then_releases_the_quarantine() {
    let host = host(loaded());
    let findings = crate::object_review::get_object_findings(
        &host,
        &scope_query(),
        Some(STEWARD),
        DOCUMENT,
        "SCAN-INV-0042",
        &Access,
    )
    .expect("a reviewer reads the findings of a quarantined document");
    assert!(findings.quarantined);
    assert_eq!(findings.findings.len(), 1);
    assert!(
        findings.evidence.iter().any(
            |evidence| evidence.object_type == EVIDENCE && evidence.id == "SCAN-INV-0042~total"
        ),
        "the evidence of the quarantined total is listed: {:?}",
        findings.evidence
    );
    assert!(
        crate::object_review::get_object_findings(
            &host,
            &scope_query(),
            Some("user:stranger"),
            DOCUMENT,
            "SCAN-INV-0042",
            &Access,
        )
        .is_err(),
        "a caller without read authority learns nothing"
    );
    let quarantined = crate::object_review::get_quarantined_objects(
        &host,
        &scope_query(),
        Some(STEWARD),
        DOCUMENT,
        &Access,
    )
    .expect("the steward lists what is held back");
    assert_eq!(
        quarantined
            .items
            .iter()
            .map(|item| (item.id.as_str(), item.open_findings))
            .collect::<Vec<_>>(),
        [("SCAN-INV-0042", 1)]
    );
    let before = host
        .cell()
        .expect("cell")
        .store()
        .read()
        .dump()
        .expect("dump");
    let wrong = correct(
        &host,
        STEWARD,
        r#"{"idempotencyKey":"inv-0042-wrong","input":{"id":"SCAN-INV-0042","currency":"USD","total":"70.00","correction_reason":"typo","correction_evidence":"SCAN-INV-0042~total"}}"#,
    );
    let refusal = wrong.expect_err("70.00 is not 30.00 + 40.00 + 2.00");
    assert_eq!(refusal.status, 422, "{refusal:?}");
    assert_eq!(
        host.cell()
            .expect("cell")
            .store()
            .read()
            .dump()
            .expect("dump"),
        before,
        "a refused correction commits nothing"
    );

    correct(
        &host,
        STEWARD,
        r#"{"idempotencyKey":"inv-0042","input":{"id":"SCAN-INV-0042","currency":"USD","total":"72.00","correction_reason":"The scan reads 72.00; the legacy import dropped the decimal point.","correction_evidence":"SCAN-INV-0042~total"}}"#,
    )
    .expect("the consistent correction commits");
    let cell = host.cell().expect("cell");
    let reader = cell.store().read();
    let released = reader
        .authoritative_object(MAIN_WORLD, DOCUMENT, "SCAN-INV-0042")
        .expect("read")
        .expect("released from quarantine");
    assert_eq!(text(&released, "total"), "72");
    let findings = reader
        .observation_findings(MAIN_WORLD, DOCUMENT, "SCAN-INV-0042")
        .expect("findings");
    assert_eq!(findings.len(), 1, "the original finding is retained");
    assert_eq!(findings[0].status, DataQualityFindingStatus::Resolved);
}
