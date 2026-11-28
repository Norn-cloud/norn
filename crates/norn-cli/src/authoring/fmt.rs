//! Native Rust `norn fmt [--check] <path>` for `TypeScript` and Python authoring sources.
//!
//! Reads every target directly from a trusted FD-pinned descriptor under an
//! [`InvocationBudget`] and atomically writes modified files through their
//! verified parent directory fd (never reopening raw pathnames).

use std::path::{Path, PathBuf};

use serde_json::json;

use super::fs_io::{
    EntryKind, InvocationBudget, SourceLang, classify_source_file, display_posix_path,
    inspect_path_no_follow, is_formattable_file,
};
use super::trusted_dir::{TrustedDir, open_regular_file_pinned};
use super::{CommandOutput, FmtArgs};

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

fn execute_fmt_single_file(args: &FmtArgs, budget: &mut InvocationBudget) -> CommandOutput {
    let Some(lang) = classify_source_file(&args.path) else {
        return CommandOutput::usage_or_io_error(format!(
            "{}: expected a .ts, .tsx, or .py authoring file",
            args.path.display()
        ));
    };
    let (parent_dir, file_name, before) = match open_regular_file_pinned(&args.path, budget) {
        Ok(loaded) => loaded,
        Err(err) => return CommandOutput::usage_or_io_error(err),
    };
    let after = match format_one(&args.path, lang, &before) {
        Ok(formatted) => formatted,
        Err(diag_json) => return CommandOutput::refused(diag_json),
    };
    if after == before {
        return CommandOutput::ok(String::new());
    }
    let shown = display_posix_path(&args.path);
    if let Err(err) = budget.charge_output_bytes(&shown, after.len()) {
        return CommandOutput::usage_or_io_error(err);
    }
    if args.check {
        return CommandOutput::refused(shown);
    }
    if let Err(err) = parent_dir.write_rel_file_atomic(Path::new(&file_name), &args.path, &after) {
        return CommandOutput::usage_or_io_error(err);
    }
    CommandOutput::ok(shown)
}

fn execute_fmt_directory(args: &FmtArgs, budget: &mut InvocationBudget) -> CommandOutput {
    let root_dir = match TrustedDir::open(&args.path) {
        Ok(dir) => dir,
        Err(err) => return CommandOutput::usage_or_io_error(err),
    };
    let files = match root_dir.walk_files(is_formattable_file, budget) {
        Ok(list) => list,
        Err(err) => return CommandOutput::usage_or_io_error(err),
    };

    let mut pending: Vec<(PathBuf, PathBuf, String)> = Vec::new();
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
        pending.push((loaded.rel_path, loaded.display_path, after));
    }

    if !args.check {
        for (rel_path, display_path, after) in &pending {
            if let Err(err) = root_dir.write_rel_file_atomic(rel_path, display_path, after) {
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
    match inspect_path_no_follow(&args.path) {
        Ok(EntryKind::File) => execute_fmt_single_file(args, &mut budget),
        Ok(EntryKind::Directory) => execute_fmt_directory(args, &mut budget),
        Err(err) => CommandOutput::usage_or_io_error(err),
    }
}
