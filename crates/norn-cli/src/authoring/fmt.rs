//! Native Rust `norn fmt [--check] <path>` for `TypeScript` and Python authoring sources.

use std::path::{Path, PathBuf};

use serde_json::json;

use super::fs_io::{
    EntryKind, SourceLang, classify_source_file, collect_walk_files, display_posix_path,
    inspect_path_no_follow, is_formattable_file, read_source_file_bounded, write_file_atomic,
};
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

fn collect_fmt_targets(path: &Path) -> Result<Vec<PathBuf>, String> {
    match inspect_path_no_follow(path)? {
        EntryKind::File => {
            if classify_source_file(path).is_none() {
                return Err(format!(
                    "{}: expected a .ts, .tsx, or .py authoring file",
                    path.display()
                ));
            }
            Ok(vec![path.to_path_buf()])
        }
        EntryKind::Directory => collect_walk_files(path, is_formattable_file),
    }
}

/// Executes `norn fmt [--check] <path>`.
pub(crate) fn execute_fmt(args: &FmtArgs) -> CommandOutput {
    let files = match collect_fmt_targets(&args.path) {
        Ok(list) => list,
        Err(err) => return CommandOutput::usage_or_io_error(err),
    };

    let mut changed = Vec::new();
    for file in files {
        let Some(lang) = classify_source_file(&file) else {
            continue;
        };
        let before = match read_source_file_bounded(&file) {
            Ok(text) => text,
            Err(err) => return CommandOutput::usage_or_io_error(err),
        };
        let after = match format_one(&file, lang, &before) {
            Ok(formatted) => formatted,
            Err(diag_json) => return CommandOutput::refused(diag_json),
        };
        if after == before {
            continue;
        }
        if !args.check
            && let Err(err) = write_file_atomic(&file, &after)
        {
            return CommandOutput::usage_or_io_error(err);
        }
        changed.push(display_posix_path(&file));
    }

    let listing = changed.join("\n");
    if args.check && !changed.is_empty() {
        CommandOutput::refused(listing)
    } else {
        CommandOutput::ok(listing)
    }
}
