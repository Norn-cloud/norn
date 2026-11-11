//! Print the Frappe projection bundle of an artifact set.
//!
//! ```sh
//! cargo run -p norn-projections --example frappe_bundle -- \
//!   fixtures/demo/explorer/three-systems.definitions.json \
//!   --types canon.Item,canon.Party,core.analytics.Restatement \
//!   --readers owner,admin,member,viewer > demo.bundle.json
//! ```
//!
//! `--readers` are the host's membership roles that may read, strongest
//! first. The output is the bundle the bridge stages with
//! `bench --site S execute norn_bridge.sync.stage --args '["demo.bundle.json"]'`.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a command-line example"
)]

#[path = "../tests/support/frappe_defs.rs"]
mod frappe_defs;

use std::process::ExitCode;

use norn_projections::{FrappeProjectionInput, project_frappe_bundle};

fn list(args: &[String], flag: &str) -> Vec<String> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|index| args.get(index.saturating_add(1)))
        .map(|value| value.split(',').map(str::to_owned).collect())
        .unwrap_or_default()
}

fn run() -> Result<String, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args
        .first()
        .ok_or("usage: frappe_bundle <definitions.json> --types a,b --readers r1,r2")?;
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let defs = frappe_defs::decode(&text)?;
    let types = list(&args, "--types");
    let readers = list(&args, "--readers");
    let selected = types
        .iter()
        .map(|name| {
            defs.object_types
                .iter()
                .find(|def| def.name.as_str() == name)
                .ok_or_else(|| format!("{name}: no such object type in {path}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let actions: Vec<_> = defs.actions.iter().collect();
    let access: Vec<_> = defs.access.iter().collect();
    let reader_roles: Vec<&str> = readers.iter().map(String::as_str).collect();
    let bundle = project_frappe_bundle(&FrappeProjectionInput {
        object_types: &selected,
        actions: &actions,
        access: &access,
        reader_roles: &reader_roles,
    })
    .map_err(|e| e.to_string())?;
    let canonical = norn_ir::canonical::canonical_json_of(&bundle).map_err(|e| e.to_string())?;
    let value: serde_json::Value = serde_json::from_slice(&canonical).map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&value).map_err(|e| e.to_string())
}

fn main() -> ExitCode {
    match run() {
        Ok(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("frappe_bundle: {error}");
            ExitCode::FAILURE
        }
    }
}
