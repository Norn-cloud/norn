//! Exact public-core provenance and trusted policy gates.
use clap::{Parser, Subcommand};
use norn_skeleton_guard::ci;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;
#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Top,
}
#[derive(Subcommand)]
enum Top {
    #[command(subcommand)]
    Ci(Gate),
}
#[derive(Subcommand)]
enum Gate {
    /// Exact candidate or expanded-distribution provenance.
    Provenance {
        #[arg(long, default_value = ".")]
        root: PathBuf,
        #[arg(long)]
        distribution: bool,
        #[arg(long)]
        base_root: Option<PathBuf>,
    },
    /// This checkout's policy.
    Policy,
    /// Base-owned policy checks the candidate as data, without running it.
    Trusted {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        base_root: PathBuf,
    },
}
fn main() -> ExitCode {
    let Top::Ci(command) = Cli::parse().command;
    let (document, passed) = match command {
        Gate::Provenance {
            root,
            distribution,
            base_root,
        } => {
            let report = match (distribution, base_root) {
                (true, Some(base)) => ci::provenance::check_distribution_against(&root, &base),
                (true, None) => ci::provenance::check_distribution(&root),
                (false, Some(base)) => ci::provenance::check_against(&root, &base),
                (false, None) => ci::provenance::check(&root),
            };
            (
                serde_json::to_string_pretty(&report),
                report.errors().is_empty(),
            )
        }
        Gate::Policy => {
            let root = PathBuf::from(".");
            let errors = ci::run(&root, &root);
            (serde_json::to_string_pretty(&errors), errors.is_empty())
        }
        Gate::Trusted { root, base_root } => {
            let errors = ci::run(&root, &base_root);
            (serde_json::to_string_pretty(&errors), errors.is_empty())
        }
    };
    match document {
        Ok(document) if writeln!(std::io::stdout().lock(), "{document}").is_ok() && passed => {
            ExitCode::SUCCESS
        }
        _ => ExitCode::FAILURE,
    }
}
