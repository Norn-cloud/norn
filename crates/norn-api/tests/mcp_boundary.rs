#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "protocol correspondence assertions"
)]
use norn_api::{
    mcp::{Backend, CallOutput, Session},
    refusal::Refusal,
};
use norn_ir::{McpTool, RuntimeObject};
use serde_json::{Value, json};
struct Refusing;
impl Backend for Refusing {
    fn call(
        &mut self,
        _: &str,
        _: RuntimeObject,
        _metadata: RuntimeObject,
    ) -> Result<CallOutput, Refusal> {
        Err(Refusal::invalid("repair the named argument"))
    }
}
fn tool(name: &str) -> McpTool {
    serde_json::from_value(json!({"name":name,"description":"Complete the declared task", "inputSchema":{"type":"object"}})).unwrap()
}
fn send(s: &mut Session, v: &Value) -> Value {
    serde_json::from_str(&s.handle(&v.to_string(), &mut Refusing).unwrap().unwrap()).unwrap()
}
fn init(s: &mut Session) {
    send(
        s,
        &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}),
    );
}
#[test]
fn pagination_binds_revision_and_refusals_keep_shared_payload() {
    let mut s = Session::new(vec![tool("a"), tool("b")], "scope/actor/revision-1").unwrap();
    init(&mut s);
    let first = send(
        &mut s,
        &json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{"limit":1}}),
    );
    let cursor = &first["result"]["nextCursor"];
    let next = send(
        &mut s,
        &json!({"jsonrpc":"2.0","id":3,"method":"tools/list","params":{"cursor":cursor,"limit":1}}),
    );
    assert_eq!(next["result"]["tools"][0]["name"], "b");
    assert!(next["result"].get("nextCursor").is_none());
    let mut changed = Session::new(vec![tool("a"), tool("b")], "scope/actor/revision-2").unwrap();
    init(&mut changed);
    assert!(
        send(
            &mut changed,
            &json!({"jsonrpc":"2.0","id":4,"method":"tools/list","params":{"cursor":cursor}})
        )
        .get("error")
        .is_some()
    );
    let refused = send(
        &mut s,
        &json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"a","arguments":{}}}),
    );
    assert_eq!(refused["result"]["isError"], true);
    assert_eq!(
        refused["result"]["structuredContent"],
        serde_json::to_value(Refusal::invalid("repair the named argument")).unwrap()
    );
}
#[test]
fn notifications_never_dispatch_and_duplicate_names_refuse() {
    let mut s = Session::new(vec![tool("a")], "scope").unwrap();
    init(&mut s);
    assert!(
        s.handle(
            r#"{"jsonrpc":"2.0","method":"tools/call","params":{"name":"a"}}"#,
            &mut Refusing
        )
        .unwrap()
        .is_none()
    );
    assert!(Session::new(vec![tool("a"), tool("a")], "scope").is_err());
}
#[test]
fn typed_invariant_pins_and_redaction_survive_without_transport_reinterpretation() {
    use norn_kernel::{CommitError, FactKey, Refusable};
    struct Stored(Refusal);
    impl Backend for Stored {
        fn call(
            &mut self,
            _: &str,
            _: RuntimeObject,
            _metadata: RuntimeObject,
        ) -> Result<CallOutput, Refusal> {
            Err(self.0.clone())
        }
    }
    let error = CommitError::<std::convert::Infallible>::InvariantViolation {
        key: Box::new(FactKey {
            object_type: norn_ir::ObjectTypeName("demo.Item".into()),
            key: "one".into(),
        }),
        owner: norn_ir::InvariantOwner::Object(norn_ir::ObjectTypeName("demo.Item".into())),
        version: "sha256:definition".into(),
        id: norn_ir::InvariantId("positive".into()),
        message: "must be positive".into(),
        outcome: Box::new(norn_ir::DataQualityOutcome::NotSatisfied),
    };
    for allowed in [true, false] {
        let refusal = Refusal::from_kernel(error.refusal(), allowed);
        let mut session = Session::new(vec![tool("demo.action")], "authorized-snapshot").unwrap();
        init(&mut session);
        let raw=session.handle(r#"{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"demo.action","arguments":{}}}"#,&mut Stored(refusal.clone())).unwrap().unwrap();
        let value: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            value["result"]["structuredContent"],
            serde_json::to_value(&refusal).unwrap()
        );
        assert_eq!(
            serde_json::from_str::<Value>(value["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap(),
            value["result"]["structuredContent"]
        );
        assert_eq!(refusal.reason.is_some(), allowed);
    }
}

#[test]
fn exact_arguments_reach_the_backend_and_wire_unchanged() {
    struct Echo;
    impl Backend for Echo {
        fn call(
            &mut self,
            _: &str,
            arguments: RuntimeObject,
            _metadata: RuntimeObject,
        ) -> Result<CallOutput, Refusal> {
            Ok(CallOutput {
                value: norn_ir::RuntimeValue::Object(arguments),
                is_error: false,
            })
        }
    }
    let mut session = Session::new(vec![tool("exact")], "snapshot").unwrap();
    init(&mut session);
    let output=session.handle(r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"exact","arguments":{"mass":12345678901234567890.123456789}}}"#,&mut Echo).unwrap().unwrap();
    assert!(output.contains("12345678901234567890.123456789"));
}

#[test]
fn malformed_params_keep_valid_ids_and_notifications_stay_silent() {
    let mut session = Session::new(vec![tool("a")], "snapshot").unwrap();
    init(&mut session);
    for id in [json!(91), json!("request-91")] {
        for params in [
            json!(false),
            json!(["bad"]),
            json!(null),
            json!({"limit":"bad"}),
            json!({"arguments":7}),
        ] {
            let result = send(
                &mut session,
                &json!({"jsonrpc":"2.0","id":id,"method":"tools/list","params":params}),
            );
            assert_eq!(result["id"], id);
            assert_eq!(result["error"]["code"], -32602);
            let notification = json!({"jsonrpc":"2.0","method":"tools/call","params":params});
            assert!(
                session
                    .handle(&notification.to_string(), &mut Refusing)
                    .unwrap()
                    .is_none()
            );
        }
    }
    let invalid = send(
        &mut session,
        &json!({"jsonrpc":"1.0","id":"bad-envelope","method":"tools/list"}),
    );
    assert!(invalid["id"].is_null());
    assert_eq!(invalid["error"]["code"], -32600);
}

#[test]
fn ping_is_empty_before_and_after_initialization_without_initializing_tools() {
    let mut session = Session::new(vec![tool("a")], "snapshot").unwrap();
    let ping = json!({"jsonrpc":"2.0","id":"alive","method":"ping"});
    let before = send(&mut session, &ping);
    assert_eq!(before, json!({"jsonrpc":"2.0","id":"alive","result":{}}));
    let blocked = send(
        &mut session,
        &json!({"jsonrpc":"2.0","id":8,"method":"tools/list"}),
    );
    assert_eq!(blocked["error"]["code"], -32000);
    init(&mut session);
    assert_eq!(send(&mut session, &ping), before);
    let unknown = send(
        &mut session,
        &json!({"jsonrpc":"2.0","id":9,"method":"unknown"}),
    );
    assert_eq!(unknown["error"]["code"], -32601);
}
