//! Native Rust `norn fmt [--check] <path>` for `TypeScript` and Python authoring sources.
//!
//! Reads every target directly from a trusted FD-pinned descriptor under an
//! [`InvocationBudget`] and atomically writes modified files directly through
//! the retained parent directory descriptor (never reopening pathnames).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::json;

use super::atomic_io::write_file_atomic_in_trusted_dir;
use super::fs_io::{
    InvocationBudget, SourceLang, classify_source_file, display_posix_path, is_formattable_file,
    read_opened_file_budgeted,
};
use super::trusted_dir::{PinnedSourceTarget, TrustedDir, open_source_target_pinned};
use super::{CommandOutput, FmtArgs};

struct PendingFmtWrite {
    parent: Arc<TrustedDir>,
    file_name: String,
    display_path: PathBuf,
    formatted: String,
}

fn format_one(file: &Path, lang: SourceLang, source: &str) -> Result<String, String> {
    match lang {
        SourceLang::TypeScript => {
            let virtual_name = format!(
                "/{}",
                file.file_name()
                    .and_then(std::ffi::OsStr::to_str)
                    .unwrap_or("module.ts")
            );
            norn_authoring::format_ts(&virtual_name, source).map_err(|err| {
                json!({
                    "ok": false,
                    "diagnostics": [{
                        "code": "NORN-FMT-TS",
                        "file": display_posix_path(file),
                        "message": err.to_string(),
                    }],
                })
                .to_string()
            })
        }
        SourceLang::Python => norn_authoring_py::format_python_source(source).map_err(|err| {
            let diag = match err.span() {
                Some(span) => json!({
                    "code": err.code(),
                    "file": display_posix_path(file),
                    "line": span.line,
                    "column": span.column,
                    "message": err.message(),
                }),
                None => json!({
                    "code": err.code(),
                    "file": display_posix_path(file),
                    "message": err.message(),
                }),
            };
            json!({
                "ok": false,
                "diagnostics": [diag],
            })
            .to_string()
        }),
    }
}

fn execute_fmt_single_file(
    args: &FmtArgs,
    parent_dir: &TrustedDir,
    file_name: &str,
    display_path: &Path,
    opened: std::fs::File,
    budget: &mut InvocationBudget,
) -> CommandOutput {
    let Some(lang) = classify_source_file(display_path) else {
        return CommandOutput::usage_or_io_error(format!(
            "{}: expected a .ts, .tsx, or .py authoring file",
            display_path.display()
        ));
    };
    if let Err(err) = budget.charge_accepted_file(display_path) {
        return CommandOutput::usage_or_io_error(err);
    }
    let before = match read_opened_file_budgeted(opened, display_path, budget) {
        Ok(content) => content,
        Err(err) => return CommandOutput::usage_or_io_error(err),
    };
    let after = match format_one(display_path, lang, &before) {
        Ok(formatted) => formatted,
        Err(diag_json) => return CommandOutput::refused(diag_json),
    };
    if after == before {
        return CommandOutput::ok(String::new());
    }
    let shown = display_posix_path(display_path);
    if let Err(err) = budget.charge_output_bytes(&shown, after.len()) {
        return CommandOutput::usage_or_io_error(err);
    }
    if args.check {
        return CommandOutput::refused(shown);
    }
    if let Err(err) = write_file_atomic_in_trusted_dir(parent_dir, file_name, display_path, &after)
    {
        return CommandOutput::usage_or_io_error(err);
    }
    CommandOutput::ok(shown)
}

fn execute_fmt_directory(
    args: &FmtArgs,
    root_dir: TrustedDir,
    budget: &mut InvocationBudget,
) -> CommandOutput {
    let root_shared = Arc::new(root_dir);
    let files = match root_shared.walk_files_pinned(is_formattable_file, budget) {
        Ok(list) => list,
        Err(err) => return CommandOutput::usage_or_io_error(err),
    };

    let mut pending: Vec<PendingFmtWrite> = Vec::new();
    let mut changed = Vec::new();
    for loaded in files {
        let Some(lang) = classify_source_file(&loaded.display_path) else {
            continue;
        };
        let after = match format_one(&loaded.display_path, lang, &loaded.content) {
            Ok(formatted) => formatted,
            Err(diag_json) => return CommandOutput::refused(diag_json),
        };
        if after == loaded.content {
            continue;
        }
        let shown = display_posix_path(&loaded.display_path);
        if let Err(err) = budget.charge_output_bytes(&shown, after.len()) {
            return CommandOutput::usage_or_io_error(err);
        }
        changed.push(shown);
        pending.push(PendingFmtWrite {
            parent: loaded.parent,
            file_name: loaded.file_name,
            display_path: loaded.display_path,
            formatted: after,
        });
    }

    if !args.check {
        for item in &pending {
            if let Err(err) = write_file_atomic_in_trusted_dir(
                &item.parent,
                &item.file_name,
                &item.display_path,
                &item.formatted,
            ) {
                return CommandOutput::usage_or_io_error(err);
            }
        }
    }

    let listing = changed.join("\n");
    if args.check && !changed.is_empty() {
        CommandOutput::refused(listing)
    } else {
        CommandOutput::ok(listing)
    }
}

/// Executes `norn fmt [--check] <path>`.
pub(crate) fn execute_fmt(args: &FmtArgs) -> CommandOutput {
    let mut budget = InvocationBudget::new();
    match open_source_target_pinned(&args.path) {
        Ok(PinnedSourceTarget::File {
            parent_dir,
            file_name,
            display_path,
            opened,
        }) => execute_fmt_single_file(
            args,
            &parent_dir,
            &file_name,
            &display_path,
            opened,
            &mut budget,
        ),
        Ok(PinnedSourceTarget::Directory(root_dir)) => {
            execute_fmt_directory(args, root_dir, &mut budget)
        }
        Err(err) => CommandOutput::usage_or_io_error(err),
    }
}
