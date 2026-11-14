//! The CLI's commands and flags as a frozen, additive-only contract (D84).
//!
//! `cli.contract.json` is generated from the clap definition and checked here;
//! `scripts/ci/contract_diff.py` fails a change that removes or narrows
//! anything in it. `NORN_BLESS=1` rewrites it; review the diff.
use std::collections::BTreeMap;

use clap::{ArgAction, CommandFactory as _};
use serde_json::{Value, json};

const SNAPSHOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/cli.contract.json");

fn arg(arg: &clap::Arg) -> Value {
    let mut aliases: Vec<String> = arg
        .get_all_aliases()
        .unwrap_or_default()
        .into_iter()
        .map(str::to_owned)
        .collect();
    aliases.sort();
    let values: Vec<String> = arg
        .get_possible_values()
        .iter()
        .map(|value| value.get_name().to_owned())
        .collect();
    let defaults: Vec<String> = arg
        .get_default_values()
        .iter()
        .map(|value| value.to_string_lossy().into_owned())
        .collect();
    json!({
        "long": arg.get_long(),
        "short": arg.get_short().map(String::from),
        "aliases": aliases,
        "positional": arg.is_positional(),
        "takes_value": arg.get_action().takes_values(),
        "multiple": matches!(arg.get_action(), ArgAction::Append | ArgAction::Count)
            || arg.get_num_args().is_some_and(|range| range.max_values() > 1),
        "required": arg.is_required_set(),
        "default": defaults,
        "values": values,
    })
}

fn command(cmd: &clap::Command) -> Value {
    let args: BTreeMap<String, Value> = cmd
        .get_arguments()
        .map(|a| (a.get_id().to_string(), arg(a)))
        .collect();
    let subcommands: BTreeMap<String, Value> = cmd
        .get_subcommands()
        .map(|c| (c.get_name().to_owned(), command(c)))
        .collect();
    json!({ "args": args, "subcommands": subcommands })
}

/// The contract document of the `norn` CLI.
fn contract() -> String {
    let mut document = command(&super::Cli::command());
    if let Some(object) = document.as_object_mut() {
        object.insert("format".to_owned(), json!("norn-cli-contract/1"));
    }
    let mut text = serde_json::to_string_pretty(&document).unwrap_or_default();
    text.push('\n');
    text
}

#[test]
fn the_cli_contract_matches_its_snapshot() {
    let current = contract();
    if std::env::var("NORN_BLESS").as_deref() == Ok("1") {
        std::fs::write(SNAPSHOT, &current).unwrap_or_else(|error| panic!("{SNAPSHOT}: {error}"));
        return;
    }
    let recorded = std::fs::read_to_string(SNAPSHOT).unwrap_or_default();
    assert!(
        recorded == current,
        "cli.contract.json differs from the clap definition; rerun with NORN_BLESS=1 and review the diff"
    );
}
