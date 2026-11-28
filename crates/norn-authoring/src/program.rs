use std::sync::Arc;

use tsrs_ast::{Diagnostic, Kind, Node, SourceFile, SymbolFlags, get_source_file_of_node};
use tsrs_compiler::{Program, ProgramOptions, ProgramRegions, new_compiler_host, new_program};
use tsrs_core::arena::Region;
use tsrs_core::context::Context;
use tsrs_core::tspath::ComparePathsOptions;
use tsrs_core::{CompilerOptions, ModuleKind, ModuleResolutionKind, P, ScriptTarget, Tristate};
use tsrs_diagnostics::Category;
use tsrs_scanner::get_ecma_line_and_byte_offset_of_position;
use tsrs_tsoptions::new_parsed_command_line;
use tsrs_vfs::{FS, bundled};

use crate::{AuthoringError, BoundedVfs, INTRINSICS_VIRTUAL_PATH, Profile, normalize_virtual_path};

/// Diagnostic severity reported by [`AuthoringProgram`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticCategory {
    /// TypeScript compiler warning.
    Warning,
    /// TypeScript compiler error.
    Error,
    /// Non-blocking suggestion diagnostic.
    Suggestion,
    /// Informational message.
    Message,
}

impl From<Category> for DiagnosticCategory {
    fn from(category: Category) -> Self {
        match category {
            Category::Warning => Self::Warning,
            Category::Error => Self::Error,
            Category::Suggestion => Self::Suggestion,
            Category::Message => Self::Message,
        }
    }
}

/// Owned summary of a `tsrs` syntactic or semantic diagnostic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticSummary {
    /// Numeric TypeScript diagnostic code (`TS<code>`).
    pub code: i32,
    /// Diagnostic category.
    pub category: DiagnosticCategory,
    /// Virtual file path where the diagnostic was reported, if tied to a file.
    pub file: Option<String>,
    /// 1-based line number, if tied to a source span.
    pub line: Option<u32>,
    /// 1-based `UTF-8` byte column offset, if tied to a source span.
    pub column: Option<u32>,
    /// Localized diagnostic message text.
    pub message: String,
}

/// Canonical identity of a resolved TypeScript symbol.
///
/// Matches the `norn-effect` profile identity contract:
/// `<package>/<declaring .d.ts without extension>#<qualified name>` or
/// `<intrinsicsModule>#<qualified name>`, computed after following import and
/// re-export aliases via `tsrs_checker`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolIdentity {
    /// Canonical identity string (for example `@norn/authoring#Norn.fn`).
    pub id: String,
    /// Declaring virtual file path.
    pub declaring_file: String,
    /// Qualified declared name without the leading quoted module path.
    pub qualified_name: String,
    /// Whether the target symbol is a value or namespace module.
    pub is_namespace: bool,
}

/// Returns the pinned `tsrs` [`CompilerOptions`] matching `tools/norn-ts/src/check.ts`.
#[must_use]
pub fn default_compiler_options() -> CompilerOptions {
    CompilerOptions {
        strict: Tristate::True,
        target: ScriptTarget::ES2022,
        module: ModuleKind::ES_NEXT,
        module_resolution: ModuleResolutionKind::BUNDLER,
        lib: Some(vec![
            "lib.esnext.d.ts".to_owned(),
            "lib.dom.d.ts".to_owned(),
        ]),
        types: Some(Vec::new()),
        skip_lib_check: Tristate::True,
        no_emit: Tristate::True,
        allow_importing_ts_extensions: Tristate::True,
        single_threaded: Tristate::True,
        ..CompilerOptions::default()
    }
}

/// Strip a leading `"..."` module-path prefix emitted by `getFullyQualifiedName`.
///
/// Mirrors `/^"[^"]*"(?:\.(.*))?$/` in `tools/norn-ts/src/check.ts`.
fn strip_quoted_module_prefix(fqn: &str) -> &str {
    let Some(after_open) = fqn.strip_prefix('"') else {
        return fqn;
    };
    let Some((_, after_close)) = after_open.split_once('"') else {
        return fqn;
    };
    if after_close.is_empty() {
        return "";
    }
    after_close.strip_prefix('.').unwrap_or(fqn)
}

/// Maps a declaring virtual file path to its profile module name ONLY when it
/// lives inside a pinned profile origin (the embedded `@norn/authoring`
/// declarations or the explicitly configured pinned `effect` declaration root).
///
/// Matches `resolveSymbol` in `tools/norn-ts/src/check.ts`: lookalike copies
/// under arbitrary `node_modules/` directories never receive a profile ID.
fn resolve_profile_module(
    declaring_file: &str,
    intrinsics_module: &str,
    effect_dist_prefix: Option<&str>,
) -> Option<String> {
    if declaring_file == INTRINSICS_VIRTUAL_PATH {
        return Some(intrinsics_module.to_owned());
    }
    if let Some(prefix) = effect_dist_prefix
        && let Some(rel) = declaring_file.strip_prefix(prefix)
    {
        let stripped = rel.strip_suffix(".d.ts").unwrap_or(rel);
        return Some(format!("effect/{stripped}"));
    }
    None
}

fn summarize_diagnostic(diag: P<Diagnostic>) -> DiagnosticSummary {
    let (file, line, column) = match diag.file() {
        Some(source_file) => {
            let file_name = source_file.file_name().to_owned();
            let (line, col) = match diag.pos() {
                Some(pos) => {
                    let (zero_line, zero_col) =
                        get_ecma_line_and_byte_offset_of_position(source_file.get(), pos);
                    let one_line = u32::try_from(zero_line)
                        .ok()
                        .and_then(|value| value.checked_add(1));
                    let one_col = zero_col.checked_add(1);
                    (one_line, one_col)
                }
                None => (None, None),
            };
            (Some(file_name), line, col)
        }
        None => (None, None, None),
    };
    DiagnosticSummary {
        code: diag.code(),
        category: DiagnosticCategory::from(diag.category()),
        file,
        line,
        column,
        message: diag.localize(),
    }
}

/// Compiled TypeScript authoring program backed by `tsrs` and [`BoundedVfs`].
///
/// The compiled ASTs, parsed source files, resolution data, and parsed command
/// line are allocated inside dedicated `tsrs` arena regions (`_regions` and
/// `_config_region`) and released when the program is dropped. Note that
/// `tsrs_compiler::free_program` / `free_unshared_program` are `unsafe fn` and
/// are intentionally not invoked because this workspace sets
/// `unsafe_code = "forbid"`; the inner `&'static Program` handle is kept
/// strictly private so no reference can outlive `self`.
pub struct AuthoringProgram {
    program: &'static Program,
    vfs: Arc<BoundedVfs>,
    profile: Profile,
    effect_dist_prefix: Option<String>,
    _regions: Arc<ProgramRegions>,
    _config_region: Region,
}

impl AuthoringProgram {
    /// Builds a new `tsrs` program over `vfs` with the given `root_files`.
    ///
    /// Automatically includes [`INTRINSICS_VIRTUAL_PATH`] as a root file so
    /// ambient `@norn/authoring` declarations are always available.
    ///
    /// # Errors
    ///
    /// Returns [`AuthoringError::VfsQuotaExceeded`] if `vfs` has breached its
    /// resource limits, or [`AuthoringError::InvalidPath`] if any root file
    /// path is invalid.
    pub fn new(
        vfs: BoundedVfs,
        root_files: &[&str],
        profile: Profile,
    ) -> Result<Self, AuthoringError> {
        if vfs.quota_breached() {
            return Err(AuthoringError::VfsQuotaExceeded(
                "cannot compile with a quota-breached BoundedVfs".to_owned(),
            ));
        }

        let mut roots = Vec::with_capacity(root_files.len().saturating_add(1));
        for &root in root_files {
            roots.push(normalize_virtual_path(root)?);
        }
        if !roots.iter().any(|path| path == INTRINSICS_VIRTUAL_PATH) {
            roots.push(INTRINSICS_VIRTUAL_PATH.to_owned());
        }

        let vfs_arc = Arc::new(vfs);
        let vfs_for_host = Arc::clone(&vfs_arc);
        let fs_dyn: Arc<dyn FS> = vfs_for_host;
        let host = new_compiler_host("/", fs_dyn, &bundled::lib_path(), None, None, None);

        let config_region = Region::new_scratch(64_usize.saturating_mul(1_024));
        let parsed_command_line = {
            let _scope = config_region.enter();
            let options_ptr = P::new(default_compiler_options());
            let compare_paths = ComparePathsOptions {
                use_case_sensitive_file_names: true,
                current_directory: "/".to_owned(),
            };
            P::new(new_parsed_command_line(
                options_ptr,
                roots,
                Vec::new(),
                compare_paths,
            ))
        };

        let regions = Arc::new(ProgramRegions::default());
        let mut program_options = ProgramOptions::new(parsed_command_line, host);
        program_options.single_threaded = Tristate::True;
        program_options.checker_regions = true;
        program_options.regions = Some(Arc::clone(&regions));

        let program = new_program(program_options);

        if vfs_arc.quota_breached() {
            return Err(AuthoringError::VfsQuotaExceeded(
                "virtual filesystem quota breached during program construction".to_owned(),
            ));
        }

        Ok(Self {
            program,
            vfs: vfs_arc,
            profile,
            effect_dist_prefix: None,
            _regions: regions,
            _config_region: config_region,
        })
    }

    /// Configures the pinned `effect` package `dist/` virtual path prefix used
    /// when computing `effect/<subpath>#<name>` profile identities.
    ///
    /// Declarations outside [`INTRINSICS_VIRTUAL_PATH`] and this exact pinned
    /// prefix never receive a profile [`SymbolIdentity`].
    #[must_use]
    pub fn with_effect_dist_prefix(mut self, prefix: impl Into<String>) -> Self {
        let mut prefix = prefix.into();
        if !prefix.starts_with('/') {
            prefix.insert(0, '/');
        }
        if !prefix.ends_with('/') {
            prefix.push('/');
        }
        self.effect_dist_prefix = Some(prefix);
        self
    }

    /// Returns the loaded authoring profile.
    #[must_use]
    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    /// Returns the backing [`BoundedVfs`].
    #[must_use]
    pub fn vfs(&self) -> &BoundedVfs {
        &self.vfs
    }

    /// Collects syntactic diagnostics for `file_name` (or all program files when `None`).
    ///
    /// # Errors
    ///
    /// Returns [`AuthoringError::InvalidPath`] or
    /// [`AuthoringError::SourceFileNotFound`] if `file_name` is invalid or not
    /// present in the program.
    pub fn syntactic_diagnostics(
        &self,
        file_name: Option<&str>,
    ) -> Result<Vec<DiagnosticSummary>, AuthoringError> {
        let target = self.resolve_optional_source_file(file_name)?;
        let ctx = Context::default();
        let diags = self.program.get_syntactic_diagnostics(&ctx, target);
        Ok(diags.into_iter().map(summarize_diagnostic).collect())
    }

    /// Collects semantic diagnostics for `file_name` (or all program files when `None`).
    ///
    /// # Errors
    ///
    /// Returns [`AuthoringError::InvalidPath`] or
    /// [`AuthoringError::SourceFileNotFound`] if `file_name` is invalid or not
    /// present in the program.
    pub fn semantic_diagnostics(
        &self,
        file_name: Option<&str>,
    ) -> Result<Vec<DiagnosticSummary>, AuthoringError> {
        let target = self.resolve_optional_source_file(file_name)?;
        let ctx = Context::default();
        let diags = self.program.get_semantic_diagnostics(&ctx, target);
        Ok(diags.into_iter().map(summarize_diagnostic).collect())
    }

    /// Collects all syntactic and semantic diagnostics for `file_name` (or all files when `None`).
    ///
    /// # Errors
    ///
    /// Returns [`AuthoringError::InvalidPath`] or
    /// [`AuthoringError::SourceFileNotFound`] if `file_name` is invalid or not
    /// present in the program.
    pub fn all_diagnostics(
        &self,
        file_name: Option<&str>,
    ) -> Result<Vec<DiagnosticSummary>, AuthoringError> {
        let mut diagnostics = self.syntactic_diagnostics(file_name)?;
        diagnostics.extend(self.semantic_diagnostics(file_name)?);
        Ok(diagnostics)
    }

    /// Resolves the canonical [`SymbolIdentity`] of `node` in `file_name`,
    /// following import and re-export aliases through `tsrs_checker`.
    ///
    /// # Errors
    ///
    /// Returns [`AuthoringError::InvalidPath`] or
    /// [`AuthoringError::SourceFileNotFound`] if `file_name` is not in the
    /// program.
    pub fn resolve_symbol_identity(
        &self,
        file_name: &str,
        node: P<Node>,
    ) -> Result<Option<SymbolIdentity>, AuthoringError> {
        let source_file = self.source_file(file_name)?;
        let ctx = Context::default();
        let mut checker = self
            .program
            .get_type_checker_for_file_exclusive(&ctx, source_file);
        Ok(self.resolve_symbol_with_checker(&mut checker, node))
    }

    /// Resolves the canonical symbol identity of each top-level `const x = <expr>`
    /// initializer in `file_name`.
    ///
    /// For call expressions `callee(...)` this resolves the callee target
    /// (`callee.name` for property access, or `callee` itself). For property
    /// accesses `obj.prop` this resolves `prop`.
    ///
    /// # Errors
    ///
    /// Returns [`AuthoringError::InvalidPath`] or
    /// [`AuthoringError::SourceFileNotFound`] if `file_name` is not in the
    /// program.
    pub fn resolve_initializer_identities(
        &self,
        file_name: &str,
    ) -> Result<Vec<(String, Option<SymbolIdentity>)>, AuthoringError> {
        let source_file = self.source_file(file_name)?;
        let ctx = Context::default();
        let mut checker = self
            .program
            .get_type_checker_for_file_exclusive(&ctx, source_file);
        let mut out = Vec::new();
        for &stmt in source_file.statements.nodes() {
            if stmt.kind() != Kind::VariableStatement {
                continue;
            }
            let decl_list = stmt
                .as_variable_statement()
                .declaration_list()
                .as_variable_declaration_list();
            for &decl in decl_list.declarations().nodes() {
                let var_decl = decl.as_variable_declaration();
                if var_decl.name().kind() != Kind::Identifier {
                    continue;
                }
                let Some(init) = var_decl.initializer() else {
                    continue;
                };
                let target_expr = if init.kind() == Kind::CallExpression {
                    init.as_call_expression().expression
                } else {
                    init
                };
                let target_node = if target_expr.kind() == Kind::PropertyAccessExpression {
                    target_expr.as_property_access_expression().name
                } else {
                    target_expr
                };
                let name = var_decl.name().text().to_owned();
                let identity = self.resolve_symbol_with_checker(&mut checker, target_node);
                out.push((name, identity));
            }
        }
        Ok(out)
    }

    fn resolve_symbol_with_checker(
        &self,
        checker: &mut tsrs_checker::Checker,
        node: P<Node>,
    ) -> Option<SymbolIdentity> {
        let direct = if let Some(parent) = node.parent()
            && parent.kind() == Kind::ShorthandPropertyAssignment
            && parent.as_shorthand_property_assignment().name() == node
        {
            checker.get_shorthand_assignment_value_symbol(Some(parent))
        } else {
            checker.get_symbol_at_location_exported(node)
        }?;

        let target = if direct.flags().intersects(SymbolFlags::Alias) {
            checker.get_aliased_symbol(direct)
        } else {
            direct
        };
        let is_namespace = target
            .flags()
            .intersects(SymbolFlags::ValueModule | SymbolFlags::NamespaceModule);
        let &declaration = target.declarations().first()?;
        let decl_sf = get_source_file_of_node(declaration)?;
        let declaring_file = decl_sf.file_name().to_owned();
        let module_part = resolve_profile_module(
            &declaring_file,
            &self.profile.intrinsics_module,
            self.effect_dist_prefix.as_deref(),
        )?;
        let fqn = checker.get_fully_qualified_name_exported(target);
        let qualified_name = strip_quoted_module_prefix(&fqn).to_owned();
        let id = format!("{module_part}#{qualified_name}");
        Some(SymbolIdentity {
            id,
            declaring_file,
            qualified_name,
            is_namespace,
        })
    }

    fn source_file(&self, file_name: &str) -> Result<P<SourceFile>, AuthoringError> {
        let normalized = normalize_virtual_path(file_name)?;
        self.program
            .get_source_file(&normalized)
            .ok_or(AuthoringError::SourceFileNotFound(normalized))
    }

    fn resolve_optional_source_file(
        &self,
        file_name: Option<&str>,
    ) -> Result<Option<P<SourceFile>>, AuthoringError> {
        file_name.map(|name| self.source_file(name)).transpose()
    }
}
