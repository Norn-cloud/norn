//! Observed-only, integer FIFO projection for the frozen seven-checkpoint cut.
//! Unit = one gram is a synthetic binding; time and rounding parity are excluded.
use super::{Cell, Fixture, PREFIX, key, open, scaled, seed_book, values};
use norn_kernel::cell_host::JsonObject;
use norn_kernel::{CellHost, LinkKey};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const FROZEN: &str = include_str!("../fixtures/stock-parity-fifo.json");

fn decimal(amount: i128, scale: u32) -> String {
    let sign = if amount < 0 { "-" } else { "" };
    let magnitude = amount.checked_abs().unwrap();
    let divisor = 10_i128.pow(scale);
    let text = format!(
        "{sign}{}.{:0width$}",
        magnitude / divisor,
        magnitude % divisor,
        width = usize::try_from(scale).unwrap()
    );
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

fn native_rows(cell: &Cell, kind: &str) -> BTreeMap<String, JsonObject> {
    let (found, complete) = cell
        .store()
        .read()
        .root_rows("main", &format!("{PREFIX}{kind}"), 256)
        .unwrap();
    assert!(complete, "parity observation requires complete enumeration");
    found
        .into_iter()
        .map(|(id, _)| {
            let row = values(cell, kind, &id);
            (id, row)
        })
        .collect()
}

fn linked(
    cell: &Cell,
    source_kind: &str,
    source: &str,
    link: &str,
    target_kind: &str,
    target: &str,
) -> bool {
    cell.load_link(&LinkKey::new(
        key(source_kind, source),
        link,
        key(target_kind, target),
    ))
    .unwrap()
    .exists()
}

fn balance(cell: &Cell) -> (i128, i128) {
    let mut quantity = 0;
    let mut value = 0;
    for (id, row) in &native_rows(cell, "Holding") {
        assert!(linked(cell, "Holding", id, "vault", "Vault", "VAULT"));
        assert!(linked(cell, "Vault", "VAULT", "holdings", "Holding", id));
        assert_eq!(row["unit"].as_str(), Some("g"));
        assert_eq!(row["valuation_method"].as_str(), Some("fifo"));
        assert_eq!(row["valuation_currency"].as_str(), Some("USD"));
        assert_eq!(row["disposition"].as_str(), Some("available"));
        quantity += scaled(&row["quantity"], 3);
        value += scaled(&row["value"], 2);
    }
    (quantity, value)
}

fn snapshot(cell: &Cell, fixture: &Fixture) -> Value {
    let database = cell.store().read().dump().unwrap();
    let head = cell.store().read().verify_ledger().unwrap();
    let reopened = open(&cell.path, fixture);
    assert_eq!(database, reopened.store().read().dump().unwrap());
    assert_eq!(head, reopened.store().read().verify_ledger().unwrap());
    let roots: BTreeMap<_, _> = ["Holding", "Allocation", "Event", "Leg", "Exchange"]
        .into_iter()
        .map(|kind| (kind, native_rows(cell, kind)))
        .collect();
    json!({"roots":roots,"database":database,"ledgerHead":{"seq":head.seq,"hash":head.hash,"commits":head.commits},"reopenEqual":true})
}

fn empty_checkpoint() -> Value {
    json!({"step":-1,"attempts":[],"stock":[],"gl":[],"documents":[]})
}

fn receipt_input(index: usize, operation: &Value) -> Value {
    let id = format!("R{}", index + 1);
    // Frozen whole-number inputs are parsed as exact JSON integer tokens.
    let quantity: serde_json::Number = operation["quantity"].as_str().unwrap().parse().unwrap();
    let rate: serde_json::Number = operation["rate"].as_str().unwrap().parse().unwrap();
    json!({"id":format!("RECEIPT-{id}"),"holding":id,"vault":"VAULT","company":"BUSINESS","party":"PARTY","event":format!("{id}-EVENT"),"debit":format!("{id}-INVENTORY"),"credit":format!("{id}-RNB"),"quantity":quantity,"rate":rate,"valuation_method":"fifo"})
}

fn delivery_input(single: bool, operation: &Value) -> Value {
    let quantity: serde_json::Number = operation["quantity"].as_str().unwrap().parse().unwrap();
    let mut input = json!({"id":"DELIVERY","company":"BUSINESS","party":"PARTY","event":"DELIVERY-EVENT","debit":"DELIVERY-COGS","credit":"DELIVERY-STOCK","quantity":quantity});
    if single {
        input["holding"] = json!("R1");
        input["allocation"] = json!("DELIVERY-ALLOCATION");
    } else {
        input["first"] = json!("R1");
        input["second"] = json!("R2");
        input["allocation1"] = json!("DELIVERY-ALLOCATION-1");
        input["allocation2"] = json!("DELIVERY-ALLOCATION-2");
    }
    input
}

fn observe_case(trace: &Value, fixture: &Fixture) -> (Value, Vec<Value>) {
    let directory = tempfile::tempdir().unwrap();
    let mut cell = open(&directory.path().join("parity.sqlite"), fixture);
    seed_book(&mut cell, false);
    assert_eq!(balance(&cell), (0, 0));
    assert!(native_rows(&cell, "Holding").is_empty());
    assert!(native_rows(&cell, "Allocation").is_empty());
    assert!(native_rows(&cell, "Event").is_empty());
    assert!(native_rows(&cell, "Leg").is_empty());
    assert!(native_rows(&cell, "Exchange").is_empty());
    let mut evidence = vec![snapshot(&cell, fixture)];
    let mut stock = Vec::<Value>::new();
    let mut checkpoints = Vec::new();
    let mut event_rows = BTreeMap::<String, JsonObject>::new();
    let mut leg_rows = BTreeMap::<String, JsonObject>::new();
    let mut leg_requests = BTreeMap::<String, String>::new();
    let mut document_requests = BTreeMap::<String, String>::new();
    for (index, step) in trace["steps"].as_array().unwrap().iter().enumerate() {
        let command = &step["command"];
        let request = command["requestId"].as_str().unwrap();
        let operation = &command["operation"];
        let previous_balance = balance(&cell);
        let receipt = operation["kind"] == "receipt";
        let single = trace["id"] == "receipt-delivery";
        let input = if receipt {
            receipt_input(index, operation)
        } else {
            delivery_input(single, operation)
        };
        let name = if receipt {
            "receive"
        } else if single {
            "deliver_single"
        } else {
            "deliver"
        };
        fixture
            .invoke(
                &mut cell,
                "createexchange",
                json!({"id":input["id"]}),
                "principal:operator",
                &format!("draft:{request}"),
            )
            .unwrap();
        fixture
            .invoke(&mut cell, name, input, "principal:operator", request)
            .unwrap();
        let events = native_rows(&cell, "Event");
        for (id, old) in &event_rows {
            assert_eq!(
                events.get(id),
                Some(old),
                "prior economic event remains immutable"
            );
        }
        let new_events: Vec<_> = events
            .iter()
            .filter(|(id, _)| !event_rows.contains_key(*id))
            .collect();
        assert_eq!(new_events.len(), 1);
        let (event_id, event) = new_events[0];
        assert_eq!(event["status"].as_str(), Some("posted"));
        let received = linked(&cell, "Event", event_id, "receiver", "Agent", "BUSINESS");
        let delivered = linked(&cell, "Event", event_id, "provider", "Agent", "BUSINESS");
        assert_ne!(
            received, delivered,
            "the observed movement has one company side"
        );
        let quantity_delta = scaled(&event["quantity"], 3) * if received { 1 } else { -1 };
        let legs = native_rows(&cell, "Leg");
        for (id, old) in &leg_rows {
            assert_eq!(legs.get(id), Some(old), "prior GL leg remains immutable");
        }
        let mut value_delta = 0;
        let mut new_legs = 0;
        for (id, leg) in &legs {
            if !leg_rows.contains_key(id) {
                assert!(linked(&cell, "Leg", id, "event", "Event", event_id));
                assert_eq!(leg["currency"].as_str(), Some("USD"));
                assert_eq!(leg["status"].as_str(), Some("posted"));
                if leg["account_code"].as_str() == Some("Inventory") {
                    value_delta += scaled(&leg["amount"], 2);
                }
                leg_requests.insert(id.clone(), request.to_owned());
                new_legs += 1;
            }
        }
        assert_eq!(new_legs, 2);
        let (quantity, value) = balance(&cell);
        assert_eq!(quantity - previous_balance.0, quantity_delta);
        assert_eq!(value - previous_balance.1, value_delta);
        assert!(cell.store().read().pending_commands().unwrap().is_empty());
        stock.push(json!({"id":event_id,"requestId":request,"line":format!("{request}:1"),"warehouse":"a","quantityDelta":decimal(quantity_delta,3),"valueDelta":decimal(value_delta,2),"quantity":decimal(quantity,3),"value":decimal(value,2),"incomingRate":null,"valuationRate":null,"fifo":null,"cancelled":false}));
        let mut gross_balance = 0;
        let gl: Vec<_> = legs.iter().map(|(id, leg)| {
            let amount = scaled(&leg["amount"], 2);
            gross_balance += amount;
            let account = match leg["account_code"].as_str().unwrap() {
                "Inventory" => "stock-a", "ReceivedNotBilled" => "srbnb", "COGS" => "cogs",
                other => panic!("unmapped account {other}"),
            };
            json!({"id":id,"requestId":leg_requests[id],"account":account,"currency":"USD","debit":decimal(amount.max(0),2),"credit":decimal((-amount).max(0),2),"cancelled":false})
        }).collect();
        assert_eq!(gross_balance, 0);
        let documents: Vec<_> = native_rows(&cell, "Exchange")
            .iter()
            .map(|(id, row)| {
                if !document_requests.contains_key(id) {
                    assert!(linked(&cell, "Exchange", id, "delivery", "Event", event_id));
                    document_requests.insert(id.clone(), request.to_owned());
                }
                let status = match row["status"].as_str().unwrap() {
                    "posted" => 1,
                    "draft" => 0,
                    other => panic!("unsupported exchange status {other}"),
                };
                json!({"requestId":document_requests[id],"status":status})
            })
            .collect();
        checkpoints.push(json!({"step":index,"attempts":[{"attempt":0,"requestId":request,"nativeId":event_id,"outcome":"accepted","nativeCode":null,"reason":null,"completionOrder":0}],"stock":stock,"gl":gl,"documents":documents}));
        evidence.push(snapshot(&cell, fixture));
        event_rows = events;
        leg_rows = legs;
    }
    (
        json!({"case":trace["id"],"method":"fifo","status":"complete","reason":null,"initial":empty_checkpoint(),"checkpoints":checkpoints}),
        evidence,
    )
}

fn observe(traces: &[Value]) -> Value {
    let frozen: Vec<Value> = serde_json::from_str(FROZEN).unwrap();
    let fixture = Fixture::load();
    let mut cases = Vec::new();
    let mut durability = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for trace in traces {
        if !frozen.contains(trace) || !seen.insert(trace["id"].to_string()) {
            cases.push(json!({"case":trace["id"],"method":trace["method"],"status":"unsupported","reason":"outside frozen integer/cent-exact FIFO profile","checkpoints":[]}));
            continue;
        }
        let (case, snapshots) = observe_case(trace, &fixture);
        durability.push(json!({"case":trace["id"],"checkpoints":snapshots}));
        cases.push(case);
    }
    json!({"format":"norn.stock-dump/1","engine":"norn","versions":{"norn-cell":env!("CARGO_PKG_VERSION"),"fixtureBinding":"one ERPNext Unit = one gram; integer/cent-exact projection only; no time or rounding parity","admittedArtifactSha256":fixture.admission_hash,"typeEnvSha256":fixture.environment_hash,"exprProfile":fixture.expr_profile,"durability":durability},"cases":cases})
}

#[test]
fn fifo_trace_observation_dump() {
    let input = std::env::var_os("NORN_PARITY_INPUT").map_or_else(
        || FROZEN.to_owned(),
        |path| std::fs::read_to_string(path).unwrap(),
    );
    let traces: Vec<Value> = serde_json::from_str(&input).unwrap();
    let dump = observe(&traces);
    if let Some(path) = std::env::var_os("NORN_PARITY_OUTPUT") {
        std::fs::write(path, serde_json::to_vec_pretty(&dump).unwrap()).unwrap();
    }
    assert_eq!(dump["cases"].as_array().unwrap().len(), 2);
    assert!(
        dump["cases"]
            .as_array()
            .unwrap()
            .iter()
            .all(|case| case["status"] == "complete")
    );
    assert_eq!(
        dump["cases"]
            .as_array()
            .unwrap()
            .iter()
            .map(|case| 1 + case["checkpoints"].as_array().unwrap().len())
            .sum::<usize>(),
        7
    );
}

#[test]
fn unsupported_profile_cannot_be_reported_as_parity() {
    let mut traces: Vec<Value> = serde_json::from_str(FROZEN).unwrap();
    traces[0]["rounding"] = json!("refuse");
    traces[1]["steps"][0]["command"]["operation"]["quantity"] = json!("10.0001");
    let dump = observe(&traces);
    assert!(
        dump["cases"]
            .as_array()
            .unwrap()
            .iter()
            .all(|case| case["status"] == "unsupported")
    );
    assert!(
        dump["versions"]["durability"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
