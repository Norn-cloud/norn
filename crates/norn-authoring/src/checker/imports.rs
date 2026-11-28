use std::collections::{HashMap, HashSet};

use tsrs_ast::{Kind, Node, SourceFile, get_source_file_of_node};
use tsrs_compiler::Program;
use tsrs_core::P;
use tsrs_core::context::Context;

use crate::{BoundedVfs, INTRINSICS_VIRTUAL_PATH};

use super::resolve::{display_file_path, node_end, node_start, node_text, pos_of};
use super::{FileReport, FileWalk, ProfileChecker, ProfileDiagnostic};

impl FileWalk<'_> {
    /// Import allowlist (`allowedImports`): the pinned `effect` package and its
    /// subpaths, the Norn intrinsics module, and relative TypeScript source
    /// modules. Side-effect-only imports are rejected.
    pub(super) fn module_import(&mut self, stmt: P<Node>) {
        let (specifier, is_side_effect_import) = if stmt.kind() == Kind::ImportDeclaration {
            let import = stmt.as_import_declaration();
            (
                Some(import.module_specifier),
                import.import_clause.is_none(),
            )
        } else {
            let export = stmt.as_export_declaration();
            (export.module_specifier, false)
        };
        let Some(specifier) = specifier else {
            return;
        };
        if specifier.kind() != Kind::StringLiteral {
            self.report(
                specifier,
                "NORN-FLOW-MODULE",
                "module specifiers must be string literals",
            );
            return;
        }
        let name = specifier.text();
        if is_side_effect_import {
            self.report(
                stmt,
                "NORN-FLOW-MODULE",
                &format!("side-effect-only import of \"{name}\" is not admitted"),
            );
            return;
        }
        let target = self
            .checker
            .get_symbol_at_location_exported(specifier)
            .and_then(|sym| sym.declarations().first().copied())
            .and_then(get_source_file_of_node);

        if self.is_listed_import(name) {
            self.check_listed_import(specifier, name, target);
            return;
        }
        if name.starts_with("./") || name.starts_with("../") {
            self.check_relative_import(specifier, name, target);
            return;
        }
        let intrinsics = &self.owner.profile.intrinsics_module;
        self.report(
            specifier,
            "NORN-FLOW-MODULE",
            &format!(
                "\"{name}\" is outside the import allowlist (effect, effect/*, {intrinsics}, local modules)"
            ),
        );
    }

    fn is_listed_import(&self, name: &str) -> bool {
        name == self.owner.profile.intrinsics_module
            || self
                .owner
                .profile
                .allowed_imports
                .packages
                .iter()
                .any(|pkg| {
                    pkg.strip_suffix("/*").map_or(name == pkg, |prefix| {
                        name.starts_with(prefix)
                            && name
                                .as_bytes()
                                .get(prefix.len())
                                .is_some_and(|&byte| byte == b'/')
                    })
                })
    }

    fn check_listed_import(
        &mut self,
        specifier: P<Node>,
        name: &str,
        target: Option<P<SourceFile>>,
    ) {
        let Some(target_sf) = target else {
            return;
        };
        let file = target_sf.file_name();
        let pinned_ok = if name == self.owner.profile.intrinsics_module {
            file == INTRINSICS_VIRTUAL_PATH && self.vfs.is_pinned_file(file)
        } else {
            self.vfs.is_pinned_file(file)
                && self
                    .effect_dist_prefix
                    .is_some_and(|prefix| file.starts_with(prefix))
        };
        if !pinned_ok {
            self.report(
                specifier,
                "NORN-FLOW-MODULE",
                &format!(
                    "\"{name}\" resolves to {}, not the pinned module",
                    display_file_path(file)
                ),
            );
        }
    }

    fn check_relative_import(
        &mut self,
        specifier: P<Node>,
        name: &str,
        target: Option<P<SourceFile>>,
    ) {
        let Some(target_sf) = target else {
            return;
        };
        let file = target_sf.file_name();
        let in_node_modules = file.split(['/', '\\']).any(|seg| seg == "node_modules");
        let is_ts_source = file.ends_with(".ts") || file.ends_with(".mts");
        if target_sf.is_declaration_file()
            || self.program.is_source_file_from_external_library(target_sf)
            || in_node_modules
            || !is_ts_source
        {
            self.report(
                specifier,
                "NORN-FLOW-MODULE",
                &format!("\"{name}\" is not a TypeScript source module the profile can check"),
            );
        }
    }
}

/// Local TypeScript source modules imported or re-exported by `sf`.
pub(super) fn local_imports(
    program: &'static Program,
    sf: P<SourceFile>,
) -> Vec<(P<Node>, P<SourceFile>)> {
    let ctx = Context::default();
    let mut checker = program.get_type_checker_for_file_exclusive(&ctx, sf);
    let mut out = Vec::new();
    for &stmt in sf.statements.nodes() {
        let specifier = if stmt.kind() == Kind::ImportDeclaration {
            Some(stmt.as_import_declaration().module_specifier)
        } else if stmt.kind() == Kind::ExportDeclaration {
            stmt.as_export_declaration().module_specifier
        } else {
            None
        };
        let Some(specifier) = specifier else {
            continue;
        };
        let Some(target) = checker
            .get_symbol_at_location_exported(specifier)
            .and_then(|sym| sym.declarations().first().copied())
            .and_then(get_source_file_of_node)
        else {
            continue;
        };
        if target.is_declaration_file() || program.is_source_file_from_external_library(target) {
            continue;
        }
        let file = target.file_name();
        if file.contains("/node_modules/") || file.contains("\\node_modules\\") {
            continue;
        }
        out.push((specifier, target));
    }
    out
}

/// Checks the transitive local import closure of `root`, emitting
/// `NORN-FLOW-IMPORT` at any direct import specifier whose target closure fails.
pub(super) fn import_diagnostics(
    owner: &ProfileChecker,
    program: &'static Program,
    vfs: &BoundedVfs,
    effect_dist_prefix: Option<&str>,
    root: P<SourceFile>,
    walked: &mut HashMap<P<SourceFile>, FileReport>,
) -> Vec<ProfileDiagnostic> {
    let mut out = Vec::new();
    let fix = owner
        .profile
        .diagnostics
        .get("NORN-FLOW-IMPORT")
        .map_or("", |doc| doc.fix.as_str());
    for (specifier, target) in local_imports(program, root) {
        let mut seen = HashSet::new();
        seen.insert(root);
        let Some(found) = first_failure(
            owner,
            program,
            vfs,
            effect_dist_prefix,
            target,
            &mut seen,
            walked,
        ) else {
            continue;
        };
        let start = pos_of(root, node_start(root, specifier));
        let end = pos_of(root, node_end(specifier));
        out.push(ProfileDiagnostic {
            code: "NORN-FLOW-IMPORT".to_owned(),
            start,
            end,
            construct: node_text(root, specifier).to_owned(),
            message: format!(
                "imported module fails the profile: {found} — {fix} [{}]",
                owner.profile.profile
            ),
        });
    }
    out
}

fn first_failure(
    owner: &ProfileChecker,
    program: &'static Program,
    vfs: &BoundedVfs,
    effect_dist_prefix: Option<&str>,
    sf: P<SourceFile>,
    seen: &mut HashSet<P<SourceFile>>,
    walked: &mut HashMap<P<SourceFile>, FileReport>,
) -> Option<String> {
    if !seen.insert(sf) {
        return None;
    }
    let report = walked
        .entry(sf)
        .or_insert_with(|| owner.walk_source_file(program, vfs, effect_dist_prefix, sf));
    if let Some(diag) = report.diagnostics.first() {
        return Some(format!(
            "{}:{}:{} {} ({})",
            report.file, diag.start.line, diag.start.column, diag.code, diag.construct
        ));
    }
    for (_, dep) in local_imports(program, sf) {
        if let Some(found) =
            first_failure(owner, program, vfs, effect_dist_prefix, dep, seen, walked)
        {
            return Some(found);
        }
    }
    None
}
