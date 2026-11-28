//! Stdio adapter over the human command catalog; unavailable backends are absent.
use super::Cli;
use clap::CommandFactory as _;
use norn_api::{
    mcp::{Backend, CallOutput, Session},
    refusal::Refusal,
};
use norn_ir::{McpTool, RuntimeObject, RuntimeValue};
use std::io::{BufRead as _, Read as _};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const MAX_MCP_LINE_BYTES: usize = 1024 * 1024;
const MCP_DISCARD_CHUNK_BYTES: u64 = 4096;

fn platform_tools() -> Result<Vec<McpTool>, String> {
    static TOOLS: OnceLock<Result<Vec<McpTool>, String>> = OnceLock::new();
    TOOLS
        .get_or_init(|| {
            let mut command = Cli::command();
            command.build();
            crate::mcp_tools::PLATFORM_TOOLS
                .iter()
                .map(|name| {
                    let operation = command
                        .find_subcommand(name)
                        .ok_or_else(|| format!("missing {name} command"))?;
                    crate::mcp_tools::command_tool(operation)
                })
                .collect()
        })
        .clone()
}

struct Human {
    workspace_root: Option<PathBuf>,
}

impl Human {
    const fn current_dir() -> Self {
        Self {
            workspace_root: None,
        }
    }

    #[cfg(test)]
    const fn with_workspace(root: PathBuf) -> Self {
        Self {
            workspace_root: Some(root),
        }
    }
}

impl Backend for Human {
    fn call(
        &mut self,
        name: &str,
        arguments: RuntimeObject,
        _metadata: RuntimeObject,
    ) -> Result<CallOutput, Refusal> {
        let workspace_root = match &self.workspace_root {
            Some(root) => root.clone(),
            None => std::env::current_dir().map_err(Refusal::host)?,
        };
        crate::mcp_tools::call_tool(name, arguments, &workspace_root)
    }
}

pub(crate) fn catalog_session_for(workspace_root: &Path) -> Result<Session, Refusal> {
    let binding = workspace_root.to_string_lossy().into_owned();
    Session::new(platform_tools().map_err(Refusal::host)?, &binding)
}

fn catalog_session() -> Result<Session, Refusal> {
    let cwd = std::env::current_dir().map_err(Refusal::host)?;
    catalog_session_for(&cwd)
}

pub(crate) fn capabilities_catalog_for(
    cursor: Option<&str>,
    workspace_root: &Path,
) -> Result<RuntimeValue, Refusal> {
    catalog_session_for(workspace_root)?.catalog(cursor, None)
}

fn capabilities_catalog(cursor: Option<&str>) -> Result<RuntimeValue, Refusal> {
    catalog_session()?.catalog(cursor, None)
}
pub(super) fn capabilities(cursor: Option<&str>) -> Result<(String, bool), String> {
    let catalog = capabilities_catalog(cursor).map_err(|e| e.explanation)?;
    let document = String::from_utf8(catalog.canonical_json().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    Ok((document, true))
}

fn drain_until_newline(input: &mut impl std::io::BufRead) -> Result<(), String> {
    let mut discard = Vec::new();
    loop {
        discard.clear();
        let read = input
            .by_ref()
            .take(MCP_DISCARD_CHUNK_BYTES)
            .read_until(b'\n', &mut discard)
            .map_err(|e| e.to_string())?;
        if read == 0 || discard.ends_with(b"\n") {
            return Ok(());
        }
    }
}

fn write_protocol_error(
    output: &mut impl std::io::Write,
    code: i32,
    message: &str,
) -> Result<(), String> {
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": serde_json::Value::Null,
        "error": {
            "code": code,
            "message": message,
        }
    });
    writeln!(output, "{payload}").map_err(|e| e.to_string())?;
    output.flush().map_err(|e| e.to_string())
}

fn serve_io(
    session: &mut Session,
    backend: &mut impl Backend,
    input: &mut impl std::io::BufRead,
    output: &mut impl std::io::Write,
) -> Result<(), String> {
    let mut line = Vec::new();
    let max_read = u64::try_from(MAX_MCP_LINE_BYTES)
        .map_err(|e| e.to_string())?
        .saturating_add(1);
    loop {
        line.clear();
        let bytes = input
            .by_ref()
            .take(max_read)
            .read_until(b'\n', &mut line)
            .map_err(|e| e.to_string())?;
        if bytes == 0 {
            break;
        }
        if bytes > MAX_MCP_LINE_BYTES {
            if !line.ends_with(b"\n") {
                drain_until_newline(input)?;
            }
            write_protocol_error(output, -32600, "MCP message exceeds 1 MiB")?;
            continue;
        }
        let Ok(source) = std::str::from_utf8(&line) else {
            write_protocol_error(output, -32700, "MCP message is not valid UTF-8")?;
            continue;
        };
        if let Some(response) = session.handle(source, backend).map_err(|e| e.message)? {
            writeln!(output, "{response}").map_err(|e| e.to_string())?;
            output.flush().map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

pub(super) fn serve() -> Result<(String, bool), String> {
    let mut session = catalog_session().map_err(|e| e.explanation)?;
    let mut backend = Human::current_dir();
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    serve_io(&mut session, &mut backend, &mut input, &mut output)?;
    Ok((String::new(), true))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "human/MCP correspondence assertions")]
mod tests {
    use super::{Human, capabilities, capabilities_catalog, catalog_session, catalog_session_for};
    use norn_api::mcp::Backend as _;
    use norn_ir::{RuntimeObject, RuntimeValue};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static MCP_TEST_SEQ: AtomicU64 = AtomicU64::new(0);

    struct ScratchWorkspace(PathBuf);

    impl ScratchWorkspace {
        fn new(label: &str) -> Self {
            let seq = MCP_TEST_SEQ.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir()
                .join(format!("norn-cli-mcp-{label}-{}-{seq}", std::process::id()));
            let _stale = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            Self(std::fs::canonicalize(&root).unwrap())
        }

        fn write(&self, rel: &str, content: &str) -> PathBuf {
            let path = self.0.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&path, content).unwrap();
            path
        }
    }

    impl Drop for ScratchWorkspace {
        fn drop(&mut self) {
            let _cleanup = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn capabilities_human_and_mcp_keep_catalog_identity_and_cursor() {
        let first = catalog_session().unwrap().catalog(None, Some(1)).unwrap();
        let cursor = first
            .as_object()
            .unwrap()
            .get("nextCursor")
            .unwrap()
            .as_str()
            .unwrap();
        for cursor in [None, Some(cursor)] {
            let (human, accepted) = capabilities(cursor).unwrap();
            assert!(accepted);
            let arguments = cursor.map_or_else(RuntimeObject::new, |cursor| {
                RuntimeObject::from([("cursor".into(), RuntimeValue::String(cursor.into()))])
            });
            let output = Human::current_dir()
                .call("norn.capabilities", arguments, RuntimeObject::new())
                .unwrap();
            assert!(!output.is_error);
            assert_eq!(output.value, RuntimeValue::from_json_text(&human).unwrap());
        }
    }

    #[test]
    fn capabilities_refusal_keeps_typed_payload_and_recovery_explanation() {
        let refusal = capabilities_catalog(Some("obsolete")).unwrap_err();
        let arguments =
            RuntimeObject::from([("cursor".into(), RuntimeValue::String("obsolete".into()))]);
        let projected = Human::current_dir()
            .call("norn.capabilities", arguments, RuntimeObject::new())
            .map(|_| ())
            .unwrap_err();
        assert_eq!(projected, refusal);
        assert_eq!(
            capabilities(Some("obsolete")).unwrap_err(),
            refusal.explanation
        );
        assert!(refusal.explanation.contains("restart"));
    }

    #[test]
    fn clap_flag_injection_and_outside_check_paths_are_refused() {
        let injected = Human::current_dir()
            .call(
                "norn.capabilities",
                RuntimeObject::from([("cursor".into(), RuntimeValue::String("--help".into()))]),
                RuntimeObject::new(),
            )
            .map(|_| ())
            .unwrap_err();
        assert!(injected.explanation.contains("restart"));

        let escaped = Human::current_dir()
            .call(
                "norn.check",
                RuntimeObject::from([
                    (
                        "artifacts".into(),
                        RuntimeValue::String("/etc/hosts".into()),
                    ),
                    ("context".into(), RuntimeValue::String("/etc/hosts".into())),
                ]),
                RuntimeObject::new(),
            )
            .map(|_| ())
            .unwrap_err();
        assert!(escaped.explanation.contains("working directory"));
    }

    #[test]
    fn serve_recovers_after_oversized_and_invalid_utf8_lines() {
        let mut session = catalog_session().unwrap().stateless();
        let mut backend = Human::current_dir();
        let mut raw = vec![b'a'; super::MAX_MCP_LINE_BYTES.saturating_add(16)];
        raw.push(b'\n');
        raw.extend_from_slice(&[0xff, 0xfe, b'\n']);
        raw.extend_from_slice(br#"{"jsonrpc":"2.0","id":7,"method":"tools/list"}"#);
        raw.push(b'\n');
        let mut cursor = std::io::Cursor::new(raw);
        let mut out = Vec::new();
        super::serve_io(&mut session, &mut backend, &mut cursor, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        let first: serde_json::Value = serde_json::from_str(lines.first().unwrap()).unwrap();
        assert_eq!(
            first.pointer("/error/code"),
            Some(&serde_json::json!(-32600))
        );
        let second: serde_json::Value = serde_json::from_str(lines.get(1).unwrap()).unwrap();
        assert_eq!(
            second.pointer("/error/code"),
            Some(&serde_json::json!(-32700))
        );
        let third: serde_json::Value = serde_json::from_str(lines.get(2).unwrap()).unwrap();
        assert_eq!(third.get("id"), Some(&serde_json::json!(7)));
        assert!(third.get("result").is_some());
    }

    fn mcp_call(
        session: &mut norn_api::mcp::Session,
        backend: &mut Human,
        id: u64,
        name: &str,
        arguments: &serde_json::Value,
    ) -> serde_json::Value {
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments }
        });
        let reply = session.handle(&req.to_string(), backend).unwrap().unwrap();
        serde_json::from_str(&reply).unwrap()
    }

    #[test]
    fn mcp_tools_list_advertises_expanded_read_only_tools_and_typed_schemas() {
        let ws = ScratchWorkspace::new("tools-list");
        let mut session = catalog_session_for(&ws.0).unwrap().stateless();
        let mut backend = Human::with_workspace(ws.0.clone());

        let list_reply = session
            .handle(
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
                &mut backend,
            )
            .unwrap()
            .unwrap();
        let list_json: serde_json::Value = serde_json::from_str(&list_reply).unwrap();
        let tools = list_json
            .pointer("/result/tools")
            .and_then(serde_json::Value::as_array)
            .unwrap();
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|tool| tool.get("name").and_then(serde_json::Value::as_str))
            .collect();
        for expected in [
            "norn.capabilities",
            "norn.check",
            "norn.compile",
            "norn.explain",
            "norn.fmt",
        ] {
            assert!(
                names.contains(&expected),
                "missing tool {expected} in {names:?}"
            );
        }
        let find_tool = |tool_name: &str| {
            tools
                .iter()
                .find(|tool| {
                    tool.get("name").and_then(serde_json::Value::as_str) == Some(tool_name)
                })
                .unwrap()
        };
        assert_eq!(
            find_tool("norn.check").pointer("/inputSchema/properties/forbid_unsafe_zen/type"),
            Some(&serde_json::json!("boolean"))
        );
        assert!(
            find_tool("norn.compile")
                .pointer("/inputSchema/properties/out")
                .is_none()
        );
        assert_eq!(
            find_tool("norn.compile").pointer("/inputSchema/properties/path/type"),
            Some(&serde_json::json!("string"))
        );
        assert_eq!(
            find_tool("norn.explain").pointer("/inputSchema/properties/kind/type"),
            Some(&serde_json::json!("string"))
        );
        assert_eq!(
            find_tool("norn.explain").pointer("/inputSchema/properties/base/type"),
            Some(&serde_json::json!("array"))
        );
        assert_eq!(
            find_tool("norn.fmt").pointer("/inputSchema/properties/check/type"),
            Some(&serde_json::json!("boolean"))
        );
    }

    #[test]
    fn mcp_check_source_admits_temp_ts_file_and_confined_directory() {
        let ws = ScratchWorkspace::new("check-source");
        ws.write(
            "src/model.ts",
            r#"import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Ticket = Norn.type({
  name: "ops.Ticket",
  title: "Ticket",
  version: "0.1.0",
  properties: Schema.Struct({
    ticket_id: Schema.String,
    status: Schema.Literals(["open", "closed"]),
  }),
  identity: [{ name: "primary", properties: ["ticket_id"] }],
})
"#,
        );
        ws.write(
            "app/norn.json",
            r#"{"format":"norn-project/1","name":"ops","sources":["defs"],"scope":{"orgId":"org_ops","environmentId":"dev"}}"#,
        );
        ws.write(
            "app/defs/ticket.ts",
            r#"import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Ticket = Norn.type({
  name: "ops.Ticket",
  title: "Ticket",
  version: "0.1.0",
  properties: Schema.Struct({ ticket_id: Schema.String }),
  identity: [{ name: "primary", properties: ["ticket_id"] }],
})
"#,
        );
        let mut session = catalog_session_for(&ws.0).unwrap().stateless();
        let mut backend = Human::with_workspace(ws.0.clone());

        let file_res = mcp_call(
            &mut session,
            &mut backend,
            2,
            "norn.check",
            &serde_json::json!({
                "source": "src/model.ts",
                "org": "org_ops",
                "env": "dev",
                "forbid_unsafe_zen": true
            }),
        );
        assert_eq!(
            file_res.pointer("/result/isError"),
            Some(&serde_json::json!(false)),
            "norn.check --source failed: {file_res}"
        );
        assert_eq!(
            file_res.pointer("/result/structuredContent/admitted"),
            Some(&serde_json::json!(true))
        );

        let dir_res = mcp_call(
            &mut session,
            &mut backend,
            3,
            "norn.check",
            &serde_json::json!({ "source": "app" }),
        );
        assert_eq!(
            dir_res.pointer("/result/structuredContent/admitted"),
            Some(&serde_json::json!(true)),
            "norn.check directory failed: {dir_res}"
        );
    }

    #[test]
    fn mcp_explain_compile_and_readonly_fmt_execute_cleanly() {
        let ws = ScratchWorkspace::new("explain-compile-fmt");
        ws.write(
            "src/model.ts",
            r#"import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Ticket = Norn.type({
  name: "ops.Ticket",
  title: "Ticket",
  version: "0.1.0",
  properties: Schema.Struct({ ticket_id: Schema.String }),
  identity: [{ name: "primary", properties: ["ticket_id"] }],
})
"#,
        );
        ws.write("src/unformatted.ts", "export const item={a:1,b:2};\n");

        let mut session = catalog_session_for(&ws.0).unwrap().stateless();
        let mut backend = Human::with_workspace(ws.0.clone());

        let explain_res = mcp_call(
            &mut session,
            &mut backend,
            4,
            "norn.explain",
            &serde_json::json!({
                "kind": "diagnostic",
                "id": "core.laws.preservation_violated",
                "format": "json"
            }),
        );
        assert_eq!(
            explain_res.pointer("/result/isError"),
            Some(&serde_json::json!(false)),
            "norn.explain diagnostic failed: {explain_res}"
        );
        assert_eq!(
            explain_res.pointer("/result/structuredContent/found"),
            Some(&serde_json::json!(true))
        );

        let compile_res = mcp_call(
            &mut session,
            &mut backend,
            5,
            "norn.compile",
            &serde_json::json!({
                "path": "src/model.ts",
                "org": "org_ops",
                "env": "dev"
            }),
        );
        assert_eq!(
            compile_res.pointer("/result/isError"),
            Some(&serde_json::json!(false)),
            "norn.compile failed: {compile_res}"
        );
        assert!(
            compile_res
                .pointer("/result/structuredContent/artifacts")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|items| !items.is_empty())
        );

        let fmt_res = mcp_call(
            &mut session,
            &mut backend,
            6,
            "norn.fmt",
            &serde_json::json!({ "path": "src/unformatted.ts", "check": true }),
        );
        assert_eq!(
            fmt_res.pointer("/result/isError"),
            Some(&serde_json::json!(true))
        );
        assert_eq!(
            std::fs::read_to_string(ws.0.join("src/unformatted.ts")).unwrap(),
            "export const item={a:1,b:2};\n",
            "MCP norn.fmt must remain read-only"
        );
    }

    #[test]
    fn mcp_refuses_parent_traversal_and_escaping_symlink_paths() {
        let ws = ScratchWorkspace::new("escape-refusal");
        let mut session = catalog_session_for(&ws.0).unwrap().stateless();
        let mut backend = Human::with_workspace(ws.0.clone());

        let escape_res = mcp_call(
            &mut session,
            &mut backend,
            7,
            "norn.check",
            &serde_json::json!({
                "source": "../escape.ts",
                "org": "org_ops",
                "env": "dev"
            }),
        );
        assert_eq!(
            escape_res.pointer("/result/isError"),
            Some(&serde_json::json!(true))
        );
        assert!(
            escape_res
                .pointer("/result/structuredContent/explanation")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|text| text.contains("working directory"))
        );

        #[cfg(unix)]
        {
            let outside = ScratchWorkspace::new("outside-symlink");
            let secret = outside.write("secret.ts", "export const x = 1\n");
            std::fs::create_dir_all(ws.0.join("src")).unwrap();
            let link = ws.0.join("src/escape_link.ts");
            std::os::unix::fs::symlink(&secret, &link).unwrap();
            let sym_res = mcp_call(
                &mut session,
                &mut backend,
                8,
                "norn.check",
                &serde_json::json!({
                    "source": "src/escape_link.ts",
                    "org": "org_ops",
                    "env": "dev"
                }),
            );
            assert_eq!(
                sym_res.pointer("/result/isError"),
                Some(&serde_json::json!(true))
            );
            assert!(
                sym_res
                    .pointer("/result/structuredContent/explanation")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|text| text.contains("working directory"))
            );
        }
    }

    #[test]
    fn mcp_explain_preserves_positional_order_for_kind_and_id() {
        let input = RuntimeObject::from([
            (
                "id".into(),
                RuntimeValue::String("core.laws.preservation_violated".into()),
            ),
            ("kind".into(), RuntimeValue::String("diagnostic".into())),
        ]);
        let argv = crate::mcp_tools::build_argv("explain", input.clone()).unwrap();
        assert_eq!(
            argv,
            vec![
                "norn".to_owned(),
                "explain".to_owned(),
                "--".to_owned(),
                "diagnostic".to_owned(),
                "core.laws.preservation_violated".to_owned(),
            ]
        );

        let ws = ScratchWorkspace::new("explain-positional-order");
        let output = crate::mcp_tools::call_tool("norn.explain", input, &ws.0).unwrap();
        assert!(!output.is_error, "norn.explain failed: {:?}", output.value);
        let text = output
            .value
            .as_object()
            .and_then(|obj| obj.get("text"))
            .and_then(RuntimeValue::as_str)
            .unwrap();
        assert!(
            text.contains("Diagnostic core.laws.preservation_violated"),
            "expected diagnostic text output, got: {text}"
        );
    }

    #[test]
    fn mcp_check_source_stops_norn_json_walk_at_workspace_root() {
        let parent = ScratchWorkspace::new("confinement-parent");
        parent.write(
            "norn.json",
            r#"{"format":"norn-project/1","name":"outside","sources":["workspace/src"],"scope":{"orgId":"org_outside","environmentId":"prod"}}"#,
        );
        parent.write(".github/workflows/ci.yml", "name: outside-ci\non: push\n");
        let workspace_root = parent.0.join("workspace");
        std::fs::create_dir_all(&workspace_root).unwrap();
        parent.write(
            "workspace/src/model.ts",
            r#"import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Ticket = Norn.type({
  name: "ops.Ticket",
  title: "Ticket",
  version: "0.1.0",
  properties: Schema.Struct({ ticket_id: Schema.String }),
  identity: [{ name: "primary", properties: ["ticket_id"] }],
})
"#,
        );

        // Without --org/--env, confined MCP must NOT read parent/norn.json above workspace_root.
        let without_scope = crate::mcp_tools::call_tool(
            "norn.check",
            RuntimeObject::from([("source".into(), RuntimeValue::String("src/model.ts".into()))]),
            &workspace_root,
        );
        let refusal = without_scope.map(|_| ()).unwrap_err();
        assert!(
            refusal.explanation.contains("--org") && refusal.explanation.contains("--env"),
            "expected missing-scope refusal without reading parent norn.json, got: {refusal:?}"
        );

        // With explicit --org/--env, confined MCP admits without tripping parent zero-CI or outside tool_root.
        let with_scope = crate::mcp_tools::call_tool(
            "norn.check",
            RuntimeObject::from([
                ("source".into(), RuntimeValue::String("src/model.ts".into())),
                ("org".into(), RuntimeValue::String("org_ws".into())),
                ("env".into(), RuntimeValue::String("dev".into())),
            ]),
            &workspace_root,
        )
        .unwrap();
        assert!(
            !with_scope.is_error,
            "confined check with explicit scope failed: {:?}",
            with_scope.value
        );
        assert_eq!(
            with_scope
                .value
                .as_object()
                .and_then(|obj| obj.get("admitted")),
            Some(&RuntimeValue::Bool(true))
        );
    }
}
