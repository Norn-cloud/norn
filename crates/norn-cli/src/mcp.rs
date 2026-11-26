//! Stdio adapter over the human command catalog; unavailable backends are absent.
use super::{Cli, Command, run_check};
use clap::{CommandFactory as _, Parser as _};
use norn_api::{
    mcp::{Backend, CallOutput, Session},
    refusal::Refusal,
};
use norn_ir::{McpTool, McpToolAnnotations, RuntimeObject, RuntimeValue};
use std::io::{BufRead as _, Read as _};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const MAX_MCP_LINE_BYTES: usize = 1024 * 1024;
const MCP_DISCARD_CHUNK_BYTES: u64 = 4096;

fn platform_tools() -> Result<Vec<McpTool>, String> {
    static TOOLS: OnceLock<Result<Vec<McpTool>, String>> = OnceLock::new();
    TOOLS
        .get_or_init(|| {
            let command = Cli::command();
            ["capabilities", "check"]
                .into_iter()
                .map(|name| {
                    let operation = command
                        .find_subcommand(name)
                        .ok_or_else(|| format!("missing {name} command"))?;
                    command_tool(operation)
                })
                .collect()
        })
        .clone()
}
fn command_tool(operation: &clap::Command) -> Result<McpTool, String> {
    // The platform catalog is generated from the human command's argument IR.
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for arg in operation
        .get_arguments()
        .filter(|arg| arg.get_long().is_some() && arg.get_action().takes_values())
    {
        let name = arg.get_id().as_str();
        properties.insert(name.into(), serde_json::json!({"type":"string", "description":arg.get_help().map_or_default(ToString::to_string)}));
        if arg.is_required_set() {
            required.push(name);
        }
    }
    serde_json::from_value(serde_json::json!({
        "name":format!("norn.{}",operation.get_name()),
        "title":operation.get_about().map(ToString::to_string),
        "description":operation.get_long_about().or_else(|| operation.get_about()).map(ToString::to_string),
        "inputSchema":{"type":"object","additionalProperties":false,"properties":properties,"required":required},
        "annotations":McpToolAnnotations { title:None, read_only_hint:Some(true), destructive_hint:Some(false), idempotent_hint:Some(true), open_world_hint:Some(false) }
    })).map_err(|e|e.to_string())
}
fn is_valid_flag_key(key: &str) -> bool {
    !key.is_empty()
        && !key.starts_with('-')
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn confine_check_path(path: &Path, workspace_root: &Path) -> Result<PathBuf, Refusal> {
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_root.join(path)
    };
    let meta = std::fs::symlink_metadata(&candidate).map_err(|_error| {
        Refusal::invalid("check path is unreadable or outside the current working directory")
    })?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(Refusal::invalid(
            "check paths must be regular files within the current working directory",
        ));
    }
    let canonical_root = std::fs::canonicalize(workspace_root).map_err(Refusal::host)?;
    let canonical_path = std::fs::canonicalize(&candidate).map_err(|_error| {
        Refusal::invalid("check path is unreadable or outside the current working directory")
    })?;
    if !canonical_path.starts_with(&canonical_root) {
        return Err(Refusal::invalid(
            "check paths must stay within the current working directory",
        ));
    }
    Ok(canonical_path)
}

struct Human;
impl Backend for Human {
    fn call(
        &mut self,
        name: &str,
        arguments: RuntimeObject,
        _metadata: RuntimeObject,
    ) -> Result<CallOutput, Refusal> {
        let operation = name
            .strip_prefix("norn.")
            .ok_or_else(|| Refusal::invalid("unknown command"))?;
        let mut argv = vec!["norn".to_owned(), operation.to_owned()];
        for (key, value) in arguments {
            if !is_valid_flag_key(&key) {
                return Err(Refusal::invalid(
                    "command argument names must match the advertised input schema",
                ));
            }
            let RuntimeValue::String(value) = value else {
                return Err(Refusal::invalid(
                    "command arguments must be strings; use the advertised input schema",
                ));
            };
            argv.push(format!("--{key}={value}"));
        }
        let parsed =
            Cli::try_parse_from(argv).map_err(|error| Refusal::invalid(error.to_string()))?;
        let (artifacts, context) = match parsed.command {
            Command::Capabilities { cursor } => {
                return capabilities_catalog(cursor.as_deref()).map(|value| CallOutput {
                    value,
                    is_error: false,
                });
            }
            Command::Check { artifacts, context } => (artifacts, context),
            Command::Mcp
            | Command::Remote(_)
            | Command::Cell(_)
            | Command::Source(_)
            | Command::Eval { .. }
            | Command::Openapi
            | Command::SourceDigest { .. }
            | Command::Explain(_) => {
                return Err(Refusal::invalid("backend unavailable"));
            }
        };
        let workspace_root = std::env::current_dir().map_err(Refusal::host)?;
        let artifacts = confine_check_path(&artifacts, &workspace_root)?;
        let context = confine_check_path(&context, &workspace_root)?;
        let (value, accepted) = run_check(&artifacts, &context).map_err(|error| {
            Refusal::invalid(format!(
                "norn {operation} failed: {error}. Repair the named arguments or files and retry."
            ))
        })?;
        Ok(CallOutput {
            value: RuntimeValue::from_json_text(&value).map_err(Refusal::host)?,
            is_error: !accepted,
        })
    }
}
fn catalog_session() -> Result<Session, Refusal> {
    let binding = std::env::current_dir()
        .map_err(Refusal::host)?
        .to_string_lossy()
        .into_owned();
    Session::new(platform_tools().map_err(Refusal::host)?, &binding)
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
    let mut backend = Human;
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    serve_io(&mut session, &mut backend, &mut input, &mut output)?;
    Ok((String::new(), true))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "human/MCP correspondence assertions")]
mod tests {
    use super::{Human, capabilities, capabilities_catalog, catalog_session};
    use norn_api::mcp::Backend as _;
    use norn_ir::{RuntimeObject, RuntimeValue};

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
            let output = Human
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
        let projected = Human
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
        let injected = Human
            .call(
                "norn.capabilities",
                RuntimeObject::from([("cursor".into(), RuntimeValue::String("--help".into()))]),
                RuntimeObject::new(),
            )
            .map(|_| ())
            .unwrap_err();
        assert!(injected.explanation.contains("restart"));

        let escaped = Human
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
        let mut backend = Human;
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
}
