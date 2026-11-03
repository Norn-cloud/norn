//! Stdio adapter over the human command catalog; unavailable backends are absent.
use super::{Cli, Command, run};
use clap::{CommandFactory as _, Parser as _};
use norn_api::{
    mcp::{Backend, CallOutput, Session},
    refusal::Refusal,
};
use norn_ir::{McpTool, McpToolAnnotations, RuntimeObject, RuntimeValue};
use std::io::{BufRead as _, Read as _, Write as _};

fn check_tool() -> Result<McpTool, String> {
    let command = Cli::command();
    let check = command
        .find_subcommand("check")
        .ok_or("missing check command")?;
    // The platform catalog is generated from the human command's argument IR.
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for arg in check
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
        "name":format!("norn.{}",check.get_name()), "title":"Check compiled definitions",
        "description":format!("{}. Read artifacts and context from local files, exactly as norn check does. Author ordinary typed source, compile it with the authoring CLI, then check its output. A refused report retains diagnostic codes, source locations, remediation and receipt/profile pins. Edit the named source and compile/check again; checking never installs definitions.",check.get_about().map(ToString::to_string).unwrap_or_default()),
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
                return Err(Refusal::invalid("file arguments must be strings"));
            };
            argv.push(format!("--{key}"));
            argv.push(value);
        }
        let parsed =
            Cli::try_parse_from(argv).map_err(|error| Refusal::invalid(error.to_string()))?;
        if !matches!(parsed.command, Command::Check { .. }) {
            return Err(Refusal::invalid("backend unavailable"));
        }
        let (value, accepted) = run(parsed.command).map_err(|error| {
            Refusal::invalid(format!(
                "norn check failed: {error}. Repair the artifacts/context files and retry."
            ))
        })?;
        Ok(CallOutput {
            value: RuntimeValue::from_json_text(&value).map_err(Refusal::host)?,
            is_error: !accepted,
        })
    }
}
pub(super) fn serve() -> Result<(String, bool), String> {
    let binding = std::env::current_dir()
        .map_err(|e| e.to_string())?
        .to_string_lossy()
        .into_owned();
    let mut session = Session::new(vec![check_tool()?], &binding).map_err(|e| e.message)?;
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
