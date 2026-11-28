mod admitted;
mod calls;
mod cfg;
mod expressions;
mod imports;
mod resolve;
mod statements;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::Serialize;
use tsrs_ast::{Kind, Node, SourceFile};
use tsrs_checker::Checker;
use tsrs_compiler::{Program, sort_and_deduplicate_diagnostics};
use tsrs_core::P;
use tsrs_core::context::Context;
use tsrs_diagnostics::Category;

use crate::{AdmittedSymbol, AuthoringError, AuthoringProgram, BoundedVfs, Profile, SymbolUseKind};

use self::resolve::{
    display_file_path, flatten_diagnostic_message, format_construct, node_end, node_start, pos_of,
    slice_text,
};

/// 1-based line and UTF-16 column source position matching `tools/norn-ts/src/check.ts`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Position {
    /// 1-based line number.
    pub line: u32,
    /// 1-based UTF-16 code-unit column number.
    pub column: u32,
}

/// A `NORN-FLOW-*` diagnostic produced by [`ProfileChecker`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProfileDiagnostic {
    /// Stable diagnostic code (for example `NORN-FLOW-SYMBOL`).
    pub code: String,
    /// Inclusive 1-based start position.
    pub start: Position,
    /// Exclusive 1-based end position.
    pub end: Position,
    /// Single-line whitespace-collapsed source excerpt (max 80 characters).
    pub construct: String,
    /// Human-readable message with remediation guidance and profile identifier.
    pub message: String,
}

/// An admitted profile symbol reference recorded while walking a file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SymbolUse {
    /// Source location formatted as `<line>:<column>`.
    pub at: String,
    /// Canonical resolved symbol identity (for example `effect/Effect#gen`).
    pub symbol: String,
    /// Semantic lowering row key from [`Profile::rows`].
    pub row: String,
}

/// Full profile check report for one TypeScript source file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FileReport {
    /// Virtual file path relative to root (without leading `/`).
    pub file: String,
    /// Profile identifier (for example `norn-effect/2`).
    pub profile: String,
    /// Sorted `NORN-FLOW-*` diagnostics (empty when admitted).
    pub diagnostics: Vec<ProfileDiagnostic>,
    /// Sorted admitted symbol references discovered in the file.
    pub uses: Vec<SymbolUse>,
}

/// Native Rust port of `ProfileChecker` (`tools/norn-ts/src/check.ts`).
///
/// Resolves every symbol by its pinned declaration identity (following import
/// and re-export aliases via `tsrs_checker`), enforces the positive grammar of
/// the `norn-effect` profile, checks transitive local imports as a closure, and
/// emits stable `NORN-FLOW-*` diagnostics and admitted symbol uses.
#[derive(Clone, Debug)]
pub struct ProfileChecker {
    pub(super) profile: Profile,
    admitted: BTreeMap<String, AdmittedSymbol>,
    rejected: BTreeMap<String, String>,
    spellings: BTreeSet<String>,
    bare_spellings: BTreeSet<String>,
}

impl ProfileChecker {
    /// Builds a checker from a validated [`Profile`].
    #[must_use]
    pub fn new(profile: Profile) -> Self {
        let mut admitted = BTreeMap::new();
        let mut rejected = BTreeMap::new();
        let mut spellings = BTreeSet::new();
        let mut bare_spellings = BTreeSet::new();

        for entry in &profile.admitted {
            admitted.insert(entry.symbol.clone(), entry.clone());
            if entry.use_kind == SymbolUseKind::Method {
                continue;
            }
            let parts: Vec<&str> = entry.probe.split('.').skip(1).collect();
            if !parts.is_empty() {
                spellings.insert(parts.join("."));
                if let Some(&last) = parts.last()
                    && !last.is_empty()
                {
                    bare_spellings.insert(last.to_owned());
                }
            }
        }
        for entry in &profile.rejected {
            rejected.insert(entry.symbol.clone(), entry.code.clone());
        }

        Self {
            profile,
            admitted,
            rejected,
            spellings,
            bare_spellings,
        }
    }

    /// Returns the loaded profile metadata and symbol tables.
    #[must_use]
    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    /// Looks up an admitted symbol by canonical identity.
    #[must_use]
    pub fn admitted_entry(&self, id: &str) -> Option<&AdmittedSymbol> {
        self.admitted.get(id)
    }

    /// Returns the stable rejection diagnostic code for a non-admitted symbol
    /// declared in a profile module.
    #[must_use]
    pub fn code_for(&self, id: &str) -> &str {
        if let Some(code) = self.rejected.get(id) {
            return code.as_str();
        }
        let module = id.split_once('#').map_or(id, |(module, _)| module);
        for entry in &self.profile.rejected_modules {
            let matched = if entry.module.ends_with('/') {
                module.starts_with(entry.module.as_str())
            } else {
                module == entry.module
            };
            if matched {
                return entry.code.as_str();
            }
        }
        self.profile.default_code.as_str()
    }

    /// Returns `true` if `text` is spelled like an admitted profile symbol.
    #[must_use]
    pub fn spelled_like_admitted(&self, text: &str, bare: bool) -> bool {
        if bare {
            self.bare_spellings.contains(text)
        } else {
            self.spellings.contains(text)
        }
    }

    /// Checks `files` inside an already compiled [`AuthoringProgram`].
    ///
    /// Shares one compiled `tsrs` program across all requested files so pinned
    /// `effect` declarations load once and local imports are checked once.
    ///
    /// # Errors
    ///
    /// Returns [`AuthoringError::InvalidPath`] or
    /// [`AuthoringError::SourceFileNotFound`] if any requested file is missing
    /// from `program`.
    pub fn check_program(
        &self,
        program: &AuthoringProgram,
        files: &[&str],
    ) -> Result<Vec<FileReport>, AuthoringError> {
        let raw_program = program.raw_program();
        let vfs = program.vfs();
        let effect_dist_prefix = program.effect_dist_prefix();
        let mut source_files = Vec::with_capacity(files.len());
        for &file in files {
            source_files.push(program.source_file(file)?);
        }

        let mut walked: HashMap<P<SourceFile>, FileReport> = HashMap::new();
        let mut reports = Vec::with_capacity(source_files.len());
        for sf in source_files {
            let mut own = walked
                .entry(sf)
                .or_insert_with(|| self.walk_source_file(raw_program, vfs, effect_dist_prefix, sf))
                .clone();
            let import_diags = imports::import_diagnostics(
                self,
                raw_program,
                vfs,
                effect_dist_prefix,
                sf,
                &mut walked,
            );
            if !import_diags.is_empty() {
                own.diagnostics.extend(import_diags);
                sort_diagnostics(&mut own.diagnostics);
            }
            reports.push(own);
        }
        Ok(reports)
    }

    /// Compiles `vfs` with `files` as roots and checks all of them.
    ///
    /// # Errors
    ///
    /// Returns [`AuthoringError`] if program construction fails or a requested
    /// root file is invalid.
    pub fn check_vfs(
        &self,
        vfs: BoundedVfs,
        files: &[&str],
        effect_dist_prefix: Option<&str>,
    ) -> Result<Vec<FileReport>, AuthoringError> {
        let mut program = AuthoringProgram::new(vfs, files, self.profile.clone())?;
        if let Some(prefix) = effect_dist_prefix {
            program = program.with_effect_dist_prefix(prefix);
        }
        self.check_program(&program, files)
    }

    /// Resolves the canonical profile symbol identity of each top-level
    /// `const x = <expr>` initializer in `file` (used for probe verification).
    ///
    /// # Errors
    ///
    /// Returns [`AuthoringError::InvalidPath`] or
    /// [`AuthoringError::SourceFileNotFound`] if `file` is not in `program`.
    pub fn resolve_initializers(
        &self,
        program: &AuthoringProgram,
        file: &str,
    ) -> Result<Vec<(String, Option<String>)>, AuthoringError> {
        let sf = program.source_file(file)?;
        let raw_program = program.raw_program();
        let ctx = Context::default();
        let mut checker = raw_program.get_type_checker_for_file_exclusive(&ctx, sf);
        let effect_dist_prefix = program.effect_dist_prefix();
        let mut out = Vec::new();
        for &stmt in sf.statements.nodes() {
            if stmt.kind() != Kind::VariableStatement {
                continue;
            }
            let decls = stmt
                .as_variable_statement()
                .declaration_list()
                .as_variable_declaration_list()
                .declarations();
            for &decl in decls.nodes() {
                let var_decl = decl.as_variable_declaration();
                if var_decl.name().kind() != Kind::Identifier {
                    continue;
                }
                let Some(init) = var_decl.initializer() else {
                    continue;
                };
                let target_node = if init.kind() == Kind::PropertyAccessExpression {
                    init.as_property_access_expression().name()
                } else {
                    init
                };
                let resolved = resolve::resolve_symbol(
                    &mut checker,
                    &self.profile.intrinsics_module,
                    effect_dist_prefix,
                    program.vfs(),
                    target_node,
                );
                out.push((var_decl.name().text().to_owned(), resolved.id));
            }
        }
        Ok(out)
    }

    pub(super) fn walk_source_file(
        &self,
        program: &'static Program,
        vfs: &BoundedVfs,
        effect_dist_prefix: Option<&str>,
        sf: P<SourceFile>,
    ) -> FileReport {
        let ctx = Context::default();
        let mut checker = program.get_type_checker_for_file_exclusive(&ctx, sf);
        let mut walk = FileWalk {
            owner: self,
            program,
            checker: &mut checker,
            sf,
            vfs,
            effect_dist_prefix,
            diagnostics: Vec::new(),
            uses: Vec::new(),
            json_data: HashMap::new(),
            bounded_json_data: HashSet::new(),
            json_data_depth_exceeded: false,
        };
        walk.run(&ctx)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ReturnMode {
    None,
    FlowResult,
    MapResult,
    Effect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Ctx {
    flags: u8,
    pub(super) ret: ReturnMode,
}

impl Ctx {
    const LAW_SUBJECT: u8 = 1 << 0;
    const FLOW: u8 = 1 << 1;
    const GENERATOR: u8 = 1 << 2;
    const RETRY_POLICY: u8 = 1 << 3;
    const PURE: u8 = 1 << 4;
    const EFFECT: u8 = 1 << 5;

    pub(super) const TOP: Self = Self {
        flags: 0,
        ret: ReturnMode::None,
    };

    const fn has(self, bit: u8) -> bool {
        (self.flags & bit) != 0
    }

    const fn set(self, bit: u8, value: bool) -> Self {
        let flags = if value {
            self.flags | bit
        } else {
            self.flags & !bit
        };
        Self {
            flags,
            ret: self.ret,
        }
    }

    pub(super) const fn law_subject(self) -> bool {
        self.has(Self::LAW_SUBJECT)
    }

    pub(super) const fn flow(self) -> bool {
        self.has(Self::FLOW)
    }

    pub(super) const fn generator(self) -> bool {
        self.has(Self::GENERATOR)
    }

    pub(super) const fn retry_policy(self) -> bool {
        self.has(Self::RETRY_POLICY)
    }

    pub(super) const fn pure(self) -> bool {
        self.has(Self::PURE)
    }

    pub(super) const fn effect(self) -> bool {
        self.has(Self::EFFECT)
    }

    pub(super) const fn with_effect(self, value: bool) -> Self {
        self.set(Self::EFFECT, value)
    }

    pub(super) const fn with_flow(self, value: bool) -> Self {
        self.set(Self::FLOW, value)
    }

    pub(super) const fn with_retry_policy(self, value: bool) -> Self {
        self.set(Self::RETRY_POLICY, value)
    }

    pub(super) const fn with_law_subject(self, value: bool) -> Self {
        self.set(Self::LAW_SUBJECT, value)
    }

    pub(super) const fn for_generator(self) -> Self {
        let next = self
            .set(Self::GENERATOR, true)
            .set(Self::RETRY_POLICY, false)
            .set(Self::PURE, false);
        Self {
            flags: next.flags,
            ret: ReturnMode::FlowResult,
        }
    }

    pub(super) const fn for_callback(self, is_pure: bool) -> Self {
        let next = self
            .set(Self::GENERATOR, false)
            .set(Self::RETRY_POLICY, false)
            .set(Self::PURE, is_pure)
            .set(Self::EFFECT, false);
        Self {
            flags: next.flags,
            ret: if is_pure {
                ReturnMode::MapResult
            } else {
                ReturnMode::Effect
            },
        }
    }
}

pub(super) struct FileWalk<'a> {
    pub(super) owner: &'a ProfileChecker,
    pub(super) program: &'static Program,
    pub(super) checker: &'a mut Checker,
    pub(super) sf: P<SourceFile>,
    pub(super) vfs: &'a BoundedVfs,
    pub(super) effect_dist_prefix: Option<&'a str>,
    pub(super) diagnostics: Vec<ProfileDiagnostic>,
    pub(super) uses: Vec<SymbolUse>,
    pub(super) json_data: HashMap<P<Node>, bool>,
    pub(super) bounded_json_data: HashSet<P<Node>>,
    pub(super) json_data_depth_exceeded: bool,
}

impl FileWalk<'_> {
    fn run(&mut self, ctx: &Context) -> FileReport {
        let mut pre_emit = self.program.get_syntactic_diagnostics(ctx, Some(self.sf));
        pre_emit.extend(self.program.get_semantic_diagnostics_with_checker(
            ctx,
            self.checker,
            self.sf,
        ));
        let deduped = sort_and_deduplicate_diagnostics(&pre_emit);
        for diag in deduped {
            if diag.category() != Category::Error {
                continue;
            }
            let Some(start) = diag.pos() else {
                continue;
            };
            let end = diag.end().unwrap_or(start);
            let message = flatten_diagnostic_message(diag);
            self.emit_span(start, end, "NORN-FLOW-TYPE", &message);
        }

        let stmts = self.sf.statements.nodes();
        for &stmt in stmts {
            self.top_level(stmt);
        }

        sort_diagnostics(&mut self.diagnostics);
        self.uses.sort_by_key(|usage| parse_at_key(&usage.at));

        FileReport {
            file: display_file_path(self.sf.file_name()),
            profile: self.owner.profile.profile.clone(),
            diagnostics: std::mem::take(&mut self.diagnostics),
            uses: std::mem::take(&mut self.uses),
        }
    }

    pub(super) fn emit_span(&mut self, start: u32, end: u32, code: &str, detail: &str) {
        let construct = format_construct(&slice_text(self.sf, start, end));
        let summary = self.owner.profile.diagnostics.get(code);
        let suffix = summary.map_or_else(String::new, |doc| format!(" — {}", doc.fix));
        self.diagnostics.push(ProfileDiagnostic {
            code: code.to_owned(),
            start: pos_of(self.sf, start),
            end: pos_of(self.sf, end),
            construct,
            message: format!("{detail}{suffix} [{}]", self.owner.profile.profile),
        });
    }

    pub(super) fn report(&mut self, node: P<Node>, code: &str, detail: &str) {
        self.emit_span(node_start(self.sf, node), node_end(node), code, detail);
    }

    pub(super) fn record_use(&mut self, node: P<Node>, entry: &AdmittedSymbol) {
        let p = pos_of(self.sf, node_start(self.sf, node));
        self.uses.push(SymbolUse {
            at: format!("{}:{}", p.line, p.column),
            symbol: entry.symbol.clone(),
            row: entry.row.clone(),
        });
    }
}

fn sort_diagnostics(diagnostics: &mut [ProfileDiagnostic]) {
    diagnostics.sort_by(|left, right| {
        left.start
            .line
            .cmp(&right.start.line)
            .then_with(|| left.start.column.cmp(&right.start.column))
            .then_with(|| left.code.cmp(&right.code))
    });
}

fn parse_at_key(at: &str) -> (u32, u32) {
    let Some((line, col)) = at.split_once(':') else {
        return (0, 0);
    };
    (line.parse().unwrap_or(0), col.parse().unwrap_or(0))
}
