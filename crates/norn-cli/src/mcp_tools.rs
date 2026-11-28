//! Read-only MCP tool schema projection, path confinement, and command dispatch.

use std::path::{Component, Path, PathBuf};

use clap::{Arg, ArgAction, CommandFactory as _, Parser as _};
use norn_api::mcp::CallOutput;
use norn_api::refusal::Refusal;
use norn_ir::{McpTool, McpToolAnnotations, RuntimeObject, RuntimeValue};
use serde_json::{Map, Value, json};

use crate::authoring::CommandOutput;
use crate::{Cli, Command};

/// Read-only human CLI subcommands exposed through `norn mcp`.
pub(crate) const PLATFORM_TOOLS: &[&str] = &["capabilities", "check", "compile", "explain", "fmt"];

fn is_exposed_arg(operation_name: &str, arg: &Arg) -> bool {
    let id = arg.get_id().as_str();
    if id == "help" || id == "version" {
        return false;
    }
    if operation_name == "compile" && id == "out" {
        return false;
    }
    arg.is_positional() || arg.get_long().is_some()
}

fn arg_schema(arg: &Arg) -> Value {
    let description = arg.get_help().map_or_else(String::new, ToString::to_string);
    if !arg.get_action().takes_values() {
        return json!({
            "type": "boolean",
            "description": description,
        });
    }
    if matches!(arg.get_action(), ArgAction::Append) {
        return json!({
            "type": "array",
            "items": { "type": "string" },
            "description": description,
        });
    }
    let values: Vec<String> = arg
        .get_possible_values()
        .iter()
        .map(|val| val.get_name().to_owned())
        .collect();
    let mut schema = Map::new();
    schema.insert("type".to_owned(), json!("string"));
    schema.insert("description".to_owned(), json!(description));
    if !values.is_empty() {
        schema.insert("enum".to_owned(), json!(values));
    }
    Value::Object(schema)
}

/// Projects one clap subcommand into an [`McpTool`] schema.
pub(crate) fn command_tool(operation: &clap::Command) -> Result<McpTool, String> {
    let op_name = operation.get_name();
    let mut properties = Map::new();
    let mut required = Vec::new();
    for arg in operation
        .get_arguments()
        .filter(|arg| is_exposed_arg(op_name, arg))
    {
        let name = arg.get_id().as_str();
        properties.insert(name.to_owned(), arg_schema(arg));
        if arg.is_required_set() {
            required.push(name);
        }
    }
    serde_json::from_value(json!({
        "name": format!("norn.{op_name}"),
        "title": operation.get_about().map(ToString::to_string),
        "description": operation
            .get_long_about()
            .or_else(|| operation.get_about())
            .map(ToString::to_string),
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "properties": properties,
            "required": required,
        },
        "annotations": McpToolAnnotations {
            title: None,
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(false),
        },
    }))
    .map_err(|error| error.to_string())
}

fn is_valid_arg_key(key: &str) -> bool {
    !key.is_empty()
        && !key.starts_with('-')
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// Confines a regular file or directory path strictly inside `workspace_root` without following symlinks.
pub(crate) fn confine_path(path: &Path, workspace_root: &Path) -> Result<PathBuf, Refusal> {
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_root.join(path)
    };
    let canonical_root = std::fs::canonicalize(workspace_root).map_err(Refusal::host)?;
    if let Ok(rel) = candidate.strip_prefix(workspace_root) {
        verify_relative_no_symlink(rel, &canonical_root)?;
    }
    let meta = std::fs::symlink_metadata(&candidate).map_err(|_error| {
        Refusal::invalid("path is unreadable or outside the current working directory")
    })?;
    if meta.file_type().is_symlink() {
        return Err(Refusal::invalid(
            "symlinks are refused; paths must stay within the current working directory",
        ));
    }
    if !meta.is_file() && !meta.is_dir() {
        return Err(Refusal::invalid(
            "paths must be regular files or directories within the current working directory",
        ));
    }
    let canonical_path = std::fs::canonicalize(&candidate).map_err(|_error| {
        Refusal::invalid("path is unreadable or outside the current working directory")
    })?;
    if !canonical_path.starts_with(&canonical_root) {
        return Err(Refusal::invalid(
            "paths must stay within the current working directory",
        ));
    }
    Ok(canonical_path)
}

fn verify_relative_no_symlink(rel: &Path, canonical_root: &Path) -> Result<(), Refusal> {
    let mut cursor = canonical_root.to_path_buf();
    for component in rel.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if cursor == canonical_root || !cursor.pop() {
                    return Err(Refusal::invalid(
                        "paths must stay within the current working directory",
                    ));
                }
            }
            Component::Normal(seg) => {
                cursor.push(seg);
                let meta = std::fs::symlink_metadata(&cursor).map_err(|_error| {
                    Refusal::invalid("path is unreadable or outside the current working directory")
                })?;
                if meta.file_type().is_symlink() {
                    return Err(Refusal::invalid(
                        "symlinks are refused; paths must stay within the current working directory",
                    ));
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(Refusal::invalid(
                    "paths must stay within the current working directory",
                ));
            }
        }
    }
    Ok(())
}

fn find_exposed_arg<'a>(subcmd: &'a clap::Command, op_name: &str, key: &str) -> Option<&'a Arg> {
    subcmd
        .get_arguments()
        .filter(|arg| is_exposed_arg(op_name, arg))
        .find(|arg| arg.get_id().as_str() == key || arg.get_long() == Some(key))
}

fn append_flag_argument(
    op_name: &str,
    arg: &Arg,
    value: RuntimeValue,
    argv: &mut Vec<String>,
) -> Result<(), Refusal> {
    let long = arg
        .get_long()
        .ok_or_else(|| Refusal::invalid("unsupported flag argument"))?;
    if !arg.get_action().takes_values() {
        let RuntimeValue::Bool(enabled) = value else {
            return Err(Refusal::invalid(
                "boolean flags require a boolean value; use the advertised input schema",
            ));
        };
        if op_name == "fmt" && arg.get_id().as_str() == "check" && !enabled {
            return Err(Refusal::invalid(
                "MCP norn.fmt is read-only; check must be true or omitted",
            ));
        }
        if enabled {
            argv.push(format!("--{long}"));
        }
        return Ok(());
    }
    if matches!(arg.get_action(), ArgAction::Append) {
        match value {
            RuntimeValue::Array(items) => {
                for item in items {
                    let RuntimeValue::String(text) = item else {
                        return Err(Refusal::invalid(
                            "repeatable arguments require an array of strings",
                        ));
                    };
                    argv.push(format!("--{long}={text}"));
                }
                return Ok(());
            }
            RuntimeValue::String(text) => {
                argv.push(format!("--{long}={text}"));
                return Ok(());
            }
            RuntimeValue::Null
            | RuntimeValue::Bool(_)
            | RuntimeValue::Number(_)
            | RuntimeValue::Object(_) => {
                return Err(Refusal::invalid(
                    "repeatable arguments require an array of strings",
                ));
            }
        }
    }
    let text = match value {
        RuntimeValue::String(text) => text,
        RuntimeValue::Number(num) => num.to_string(),
        RuntimeValue::Null
        | RuntimeValue::Bool(_)
        | RuntimeValue::Array(_)
        | RuntimeValue::Object(_) => {
            return Err(Refusal::invalid(
                "command arguments must match the advertised input schema",
            ));
        }
    };
    argv.push(format!("--{long}={text}"));
    Ok(())
}

pub(crate) fn build_argv(
    operation: &str,
    arguments: RuntimeObject,
) -> Result<Vec<String>, Refusal> {
    if !PLATFORM_TOOLS.contains(&operation) {
        return Err(Refusal::invalid("backend unavailable"));
    }
    let mut root = Cli::command();
    root.build();
    let subcmd = root
        .find_subcommand(operation)
        .ok_or_else(|| Refusal::invalid("unknown command"))?;
    let mut argv = vec!["norn".to_owned(), operation.to_owned()];
    let mut positionals: Vec<(usize, String)> = Vec::new();
    for (key, value) in arguments {
        if !is_valid_arg_key(&key) {
            return Err(Refusal::invalid(
                "command argument names must match the advertised input schema",
            ));
        }
        let Some(arg) = find_exposed_arg(subcmd, operation, &key) else {
            return Err(Refusal::invalid(
                "command argument names must match the advertised input schema",
            ));
        };
        if arg.is_positional() {
            let RuntimeValue::String(text) = value else {
                return Err(Refusal::invalid(
                    "positional arguments must be strings; use the advertised input schema",
                ));
            };
            positionals.push((arg.get_index().unwrap_or(0), text));
        } else {
            append_flag_argument(operation, arg, value, &mut argv)?;
        }
    }
    if !positionals.is_empty() {
        positionals.sort_by_key(|(idx, _)| *idx);
        argv.push("--".to_owned());
        for (_, text) in positionals {
            argv.push(text);
        }
    }
    Ok(argv)
}

fn json_object_output(raw: &str, is_error: bool, array_key: Option<&str>) -> CallOutput {
    if let Ok(parsed) = RuntimeValue::from_json_text(raw) {
        return match parsed {
            RuntimeValue::Object(_) => CallOutput {
                value: parsed,
                is_error,
            },
            RuntimeValue::Array(_) => {
                let field_name = array_key.unwrap_or("output");
                CallOutput {
                    value: RuntimeValue::Object(RuntimeObject::from([
                        ("ok".into(), RuntimeValue::Bool(!is_error)),
                        (field_name.into(), parsed),
                    ])),
                    is_error,
                }
            }
            RuntimeValue::Null
            | RuntimeValue::Bool(_)
            | RuntimeValue::Number(_)
            | RuntimeValue::String(_) => CallOutput {
                value: RuntimeValue::Object(RuntimeObject::from([
                    ("ok".into(), RuntimeValue::Bool(!is_error)),
                    ("output".into(), parsed),
                ])),
                is_error,
            },
        };
    }
    CallOutput {
        value: RuntimeValue::Object(RuntimeObject::from([
            ("ok".into(), RuntimeValue::Bool(!is_error)),
            ("text".into(), RuntimeValue::String(raw.to_owned())),
        ])),
        is_error,
    }
}

fn from_command_output(
    output: CommandOutput,
    array_key: Option<&str>,
) -> Result<CallOutput, Refusal> {
    if output.exit_code == 2 {
        return Err(Refusal::invalid(output.stderr));
    }
    let is_error = output.exit_code != 0;
    Ok(json_object_output(&output.stdout, is_error, array_key))
}

fn confine_opt_path(slot: &mut Option<PathBuf>, workspace_root: &Path) -> Result<(), Refusal> {
    if let Some(path) = slot.as_deref() {
        *slot = Some(confine_path(path, workspace_root)?);
    }
    Ok(())
}

fn run_mcp_fmt(
    mut fmt_args: crate::authoring::FmtArgs,
    workspace_root: &Path,
) -> Result<CallOutput, Refusal> {
    fmt_args.check = true;
    fmt_args.path = confine_path(&fmt_args.path, workspace_root)?;
    let output = crate::authoring::execute_fmt(&fmt_args);
    if output.exit_code == 2 {
        return Err(Refusal::invalid(output.stderr));
    }
    if let Ok(RuntimeValue::Object(obj)) = RuntimeValue::from_json_text(&output.stdout) {
        return Ok(CallOutput {
            value: RuntimeValue::Object(obj),
            is_error: output.exit_code != 0,
        });
    }
    let unformatted: Vec<RuntimeValue> = output
        .stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| RuntimeValue::String(line.to_owned()))
        .collect();
    let clean = output.exit_code == 0;
    Ok(CallOutput {
        value: RuntimeValue::Object(RuntimeObject::from([
            ("ok".into(), RuntimeValue::Bool(clean)),
            ("formatted".into(), RuntimeValue::Bool(clean)),
            ("unformattedFiles".into(), RuntimeValue::Array(unformatted)),
        ])),
        is_error: !clean,
    })
}

/// Executes an MCP tool call confined strictly to `workspace_root`.
pub(crate) fn call_tool(
    name: &str,
    arguments: RuntimeObject,
    workspace_root: &Path,
) -> Result<CallOutput, Refusal> {
    let operation = name
        .strip_prefix("norn.")
        .ok_or_else(|| Refusal::invalid("unknown command"))?;
    let argv = build_argv(operation, arguments)?;
    let parsed = Cli::try_parse_from(argv).map_err(|error| Refusal::invalid(error.to_string()))?;
    match parsed.command {
        Command::Capabilities { cursor } => {
            crate::mcp::capabilities_catalog_for(cursor.as_deref(), workspace_root).map(|value| {
                CallOutput {
                    value,
                    is_error: false,
                }
            })
        }
        Command::Check(mut check_args) => {
            confine_opt_path(&mut check_args.artifacts, workspace_root)?;
            confine_opt_path(&mut check_args.context, workspace_root)?;
            confine_opt_path(&mut check_args.source, workspace_root)?;
            confine_opt_path(&mut check_args.tool_root, workspace_root)?;
            let output = crate::run_check_args_confined(&check_args, workspace_root);
            from_command_output(output, None)
        }
        Command::Compile(mut compile_args) => {
            if compile_args.out.is_some() {
                return Err(Refusal::invalid(
                    "MCP norn.compile is read-only and refuses --out",
                ));
            }
            compile_args.path = confine_path(&compile_args.path, workspace_root)?;
            confine_opt_path(&mut compile_args.tool_root, workspace_root)?;
            confine_opt_path(&mut compile_args.root, workspace_root)?;
            confine_opt_path(&mut compile_args.modules, workspace_root)?;
            let output = crate::authoring::execute_compile_confined(&compile_args, workspace_root);
            from_command_output(output, Some("artifacts"))
        }
        Command::Explain(mut explain_args) => {
            confine_opt_path(&mut explain_args.defs, workspace_root)?;
            confine_opt_path(&mut explain_args.context, workspace_root)?;
            confine_opt_path(&mut explain_args.wording, workspace_root)?;
            for base_path in &mut explain_args.base {
                *base_path = confine_path(base_path, workspace_root)?;
            }
            let (text, admitted) = crate::explain::execute(&explain_args).map_err(|error| {
                Refusal::invalid(format!(
                    "norn explain failed: {error}. Repair the named arguments or files and retry."
                ))
            })?;
            Ok(json_object_output(&text, !admitted, None))
        }
        Command::Fmt(fmt_args) => run_mcp_fmt(fmt_args, workspace_root),
        Command::Mcp
        | Command::Remote(_)
        | Command::Cell(_)
        | Command::Source(_)
        | Command::Profile(_)
        | Command::Question(_)
        | Command::Dashboard(_)
        | Command::Eval { .. }
        | Command::Openapi
        | Command::SourceDigest { .. }
        | Command::Version { .. } => Err(Refusal::invalid("backend unavailable")),
    }
}
