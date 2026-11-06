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
        /// Immutable candidate commit. Required for bound mode.
        #[arg(long, required_unless_present = "legacy_distribution")]
        head: Option<String>,
        /// Git checkout supplying independently expected archive bytes.
        #[arg(long, requires = "head")]
        source_root: Option<PathBuf>,
        #[arg(
            long,
            requires = "head",
            required_unless_present = "legacy_distribution"
        )]
        repository: Option<String>,
        /// Explicit standalone legacy distribution, with strict stored hashes.
        #[arg(long, conflicts_with = "head", requires = "distribution")]
        legacy_distribution: bool,
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
        /// API evidence of a maintainer approval of the exact candidate head.
        #[arg(long)]
        maintainer_evidence: Option<PathBuf>,
        #[arg(long)]
        rights_evidence: PathBuf,
        #[arg(long)]
        head: String,
        #[arg(long)]
        repository: String,
    },
}
fn main() -> ExitCode {
    let Top::Ci(command) = Cli::parse().command;
    let (document, passed) = match command {
        Gate::Provenance {
            root,
            distribution,
            head,
            source_root,
            repository,
            legacy_distribution,
            base_root,
        } => {
            let report = if legacy_distribution {
                match base_root {
                    Some(base) => ci::provenance::check_distribution_against(&root, &base),
                    None => ci::provenance::check_distribution(&root),
                }
            } else {
                let (Some(head), Some(repository)) = (head, repository) else {
                    return ExitCode::FAILURE;
                };
                if distribution != source_root.is_some() {
                    return ExitCode::FAILURE;
                }
                let base = base_root.as_deref().unwrap_or(&root);
                ci::provenance::check_bound(
                    &root,
                    &ci::provenance::BoundSource {
                        source: source_root.as_deref().unwrap_or(&root),
                        head: &head,
                        base,
                        repository: &repository,
                    },
                    distribution,
                )
            };
            (
                serde_json::to_string_pretty(&report),
                report.errors().is_empty(),
            )
        }
        Gate::Policy => {
            let root = PathBuf::from(".");
            let errors = ci::run_local(&root);
            (serde_json::to_string_pretty(&errors), errors.is_empty())
        }
        Gate::Trusted {
            root,
            base_root,
            maintainer_evidence,
            rights_evidence,
            head,
            repository,
        } => {
            let report = ci::check_with_rights(
                &root,
                &base_root,
                &head,
                &repository,
                maintainer_evidence.as_deref(),
                &rights_evidence,
            );
            (
                serde_json::to_string_pretty(&report),
                report.errors.is_empty(),
            )
        }
    };
    match document {
        Ok(document) if writeln!(std::io::stdout().lock(), "{document}").is_ok() && passed => {
            ExitCode::SUCCESS
        }
        _ => ExitCode::FAILURE,
    }
}
