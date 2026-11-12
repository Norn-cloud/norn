//! Stdio adapter over the human command catalog; unavailable backends are absent.
use super::{Cli, Command, run};
use clap::{CommandFactory as _, Parser as _};
use norn_api::{
    mcp::{Backend, CallOutput, Session},
    refusal::Refusal,
};
use norn_ir::{McpTool, McpToolAnnotations, RuntimeObject, RuntimeValue};
use std::io::{BufRead as _, Read as _, Write as _};

fn platform_tools() -> Result<Vec<McpTool>, String> {
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
        properties.insert(name.into(), serde_json::json!({"type":"string", "description":arg.get_help().map(ToString::to_string).unwrap_or_default()}));
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
            let RuntimeValue::String(value) = value else {
                return Err(Refusal::invalid(
                    "command arguments must be strings; use the advertised input schema",
                ));
            };
            argv.push(format!("--{key}"));
            argv.push(value);
        }
        let parsed =
            Cli::try_parse_from(argv).map_err(|error| Refusal::invalid(error.to_string()))?;
        let command = match parsed.command {
            Command::Capabilities { cursor } => {
                return capabilities_catalog(cursor.as_deref()).map(|value| CallOutput {
                    value,
                    is_error: false,
                });
            }
            command @ Command::Check { .. } => command,
            Command::Mcp
            | Command::Remote(_)
            | Command::Port(_)
            | Command::Source(_)
            | Command::Eval { .. }
            | Command::Openapi
            | Command::SourceDigest { .. } => {
                return Err(Refusal::invalid("backend unavailable"));
            }
        };
        let (value, accepted) = run(command).map_err(|error| {
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
pub(super) fn serve() -> Result<(String, bool), String> {
    let mut session = catalog_session().map_err(|e| e.explanation)?;
    let mut backend = Human;
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    loop {
        let mut line = Vec::new();
        let bytes = input
            .by_ref()
            .take(1024 * 1024 + 1)
            .read_until(b'\n', &mut line)
            .map_err(|e| e.to_string())?;
        if bytes == 0 {
            break;
        }
        if bytes > 1024 * 1024 {
            return Err("MCP message exceeds 1 MiB".into());
        }
        let source = std::str::from_utf8(&line).map_err(|e| e.to_string())?;
        if let Some(response) = session
            .handle(source, &mut backend)
            .map_err(|e| e.message)?
        {
            writeln!(output, "{response}").map_err(|e| e.to_string())?;
            output.flush().map_err(|e| e.to_string())?;
        }
    }
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
    fn port_is_absent_from_the_catalog_and_refused_when_called() {
        // `norn port` starts a process; the MCP surface must never reach it.
        let names: Vec<String> = super::platform_tools()
            .unwrap()
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        assert_eq!(names, ["norn.capabilities", "norn.check"]);
        for arguments in [
            RuntimeObject::new(),
            RuntimeObject::from([("tool".into(), RuntimeValue::String("selftest".into()))]),
        ] {
            assert!(
                Human
                    .call("norn.port", arguments, RuntimeObject::new())
                    .is_err()
            );
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
}
