//! `norn-cell-restore`: the operator path for a cell its open refuses
//! (`cell.unopenable`, #1423).
//!
//! - `norn-cell-restore diagnose <cell.sqlite>` reads the file and prints how
//!   far its history is proven and the first ledger entry the store's own
//!   value proofs refuse.
//! - `norn-cell-restore restore <cell.sqlite> <target.sqlite> <staging-dir>
//!   [--through N]` rebuilds the proven prefix of the cell's own export
//!   journal into the absent target, through the typed replay a restore uses.
//!   A prefix shorter than the journal drops commits, so it needs
//!   `--through N` with N exactly the diagnosed `provenThrough`; a fully
//!   proven journal restores whole without it.
//!
//! The source is opened read-only. Run either on a copy of the cell's file
//! taken with the cell not serving, together with its `-wal` and `-shm`
//! sidecars when present (a WAL tail left behind is a cut no check can see),
//! and keep the printed report with the incident.

use std::io::Write as _;
use std::path::Path;
use std::process::ExitCode;

use norn_cell::{CellStore, HistoryDiagnosis, StoreOptions};
use serde_json::{Value, json};

const USAGE: &str = "usage: norn-cell-restore diagnose <cell.sqlite>\n       norn-cell-restore restore <cell.sqlite> <target.sqlite> <staging-dir> [--through N]";

fn diagnosis(found: &HistoryDiagnosis) -> Value {
    json!({
        "headCommit": found.head_commit,
        "journalCommits": found.journal_commits,
        "provenThrough": found.proven_through,
        "firstUnproven": found.first_unproven.as_ref().map(|entry| json!({
            "seq": entry.seq, "commitSeq": entry.commit_seq, "op": entry.op, "reason": entry.reason,
        })),
    })
}

fn run(args: &[String]) -> Result<Value, String> {
    let options = StoreOptions::default();
    match args {
        [command, source] if command == "diagnose" => {
            CellStore::diagnose_history(Path::new(source))
                .map(|found| diagnosis(&found))
                .map_err(|error| error.to_string())
        }
        [command, source, target, staging, rest @ ..] if command == "restore" => {
            let through = match rest {
                [] => None,
                [flag, cut] if flag == "--through" => Some(
                    cut.parse::<u64>()
                        .map_err(|error| format!("--through {cut}: {error}"))?,
                ),
                _ => return Err(USAGE.to_owned()),
            };
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| error.to_string())?;
            let restored = runtime
                .block_on(CellStore::restore_proven_prefix(
                    Path::new(source),
                    Path::new(target),
                    Path::new(staging),
                    through,
                    options,
                ))
                .map_err(|error| error.to_string())?;
            Ok(json!({
                "diagnosis": diagnosis(&restored.diagnosis),
                "restored": {
                    "commits": restored.report.ledger_head.commits,
                    "ledgerSeq": restored.report.ledger_head.seq,
                    "ledgerHash": restored.report.ledger_head.hash,
                    "exportHead": restored.report.manifest.head_hash,
                    "fencedCommands": restored.report.fenced_commands,
                },
            }))
        }
        _ => Err(USAGE.to_owned()),
    }
}

fn main() -> ExitCode {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let (text, code) = match run(&args) {
        Ok(report) => (report.to_string(), ExitCode::SUCCESS),
        Err(error) => (error, ExitCode::FAILURE),
    };
    let written = if code == ExitCode::SUCCESS {
        writeln!(std::io::stdout().lock(), "{text}")
    } else {
        writeln!(std::io::stderr().lock(), "{text}")
    };
    if written.is_err() {
        return ExitCode::FAILURE;
    }
    code
}
