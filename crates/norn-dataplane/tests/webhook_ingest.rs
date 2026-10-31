#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    missing_docs,
    reason = "integration-test crate"
)]
#![deny(
    clippy::match_wildcard_for_single_variants,
    clippy::format_collect,
    clippy::format_push_string,
    clippy::inefficient_to_string,
    clippy::large_types_passed_by_value,
    clippy::needless_pass_by_value,
    clippy::large_futures
)]

//! T12-DP-4: the Frappe and Shopify webhook fast path and the Frappe
//! completeness sweep (`cargo test -p norn-dataplane webhook_ingest`).

use std::collections::BTreeMap;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hmac::{Hmac, Mac};
use norn_dataplane::frappe::FRAPPE_WEBHOOK_PIPELINE;
use norn_dataplane::shopify::SHOPIFY_WEBHOOK_PIPELINE;
use norn_dataplane::sweep::FRAPPE_SWEEP_PIPELINE;
use norn_dataplane::webhook::{normalize_frappe_datetime, normalize_instant};
use norn_dataplane::{
    Applied, Convergence, FrappeSource, FrappeSweep, IngestError, ObservationError,
    ObservationKind, RawObservation, RedactionPolicy, ShopifyHeaders, ShopifySource, SweepCursor,
    SweepFeed, ingest_frappe_webhook, ingest_shopify_webhook,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::Sha256;

const FRAPPE_SECRET: &[u8] = b"frappe-webhook-secret";
const SHOPIFY_SECRET: &[u8] = b"shopify-client-secret";

fn sign(secret: &[u8], body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
    mac.update(body);
    STANDARD.encode(mac.finalize().into_bytes())
}

fn erpnext() -> FrappeSource {
    FrappeSource {
        source: "erpnext".to_owned(),
        redaction: None,
    }
}

#[expect(
    clippy::unwrap_in_result,
    reason = "This test fixture must panic on failed prerequisites rather than simulate a recoverable domain refusal"
)]
fn frappe(body: &Value, extracted_at: u64) -> Result<RawObservation, IngestError> {
    let bytes = serde_json::to_vec(body).unwrap();
    let signature = sign(FRAPPE_SECRET, &bytes);
    ingest_frappe_webhook(
        &erpnext(),
        FRAPPE_SECRET,
        &bytes,
        Some(signature.as_str()),
        extracted_at,
    )
}

#[test]
fn webhook_ingest_frappe_signature_is_checked_before_the_body() {
    let body = b"not json at all";
    let source = erpnext();
    let good = sign(FRAPPE_SECRET, body);
    // The signature is checked first: a well-signed non-JSON body fails on
    // JSON, a badly signed one fails on the signature.
    assert!(matches!(
        ingest_frappe_webhook(&source, FRAPPE_SECRET, body, Some(good.as_str()), 1),
        Err(IngestError::Observation(ObservationError::Json(_)))
    ));
    let other = sign(b"another secret", body);
    assert!(matches!(
        ingest_frappe_webhook(&source, FRAPPE_SECRET, body, Some(other.as_str()), 1),
        Err(IngestError::BadSignature)
    ));
    assert!(matches!(
        ingest_frappe_webhook(&source, FRAPPE_SECRET, body, None, 1),
        Err(IngestError::MissingSignature { .. })
    ));
    assert!(matches!(
        ingest_frappe_webhook(&source, FRAPPE_SECRET, body, Some("  "), 1),
        Err(IngestError::MissingSignature { .. })
    ));
    assert!(matches!(
        ingest_frappe_webhook(&source, FRAPPE_SECRET, body, Some("%%%"), 1),
        Err(IngestError::MalformedSignature { .. })
    ));
    assert!(matches!(
        ingest_frappe_webhook(&source, b"", body, Some(good.as_str()), 1),
        Err(IngestError::MissingSecret)
    ));
}

#[test]
fn webhook_ingest_frappe_maps_events_to_observations() {
    let doc = json!({
        "doctype": "Sales Order",
        "name": "SO-0001",
        "modified": "2026-10-01 09:15:02.481233",
        "grand_total": 1250.5
    });
    let update = frappe(&json!({"event": "on_update", "doc": doc}), 10).unwrap();
    assert_eq!(update.kind, ObservationKind::Upsert);
    assert_eq!(update.source, "erpnext");
    assert_eq!(update.source_id, "Sales Order/SO-0001");
    assert_eq!(
        update.event_id,
        "Sales Order/SO-0001@2026-10-01 09:15:02.481233"
    );
    assert_eq!(update.source_version, "2026-10-01 09:15:02.481233");
    assert_eq!(update.pipeline_version, FRAPPE_WEBHOOK_PIPELINE);
    assert_eq!(update.extracted_at, 10);
    assert_eq!(update.payload, doc, "payload is the document as received");
    update.validate().unwrap();

    let trash = frappe(&json!({"event": "on_trash", "doc": doc}), 11).unwrap();
    assert_eq!(trash.kind, ObservationKind::Delete);
    assert_eq!(
        trash.event_id,
        "Sales Order/SO-0001#deleted@2026-10-01 09:15:02.481233"
    );

    let bare = frappe(&doc, 12).unwrap();
    assert_eq!(bare.kind, ObservationKind::Upsert);
    assert_eq!(bare.event_id, update.event_id, "a bare doc is on_update");

    assert!(matches!(
        frappe(&json!({"event": "before_save", "doc": doc}), 1),
        Err(IngestError::UnsupportedEvent { .. })
    ));
    assert!(matches!(
        frappe(&json!({"event": "on_update", "doc": "SO-0001"}), 1),
        Err(IngestError::MissingField { field: "doc", .. })
    ));
    assert!(matches!(
        frappe(&json!({"doctype": "Sales Order", "name": "SO-0001"}), 1),
        Err(IngestError::MissingField {
            field: "modified",
            ..
        })
    ));
}

#[test]
fn webhook_ingest_frappe_redaction_runs_before_load() {
    let source = FrappeSource {
        source: "erpnext".to_owned(),
        redaction: Some(RedactionPolicy {
            id: "pii.customer".to_owned(),
            version: "1".to_owned(),
            pointers: vec!["/mobile_no".to_owned()],
        }),
    };
    let body = serde_json::to_vec(&json!({
        "doctype": "Customer",
        "name": "CUST-1",
        "modified": "2026-10-01 08:00:00",
        "mobile_no": "+20 100 000 0000"
    }))
    .unwrap();
    let signature = sign(FRAPPE_SECRET, &body);
    let observation =
        ingest_frappe_webhook(&source, FRAPPE_SECRET, &body, Some(signature.as_str()), 5).unwrap();
    assert_eq!(observation.payload["mobile_no"], json!("[redacted]"));
    assert_eq!(
        observation.redaction.unwrap().redacted,
        vec!["/mobile_no".to_owned()]
    );
}

#[test]
fn webhook_ingest_timestamps_normalize() {
    assert_eq!(
        normalize_frappe_datetime("2026-10-01 09:05:00").unwrap(),
        "2026-10-01 09:05:00.000000"
    );
    assert_eq!(
        normalize_frappe_datetime("2026-10-01T09:06:30.25").unwrap(),
        "2026-10-01 09:06:30.250000"
    );
    assert!(normalize_frappe_datetime("yesterday").is_err());
    // The same instant in two offsets normalizes to one version.
    assert_eq!(
        normalize_instant("2026-10-01T05:15:02-04:00").unwrap(),
        normalize_instant("2026-10-01T09:15:02Z").unwrap()
    );
    assert!(
        normalize_instant("2026-10-01T09:15:02Z").unwrap()
            < normalize_instant("2026-10-01T09:15:03+00:00").unwrap()
    );
    assert!(normalize_instant("1969-12-31T23:59:59Z").is_err());
}

fn shop() -> ShopifySource {
    ShopifySource {
        source: "shopify".to_owned(),
        shop_domain: "acme.myshopify.com".to_owned(),
        redaction: None,
    }
}

fn shopify_headers(topic: &str, body: &[u8]) -> ShopifyHeaders {
    let mut headers = BTreeMap::new();
    headers.insert("x-shopify-hmac-sha256", sign(SHOPIFY_SECRET, body));
    headers.insert("x-shopify-topic", topic.to_owned());
    headers.insert("x-shopify-shop-domain", "acme.myshopify.com".to_owned());
    headers.insert(
        "x-shopify-event-id",
        "98880550-7158-44d4-b7cd-2c97c8a091b5".to_owned(),
    );
    headers.insert(
        "x-shopify-webhook-id",
        "b54557e4-bdd9-4b37-8a5f-bf7d70bcd043".to_owned(),
    );
    headers.insert("x-shopify-triggered-at", "2026-10-01T09:20:00Z".to_owned());
    // The host's lookup is case-insensitive.
    ShopifyHeaders::from_lookup(|name| headers.get(name.to_ascii_lowercase().as_str()).cloned())
}

#[test]
fn webhook_ingest_shopify_upsert_and_delete() {
    let body = serde_json::to_vec(&json!({
        "id": 450_789_469_u64,
        "updated_at": "2026-10-01T05:15:02-04:00",
        "total_price": "199.65"
    }))
    .unwrap();
    let headers = shopify_headers("orders/updated", &body);
    let observation = ingest_shopify_webhook(&shop(), SHOPIFY_SECRET, &headers, &body, 7).unwrap();
    assert_eq!(observation.source, "shopify");
    assert_eq!(observation.source_id, "orders/450789469");
    assert_eq!(observation.event_id, "98880550-7158-44d4-b7cd-2c97c8a091b5");
    assert_eq!(observation.kind, ObservationKind::Upsert);
    assert_eq!(
        observation.source_version,
        normalize_instant("2026-10-01T09:15:02Z").unwrap()
    );
    assert_eq!(observation.pipeline_version, SHOPIFY_WEBHOOK_PIPELINE);
    observation.validate().unwrap();

    // Redelivery: same ids, later receipt time, same observation id.
    let again = ingest_shopify_webhook(&shop(), SHOPIFY_SECRET, &headers, &body, 9).unwrap();
    assert_eq!(again.observation_id(), observation.observation_id());
    let mut view = Convergence::new();
    assert_eq!(view.apply(&observation), Applied::Advanced);
    assert_eq!(view.apply(&again), Applied::Duplicate);

    let delete_body = serde_json::to_vec(&json!({"id": 450_789_469_u64})).unwrap();
    let mut delete_headers = shopify_headers("orders/delete", &delete_body);
    delete_headers.event_id = None;
    let delete =
        ingest_shopify_webhook(&shop(), SHOPIFY_SECRET, &delete_headers, &delete_body, 8).unwrap();
    assert_eq!(delete.kind, ObservationKind::Delete);
    assert_eq!(
        delete.event_id, "b54557e4-bdd9-4b37-8a5f-bf7d70bcd043",
        "falls back to the webhook id"
    );
    assert_eq!(
        delete.source_version,
        normalize_instant("2026-10-01T09:20:00Z").unwrap()
    );
    assert_eq!(view.apply(&delete), Applied::Advanced);
    assert_eq!(
        view.current("shopify", "orders/450789469").unwrap().kind,
        ObservationKind::Delete
    );
}

#[test]
fn webhook_ingest_shopify_refusals() {
    let body = serde_json::to_vec(&json!({"id": 1, "updated_at": "2026-10-01T09:15:02Z"})).unwrap();
    let headers = shopify_headers("orders/updated", &body);

    let tampered =
        serde_json::to_vec(&json!({"id": 2, "updated_at": "2026-10-01T09:15:02Z"})).unwrap();
    assert!(matches!(
        ingest_shopify_webhook(&shop(), SHOPIFY_SECRET, &headers, &tampered, 1),
        Err(IngestError::BadSignature)
    ));

    let mut other_shop = shop();
    other_shop.shop_domain = "other.myshopify.com".to_owned();
    assert!(matches!(
        ingest_shopify_webhook(&other_shop, SHOPIFY_SECRET, &headers, &body, 1),
        Err(IngestError::ShopMismatch { .. })
    ));

    let mut anonymous = headers;
    anonymous.event_id = None;
    anonymous.webhook_id = None;
    assert!(matches!(
        ingest_shopify_webhook(&shop(), SHOPIFY_SECRET, &anonymous, &body, 1),
        Err(IngestError::MissingField { .. })
    ));

    let no_id = serde_json::to_vec(&json!({"updated_at": "2026-10-01T09:15:02Z"})).unwrap();
    let no_id_headers = shopify_headers("orders/updated", &no_id);
    assert!(matches!(
        ingest_shopify_webhook(&shop(), SHOPIFY_SECRET, &no_id_headers, &no_id, 1),
        Err(IngestError::MissingField { field: "id", .. })
    ));
}

fn sales_order_sweep() -> FrappeSweep {
    FrappeSweep {
        source: erpnext(),
        doctype: "Sales Order".to_owned(),
        page_size: 2,
        lookback_secs: 300,
    }
}

fn query(command: &norn_dataplane::SweepCommand) -> BTreeMap<String, String> {
    command.query.iter().cloned().collect()
}

#[test]
fn webhook_ingest_sweep_plans_keyset_pages() {
    let sweep = sales_order_sweep();
    let first = sweep.plan(SweepFeed::Documents, None).unwrap();
    assert_eq!(first.method, "GET");
    assert_eq!(first.path_segments, ["api", "resource", "Sales Order"]);
    let q = query(&first);
    assert_eq!(q["fields"], r#"["name","modified"]"#);
    assert_eq!(q["order_by"], "modified asc, name asc");
    assert_eq!(q["limit_page_length"], "2");
    assert!(!q.contains_key("filters"));

    let after = SweepCursor {
        feed: SweepFeed::Documents,
        doctype: "Sales Order".to_owned(),
        modified: "2026-10-01 09:06:30.250000".to_owned(),
        name: "SO-0003".to_owned(),
    };
    let next = sweep.plan(SweepFeed::Documents, Some(&after)).unwrap();
    let q = query(&next);
    assert_eq!(
        q["filters"],
        r#"[["modified",">=","2026-10-01 09:06:30.250000"]]"#
    );
    assert_eq!(
        q["or_filters"],
        r#"[["modified",">","2026-10-01 09:06:30.250000"],["name",">","SO-0003"]]"#
    );
    assert_eq!(next.after.as_ref(), Some(&after));

    let deletions = sweep.plan(SweepFeed::Deletions, None).unwrap();
    assert_eq!(
        deletions.path_segments,
        ["api", "resource", "Deleted Document"]
    );
    assert_eq!(
        query(&deletions)["filters"],
        r#"[["deleted_doctype","=","Sales Order"]]"#
    );

    // A cursor of the other feed is refused, as is an empty page size.
    assert!(matches!(
        sweep.plan(SweepFeed::Deletions, Some(&after)),
        Err(IngestError::InvalidSweep { .. })
    ));
    let mut empty = sales_order_sweep();
    empty.page_size = 0;
    assert!(matches!(
        empty.plan(SweepFeed::Documents, None),
        Err(IngestError::InvalidSweep { .. })
    ));

    // A new cycle rewinds by the lookback, before every name.
    let rewound = sweep.rewind(&after).unwrap();
    assert_eq!(rewound.modified, "2026-10-01 09:01:30.250000");
    assert_eq!(rewound.name, "");
    let cycle = sweep
        .start_cycle(SweepFeed::Documents, Some(&after))
        .unwrap();
    assert_eq!(cycle.after, Some(rewound));

    // The plan is a serializable perform command.
    let encoded = serde_json::to_value(&next).unwrap();
    let decoded: norn_dataplane::SweepCommand = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded, next);
}

#[derive(Deserialize)]
struct Delivery {
    event: String,
    doc: Value,
}

#[derive(Deserialize)]
struct ConvergenceFixture {
    #[allow(dead_code, reason = "documents the fixture")]
    description: String,
    doctype: String,
    delivered: Vec<Delivery>,
    all_events: Vec<Delivery>,
    sweep_documents_response: Value,
    sweep_document_responses: Vec<Value>,
    sweep_deletions_response: Value,
}

fn deliver(deliveries: &[Delivery], start: u64) -> Vec<RawObservation> {
    deliveries
        .iter()
        .zip(start..)
        .map(|(delivery, at)| {
            frappe(&json!({"event": delivery.event, "doc": delivery.doc}), at).unwrap()
        })
        .collect()
}

fn summary(view: &Convergence) -> Vec<(String, ObservationKind, String, String, Value)> {
    view.records()
        .map(|(_, source_id, record)| {
            (
                source_id.to_owned(),
                record.kind,
                record.event_id.clone(),
                record.source_version.clone(),
                record.payload.clone(),
            )
        })
        .collect()
}

#[test]
fn webhook_ingest_duplicate_and_missed_webhooks_converge_through_the_sweep() {
    let path = format!(
        "{}/tests/fixtures/webhook_ingest/frappe_sales_order.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let fixture: ConvergenceFixture =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let sweep = FrappeSweep {
        doctype: fixture.doctype.clone(),
        ..sales_order_sweep()
    };

    let truth = Convergence::from_log(&deliver(&fixture.all_events, 1_000));

    // What reached Norn: one duplicate, and the SO-0002 submit and SO-0001
    // trash were lost.
    let delivered = deliver(&fixture.delivered, 2_000);
    let mut view = Convergence::new();
    let applied: Vec<Applied> = delivered.iter().map(|o| view.apply(o)).collect();
    assert_eq!(
        applied,
        [
            Applied::Advanced,
            Applied::Duplicate,
            Applied::Advanced,
            Applied::Advanced
        ]
    );
    assert_ne!(
        summary(&view),
        summary(&truth),
        "two deliveries were missed"
    );

    // One sweep cycle over both feeds.
    let documents = sweep.start_cycle(SweepFeed::Documents, None).unwrap();
    let fetches = sweep
        .plan_documents(&documents, &fixture.sweep_documents_response)
        .unwrap();
    assert_eq!(fetches.len(), fixture.sweep_document_responses.len());
    for (fetch, response) in fetches.iter().zip(&fixture.sweep_document_responses) {
        assert_eq!(fetch.method, "GET");
        assert_eq!(fetch.path_segments, ["api", "method", "frappe.client.get"]);
        assert_eq!(
            fetch.query,
            vec![
                ("doctype".to_owned(), "Sales Order".to_owned()),
                (
                    "name".to_owned(),
                    response["message"]["name"].as_str().unwrap().to_owned()
                )
            ]
        );
        let decoded: norn_dataplane::SweepDocumentCommand =
            serde_json::from_value(serde_json::to_value(fetch).unwrap()).unwrap();
        assert_eq!(&decoded, fetch);
    }
    let page = sweep
        .ingest(
            &documents,
            &fixture.sweep_documents_response,
            &fixture.sweep_document_responses,
            3_000,
        )
        .unwrap();
    assert!(page.exhausted || page.observations.len() == 2);
    assert_eq!(
        page.next.as_ref().map(|cursor| cursor.name.as_str()),
        Some("SO-0002")
    );
    let swept: Vec<Applied> = page.observations.iter().map(|o| view.apply(o)).collect();
    // SO-0003 was already delivered (duplicate); the SO-0002 submit was missed.
    assert_eq!(swept, [Applied::Duplicate, Applied::Advanced]);
    for observation in &page.observations {
        assert_eq!(observation.pipeline_version, FRAPPE_SWEEP_PIPELINE);
        assert!(observation.cursor.is_some());
        observation.validate().unwrap();
    }

    let deletions = sweep.start_cycle(SweepFeed::Deletions, None).unwrap();
    let page = sweep
        .ingest(&deletions, &fixture.sweep_deletions_response, &[], 3_001)
        .unwrap();
    assert!(page.exhausted);
    assert_eq!(page.observations.len(), 1);
    assert_eq!(page.observations[0].kind, ObservationKind::Delete);
    assert_eq!(view.apply(&page.observations[0]), Applied::Advanced);

    // Converged: complete payloads, including child tables, match webhooks.
    assert_eq!(summary(&view), summary(&truth));
    assert_eq!(
        view.current("erpnext", "Sales Order/SO-0002")
            .unwrap()
            .payload["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // Reverse the delivery order: the sweep arrives before any webhook.
    let live = sweep
        .ingest(
            &documents,
            &fixture.sweep_documents_response,
            &fixture.sweep_document_responses,
            3_002,
        )
        .unwrap();
    let mut sweep_first = Convergence::from_log(live.observations.iter().chain(&page.observations));
    for observation in deliver(&fixture.all_events, 3_100) {
        sweep_first.apply(&observation);
    }
    let payload = &sweep_first
        .current("erpnext", "Sales Order/SO-0002")
        .unwrap()
        .payload;
    assert!(payload.as_object().unwrap().contains_key("delivery_date"));
    assert_eq!(payload["delivery_date"], Value::Null);
    assert!(
        payload["items"][0]
            .as_object()
            .unwrap()
            .contains_key("description")
    );
    assert_eq!(payload["items"][0]["description"], Value::Null);
    assert_eq!(summary(&sweep_first), summary(&truth));
    assert_eq!(summary(&sweep_first), summary(&view));

    // A second sweep (and webhook redeliveries) change nothing.
    let before = summary(&view);
    for observation in sweep
        .ingest(
            &documents,
            &fixture.sweep_documents_response,
            &fixture.sweep_document_responses,
            4_000,
        )
        .unwrap()
        .observations
        .iter()
        .chain(&delivered)
    {
        assert_eq!(view.apply(observation), Applied::Duplicate);
    }
    assert_eq!(summary(&view), before);

    // The sweep and the webhook name the same event identically, so the
    // cell inbox dedups them too.
    let webhook_so3 = &delivered[3];
    let resweep = sweep
        .ingest(
            &documents,
            &fixture.sweep_documents_response,
            &fixture.sweep_document_responses,
            5_000,
        )
        .unwrap();
    let swept_so3 = &resweep.observations[0];
    assert_eq!(swept_so3.observation_id(), webhook_so3.observation_id());
    assert_eq!(
        swept_so3.submission().unwrap().event_id,
        webhook_so3.submission().unwrap().event_id
    );
}

#[test]
fn webhook_ingest_sweep_refuses_malformed_pages() {
    let sweep = sales_order_sweep();
    let documents = sweep.plan(SweepFeed::Documents, None).unwrap();
    assert!(matches!(
        sweep.ingest(&documents, &json!({"message": []}), &[], 1),
        Err(IngestError::MissingField { field: "data", .. })
    ));
    assert!(matches!(
        sweep.plan_documents(&documents, &json!({"data": [{"name": "SO-1"}]})),
        Err(IngestError::MissingField {
            field: "modified",
            ..
        })
    ));
    let empty = sweep
        .ingest(&documents, &json!({"data": []}), &[], 1)
        .unwrap();
    assert!(empty.exhausted);
    assert!(empty.next.is_none());

    let deletions = sweep.plan(SweepFeed::Deletions, None).unwrap();
    let foreign = json!({"data": [{
        "name": "DEL-9",
        "deleted_doctype": "Customer",
        "deleted_name": "CUST-1",
        "modified": "2026-10-01 09:00:00",
        "data": "{\"name\": \"CUST-1\", \"modified\": \"2026-10-01 08:00:00\"}"
    }]});
    assert!(matches!(
        sweep.ingest(&deletions, &foreign, &[], 1),
        Err(IngestError::InvalidSweep { .. })
    ));
}

#[test]
fn webhook_ingest_sweep_requires_every_full_document_before_advancing() {
    let sweep = sales_order_sweep();
    let command = sweep.plan(SweepFeed::Documents, None).unwrap();
    let listing = json!({"data": [
        {"name": "SO-1", "modified": "2026-10-01 09:00:00"},
        {"name": "SO-2", "modified": "2026-10-01 09:01:00"}
    ]});
    let responses = vec![
        json!({"message": {"doctype": "Sales Order", "name": "SO-1", "modified": "2026-10-01 09:00:00.000000", "items": [{"item_code": "A", "qty": 1}]}}),
        json!({"message": {"doctype": "Sales Order", "name": "SO-2", "modified": "2026-10-01 09:01:00.000000", "items": [{"item_code": "B", "qty": 2}]}}),
    ];
    let fetches = sweep.plan_documents(&command, &listing).unwrap();
    assert_eq!(fetches.len(), 2);
    assert_eq!(
        fetches[1].path_segments,
        ["api", "method", "frappe.client.get"]
    );
    assert_eq!(fetches[1].cursor.modified, "2026-10-01 09:01:00.000000");

    // Listing rows alone, partial results, and excess results never produce
    // observations or a cursor; even one malformed final result rejects all.
    for missing in [&[][..], &responses[..1]] {
        assert!(matches!(
            sweep.ingest(&command, &listing, missing, 1),
            Err(IngestError::InvalidSweep { .. })
        ));
    }
    let mut extra = responses.clone();
    extra.push(responses[0].clone());
    assert!(matches!(
        sweep.ingest(&command, &listing, &extra, 1),
        Err(IngestError::InvalidSweep { .. })
    ));
    for bad in [
        json!({"message": "fetch failed"}),
        json!({"message": []}),
        json!({"data": responses[1]["message"]}),
        listing["data"][1].clone(),
    ] {
        let mut malformed = responses.clone();
        malformed[1] = bad;
        assert!(matches!(
            sweep.ingest(&command, &listing, &malformed, 1),
            Err(IngestError::MissingField { .. })
        ));
    }
    for (field, value) in [
        ("doctype", "Customer"),
        ("name", "SO-OTHER"),
        ("modified", "2026-10-01 09:00:00"),
        ("modified", "2026-10-01 09:02:00"),
    ] {
        let mut mismatched = responses.clone();
        mismatched[1]["message"][field] = json!(value);
        assert!(matches!(
            sweep.ingest(&command, &listing, &mismatched, 1),
            Err(IngestError::InvalidSweep { .. })
        ));
    }
    let mut reversed = responses.clone();
    reversed.reverse();
    assert!(sweep.ingest(&command, &listing, &reversed, 1).is_err());

    let page = sweep.ingest(&command, &listing, &responses, 2).unwrap();
    assert_eq!(page.observations.len(), 2);
    assert_eq!(page.next.as_ref(), Some(&fetches[1].cursor));
    assert!(!page.exhausted);
    assert_eq!(page.observations[1].payload, responses[1]["message"]);
    assert_eq!(page.observations[1].payload["items"][0]["item_code"], "B");

    let deletions = sweep.plan(SweepFeed::Deletions, None).unwrap();
    assert!(
        sweep
            .plan_documents(&deletions, &json!({"data": []}))
            .is_err()
    );
    assert!(
        sweep
            .ingest(&deletions, &json!({"data": []}), &responses, 2)
            .is_err()
    );
}

#[test]
fn webhook_ingest_source_decoders_refuse_inexact_numeric_evidence() {
    let frappe_body = br#"{"doctype":"Sales Order","name":"SO-1","modified":"2026-10-01 09:00:00","amount":9007199254740993.0}"#;
    let signature = sign(FRAPPE_SECRET, frappe_body);
    assert!(matches!(
        ingest_frappe_webhook(&erpnext(), FRAPPE_SECRET, frappe_body, Some(&signature), 1),
        Err(IngestError::Observation(ObservationError::LossyPayload))
    ));
    let shopify_body =
        br#"{"id":1,"updated_at":"2026-10-01T09:00:00Z","amount":9007199254740993.0}"#;
    let headers = shopify_headers("orders/updated", shopify_body);
    assert!(matches!(
        ingest_shopify_webhook(&shop(), SHOPIFY_SECRET, &headers, shopify_body, 1),
        Err(IngestError::Observation(ObservationError::LossyPayload))
    ));
    let sweep = sales_order_sweep();
    let command = sweep.plan(SweepFeed::Deletions, None).unwrap();
    let listing = json!({"data": [{
        "name": "DEL-1", "deleted_doctype": "Sales Order", "deleted_name": "SO-1",
        "modified": "2026-10-01 09:01:00",
        "data": std::str::from_utf8(frappe_body).unwrap()
    }]});
    assert!(matches!(
        sweep.ingest(&command, &listing, &[], 1),
        Err(IngestError::Observation(ObservationError::LossyPayload))
    ));
}

#[test]
fn webhook_ingest_sweep_document_queries_preserve_unencoded_identity() {
    let sweep = FrappeSweep {
        doctype: "Sales / Order & 雪".to_owned(),
        ..sales_order_sweep()
    };
    let command = sweep.plan(SweepFeed::Documents, None).unwrap();
    let listing =
        json!({"data": [{"name": "SO/1?&name=OTHER", "modified": "2026-10-01 09:00:00"}]});
    let fetches = sweep.plan_documents(&command, &listing).unwrap();
    assert_eq!(
        fetches[0].path_segments,
        ["api", "method", "frappe.client.get"]
    );
    assert_eq!(
        fetches[0].query,
        vec![
            ("doctype".to_owned(), "Sales / Order & 雪".to_owned()),
            ("name".to_owned(), "SO/1?&name=OTHER".to_owned()),
        ]
    );
    let responses = [json!({"message": {
        "doctype": sweep.doctype, "name": "SO/1?&name=OTHER",
        "modified": "2026-10-01 09:00:00", "optional": null,
        "items": [{"description": null}]
    }})];
    let page = sweep.ingest(&command, &listing, &responses, 1).unwrap();
    assert_eq!(page.observations[0].payload, responses[0]["message"]);
}
