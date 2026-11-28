use std::sync::Arc;

use tsrs_ast::{ExternalModuleIndicatorOptions, SourceFileParseOptions};
use tsrs_compiler::{ProgramOptions, ProgramRegions, new_compiler_host, new_program};
use tsrs_core::arena::Region;
use tsrs_core::context::Context;
use tsrs_core::tspath::{self, ComparePathsOptions};
use tsrs_core::{
    CompilerOptions, P, TextChange, Tristate, ensure_script_kind_from_file_name,
};
use tsrs_ls::autoimport::{self, ProjectID};
use tsrs_ls::format::api::{FormatContext, format_document, with_format_code_settings};
use tsrs_ls::host::Host as LsHost;
use tsrs_ls::lsconv::{
    Converters, compute_lsp_line_starts, file_name_to_document_uri, new_converters,
};
use tsrs_ls::lsutil::{
    EditorSettings, FormatCodeSettings, IndentStyle, SemicolonPreference, UserPreferences,
    get_default_format_code_settings, new_default_user_preferences,
};
use tsrs_ls::new_language_service;
use tsrs_ls::sourcemap::ECMALineInfo;
use tsrs_lsproto::{
    CodeActionContext, CodeActionKind, CodeActionParams, PositionEncodingKind,
    TextDocumentIdentifier, TextEdit,
};
use tsrs_parser::parse_source_file_owned;
use tsrs_tsoptions::new_parsed_command_line;
use tsrs_vfs::{FS, bundled};

use crate::{AuthoringError, BoundedVfs, VfsLimits, default_compiler_options, normalize_virtual_path};

/// Returns the canonical Norn TypeScript [`FormatCodeSettings`] matching
/// `FORMAT_SETTINGS` in `tools/norn-ts/src/fmt.ts`.
#[must_use]
pub fn norn_format_settings() -> FormatCodeSettings {
    let mut settings = get_default_format_code_settings();
    settings.editor_settings = EditorSettings {
        base_indent_size: 0,
        indent_size: 2,
        tab_size: 2,
        new_line_character: "\n".to_owned(),
        convert_tabs_to_spaces: Tristate::True,
        indent_style: IndentStyle::SMART,
        trim_trailing_whitespace: Tristate::True,
    };
    settings.semicolons = SemicolonPreference::Remove;
    settings.insert_space_after_comma_delimiter = Tristate::True;
    settings.insert_space_after_semicolon_in_for_statements = Tristate::True;
    settings.insert_space_before_and_after_binary_operators = Tristate::True;
    settings.insert_space_after_keywords_in_control_flow_statements = Tristate::True;
    settings.insert_space_after_opening_and_before_closing_nonempty_braces = Tristate::True;
    settings.insert_space_after_opening_and_before_closing_nonempty_brackets = Tristate::False;
    settings.insert_space_after_opening_and_before_closing_nonempty_parenthesis = Tristate::False;
    settings.place_open_brace_on_new_line_for_functions = Tristate::False;
    settings.place_open_brace_on_new_line_for_control_blocks = Tristate::False;
    settings
}

struct FormatLsHost {
    file_name: String,
    text: String,
    converters: Arc<Converters>,
    preferences: UserPreferences,
}

impl FormatLsHost {
    fn new(file_name: String, text: String) -> Self {
        let line_map = compute_lsp_line_starts(&text);
        let key = file_name.clone();
        let converters = new_converters(PositionEncodingKind::UTF8, move |candidate| {
            if candidate == key {
                Some(Arc::clone(&line_map))
            } else {
                None
            }
        });
        let mut preferences = new_default_user_preferences();
        preferences.format_code_settings = norn_format_settings();
        Self {
            file_name,
            text,
            converters,
            preferences,
        }
    }
}

impl LsHost for FormatLsHost {
    fn use_case_sensitive_file_names(&self) -> bool {
        true
    }

    fn read_file(&self, path: &str) -> Option<String> {
        if path == self.file_name {
            Some(self.text.clone())
        } else {
            None
        }
    }

    fn converters(&self) -> Arc<Converters> {
        Arc::clone(&self.converters)
    }

    fn get_preferences(&self, _active_file: &str) -> UserPreferences {
        self.preferences.clone()
    }

    fn get_ecma_line_info(&self, _file_name: &str) -> Option<Arc<ECMALineInfo>> {
        None
    }

    fn auto_import_registry(&self) -> Option<Arc<autoimport::Registry>> {
        None
    }

    fn read_directory(
        &self,
        _current_dir: &str,
        _path: &str,
        _extensions: &[String],
        _excludes: &[String],
        _includes: &[String],
        _depth: usize,
    ) -> Vec<String> {
        Vec::new()
    }

    fn get_directories(&self, _path: &str) -> Vec<String> {
        Vec::new()
    }

    fn directory_exists(&self, _path: &str) -> bool {
        false
    }

    fn file_exists(&self, path: &str) -> bool {
        path == self.file_name
    }
}

/// Applies non-overlapping byte-offset edits safely in reverse start order.
fn apply_byte_edits(
    source: &str,
    mut edits: Vec<(usize, usize, String)>,
) -> Result<String, AuthoringError> {
    edits.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| right.1.cmp(&left.1)));
    let mut current = source.to_owned();
    for (start, end, replacement) in edits {
        let len = current.len();
        if start > end
            || end > len
            || !current.is_char_boundary(start)
            || !current.is_char_boundary(end)
        {
            return Err(AuthoringError::InvalidEditRange { start, end, len });
        }
        let Some(head) = current.get(..start) else {
            return Err(AuthoringError::InvalidEditRange { start, end, len });
        };
        let Some(tail) = current.get(end..) else {
            return Err(AuthoringError::InvalidEditRange { start, end, len });
        };
        let mut next = String::with_capacity(
            head.len()
                .saturating_add(replacement.len())
                .saturating_add(tail.len()),
        );
        next.push_str(head);
        next.push_str(&replacement);
        next.push_str(tail);
        current = next;
    }
    Ok(current)
}

fn apply_text_changes(source: &str, changes: Vec<TextChange>) -> Result<String, AuthoringError> {
    let mut edits = Vec::with_capacity(changes.len());
    for change in changes {
        let Some(start_u32) = change.text_range.source_pos() else {
            return Err(AuthoringError::FormatFailed(
                "formatter emitted a synthetic start position".to_owned(),
            ));
        };
        let Some(end_u32) = change.text_range.source_end() else {
            return Err(AuthoringError::FormatFailed(
                "formatter emitted a synthetic end position".to_owned(),
            ));
        };
        let start = usize::try_from(start_u32).map_err(|err| {
            AuthoringError::FormatFailed(format!("edit start offset overflow: {err}"))
        })?;
        let end = usize::try_from(end_u32).map_err(|err| {
            AuthoringError::FormatFailed(format!("edit end offset overflow: {err}"))
        })?;
        edits.push((start, end, change.new_text));
    }
    apply_byte_edits(source, edits)
}

fn sort_imports_non_destructive(file_name: &str, text: &str) -> Result<String, AuthoringError> {
    let limits = VfsLimits {
        max_files: 4,
        max_file_bytes: text.len().max(1_024),
        max_total_bytes: text.len().saturating_add(1_024),
    };
    let mut vfs = BoundedVfs::without_intrinsics(limits);
    let normalized = vfs.add_file(file_name, text)?;
    let fs_dyn: Arc<dyn FS> = Arc::new(vfs);
    let compiler_host = new_compiler_host("/", fs_dyn, &bundled::lib_path(), None, None, None);

    let config_region = Region::new_scratch(32_usize.saturating_mul(1_024));
    let parsed_command_line = {
        let _scope = config_region.enter();
        let mut options: CompilerOptions = default_compiler_options();
        options.no_lib = Tristate::True;
        options.lib = None;
        let options_ptr = P::new(options);
        let compare_paths = ComparePathsOptions {
            use_case_sensitive_file_names: true,
            current_directory: "/".to_owned(),
        };
        P::new(new_parsed_command_line(
            options_ptr,
            vec![normalized.clone()],
            Vec::new(),
            compare_paths,
        ))
    };

    let regions = Arc::new(ProgramRegions::default());
    let mut program_options = ProgramOptions::new(parsed_command_line, compiler_host);
    program_options.single_threaded = Tristate::True;
    program_options.skip_module_resolution = true;
    program_options.checker_regions = true;
    program_options.regions = Some(Arc::clone(&regions));

    let program = new_program(program_options);
    let Some(source_file) = program.get_source_file(&normalized) else {
        return Err(AuthoringError::SourceFileNotFound(normalized));
    };

    let ls_host = Arc::new(FormatLsHost::new(normalized.clone(), text.to_owned()));
    let converters = ls_host.converters();
    let ls_host_dyn: Arc<dyn LsHost> = Arc::clone(&ls_host);
    let language_service = new_language_service(
        ProjectID("norn-fmt".to_owned()),
        program,
        ls_host_dyn,
        &normalized,
    );

    let doc_uri = file_name_to_document_uri(&normalized);
    let mut context = CodeActionContext::default();
    context.only = Some(vec![CodeActionKind::SOURCE_SORT_IMPORTS_TS]);
    let mut params = CodeActionParams::default();
    params.text_document = TextDocumentIdentifier {
        uri: doc_uri.clone(),
    };
    params.context = context;

    let response = language_service
        .provide_code_actions(&Context::default(), &params)
        .map_err(|err| AuthoringError::FormatFailed(format!("sort imports failed: {err:?}")))?;

    let mut lsp_edits: Vec<TextEdit> = Vec::new();
    if let Some(actions) = response.command_or_code_action_array {
        for item in actions {
            let Some(action) = item.code_action else {
                continue;
            };
            let Some(edit) = action.edit else {
                continue;
            };
            let Some(changes) = edit.changes else {
                continue;
            };
            for (uri, file_edits) in changes {
                if uri == doc_uri {
                    lsp_edits.extend(file_edits);
                }
            }
        }
    }

    if lsp_edits.is_empty() {
        return Ok(text.to_owned());
    }

    let mut byte_edits = Vec::with_capacity(lsp_edits.len());
    for edit in lsp_edits {
        let start_u32 = converters.line_and_character_to_position(&source_file, edit.range.start);
        let end_u32 = converters.line_and_character_to_position(&source_file, edit.range.end);
        let start = usize::try_from(start_u32).map_err(|err| {
            AuthoringError::FormatFailed(format!("LSP edit start offset overflow: {err}"))
        })?;
        let end = usize::try_from(end_u32).map_err(|err| {
            AuthoringError::FormatFailed(format!("LSP edit end offset overflow: {err}"))
        })?;
        byte_edits.push((start, end, edit.new_text));
    }

    apply_byte_edits(text, byte_edits)
}

fn format_document_pass(file_name: &str, text: &str) -> Result<String, AuthoringError> {
    let region = Region::new_scratch(64_usize.saturating_mul(1_024));
    let changes = {
        let _scope = region.enter();
        let path = tspath::to_path(file_name, "/", true);
        let opts = SourceFileParseOptions {
            file_name: file_name.to_owned(),
            path,
            external_module_indicator_options: ExternalModuleIndicatorOptions::default(),
        };
        let script_kind = ensure_script_kind_from_file_name(file_name);
        let source_file = parse_source_file_owned(opts, text.to_owned(), script_kind);
        let format_ctx =
            with_format_code_settings(&FormatContext::default(), norn_format_settings(), "\n");
        format_document(&format_ctx, source_file)
    };
    apply_text_changes(text, changes)
}

/// Formats a Norn TypeScript source file in memory without executing code.
///
/// Upholds `FMT-SEMANTIC-1`:
/// - Never executes authored source.
/// - Sorts and coalesces imports using `SOURCE_SORT_IMPORTS_TS` without
///   removing unused imports.
/// - Applies canonical Norn formatting settings (2-space indent, semicolons
///   removed, brace and operator spacing).
/// - Normalizes `CRLF` to `LF` and emits a single trailing newline.
///
/// # Errors
///
/// Returns [`AuthoringError::InvalidPath`] if `file_name` is not a valid
/// virtual path, or [`AuthoringError::FormatFailed`] /
/// [`AuthoringError::InvalidEditRange`] if formatting fails.
pub fn format_source(file_name: &str, text: &str) -> Result<String, AuthoringError> {
    let normalized_file = normalize_virtual_path(file_name)?;
    let sorted = sort_imports_non_destructive(&normalized_file, text)?;
    let formatted = format_document_pass(&normalized_file, &sorted)?;
    let lf = formatted.replace("\r\n", "\n");
    let trimmed = lf.trim_end_matches('\n');
    let mut out = String::with_capacity(trimmed.len().saturating_add(1));
    out.push_str(trimmed);
    out.push('\n');
    Ok(out)
}
