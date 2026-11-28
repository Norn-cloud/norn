//! Local embedded cell runtime subcommands (`norn test` and `norn dev`).
//!
//! Both commands boot the real `norn-api` [`LocalCell`] over `DevHost<NativeEngine>`
//! with deterministic [`ManualClock`](norn_api::clock::ManualClock), fail-closed scope and
//! role authorization, and real checker + kernel admission gates.

mod dev_server;
mod http_wire;
mod pack;
mod scenario;
mod scenario_assert;
mod scenario_parse;
#[cfg(test)]
mod tests;

use std::fmt::Write as _;
use std::path::Path;

use norn_api::local_cell::{DEFAULT_LOCAL_CLOCK_MS, LocalCell};
use norn_api::refusal::Refusal;
use norn_api::scope::CellScope;

use crate::authoring::CommandOutput;

pub(crate) use dev_server::{DevArgs, execute_dev};
pub(crate) use scenario::{TestArgs, execute_test};

use self::pack::{PackInput, PackLoadError, load_combined_packs};

/// Startup failure distinguishing refusal/diagnostics (exit `1`) from usage/IO (exit `2`).
pub(crate) enum BootFailure {
    Refused(String),
    Io(String),
}

impl BootFailure {
    pub(crate) fn into_output(self) -> CommandOutput {
        match self {
            Self::Refused(doc) => CommandOutput::refused(doc),
            Self::Io(message) => CommandOutput::usage_or_io_error(&message),
        }
    }
}

/// Boot a [`LocalCell`] (in-memory or SQLite-backed) and install any base + app
/// packs through the real admission and `apply_definitions` gates.
pub(crate) fn boot_local_cell(
    org: &str,
    env: &str,
    clock_ms: Option<u64>,
    db_path: Option<&Path>,
    base_paths: &[std::path::PathBuf],
    app_pack: Option<&PackInput>,
) -> Result<LocalCell, BootFailure> {
    if org.trim().is_empty() || env.trim().is_empty() {
        return Err(BootFailure::Io(
            "--org and --env must be non-empty scope identifiers".to_owned(),
        ));
    }
    let artifacts =
        load_combined_packs(base_paths, app_pack, org, env).map_err(|err| match err {
            PackLoadError::Diagnostics(doc) => BootFailure::Refused(doc),
            PackLoadError::Io(msg) => BootFailure::Io(msg),
        })?;
    let scope = CellScope {
        org_id: org.to_owned(),
        environment_id: env.to_owned(),
    };
    let start_ms = clock_ms.unwrap_or(DEFAULT_LOCAL_CLOCK_MS);
    let opened = match db_path {
        Some(path) => LocalCell::open_file(path, scope, start_ms, &artifacts),
        None => LocalCell::open_in_memory(scope, start_ms, &artifacts),
    };
    opened.map_err(|refusal| BootFailure::Refused(format_refusal(&refusal)))
}

/// Render a [`Refusal`] including all diagnostics and structured counterexample witnesses.
pub(crate) fn format_refusal(refusal: &Refusal) -> String {
    let mut out = format!(
        "refused ({}) {}: {}\n  {}\n",
        refusal.status, refusal.code, refusal.message, refusal.explanation
    );
    for diagnostic in &refusal.diagnostics {
        let at = diagnostic
            .span
            .as_ref()
            .map_or_default(|span| format!(" at {}", span.pointer));
        let _written = writeln!(
            out,
            "  {} {}{at}: {}",
            diagnostic.severity, diagnostic.code, diagnostic.message
        );
        if let Some(witness) = &diagnostic.witness {
            let _written = writeln!(
                out,
                "    witness: {}",
                crate::cell_render::format_witness(witness)
            );
        }
    }
    out.trim_end().to_owned()
}
