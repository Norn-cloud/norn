//! Per-file non-test LOC ceiling with a shrink-only allowlist (`loc-baseline.json`).
//!
//! Every non-test `.rs` source file under `crates/` must stay at or below
//! [`MAX_NON_TEST_LOC`] (1,000) non-test lines, unless it is grandfathered in
//! `crates/norn-skeleton-guard/loc-baseline.json` at its frozen ceiling.
//!
//! - New files exceeding [`MAX_NON_TEST_LOC`] fail closed (`loc-cap-exceeded`).
//! - Grandfathered files growing by even +1 non-test line above their frozen
//!   ceiling fail closed (`loc-cap-exceeded`).
//! - Grandfathered files shrinking at or below their frozen ceiling are
//!   accepted so normal cleanup PRs do not require a gated edit to
//!   `crates/norn-skeleton-guard/loc-baseline.json`.
//! - `scripts/baseline-shrink-only.sh` ensures `loc-baseline.json` can never
//!   gain new files, raise `max_non_test_loc`, or raise any recorded ceiling.
//!
//! # Non-test LOC definition
//!
//! Only crate-level integration tests (`crates/<crate>/tests/...`) and module
//! files reached **exclusively** through `#[cfg(test)] mod ...;` declarations
//! (never reachable via any non-test `mod` edge and never a crate root such as
//! `lib.rs`, `main.rs`, `build.rs`, or `src/bin/*.rs`) are excluded as test
//! files. Inside every remaining `.rs` file, physical lines wholly belonging to
//! `#[cfg(test)]` (or `#[cfg(all(test, ...))]`) AST items (`mod`, `fn`,
//! `struct`, `enum`, `impl`, `trait`, `const`, `static`, `type`, `use`,
//! `extern`, `macro_rules!`, associated `impl`/`trait` items) are excluded via
//! `syn` + `proc-macro2` source spans; non-item `#[cfg(test)]` attributes on
//! struct fields, enum variants, function parameters, or closure parameters
//! never exclude production code.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use proc_macro2::Span;
use syn::visit::Visit;

use crate::ratchet::{cfg_is_test, has_cfg_test};
use crate::{BaselineError, ScanError, Violation, violation};

/// Maximum permitted non-test lines for any `.rs` file not grandfathered in
/// `loc-baseline.json`.
pub const MAX_NON_TEST_LOC: usize = 1_000;

/// Parsed `loc-baseline.json`: the pinned cap and per-file frozen ceilings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocBaseline {
    /// The pinned non-test LOC cap (must equal [`MAX_NON_TEST_LOC`]).
    pub max_non_test_loc: usize,
    /// Per-file frozen non-test LOC ceilings (every entry strictly above `max_non_test_loc`).
    pub files: BTreeMap<String, usize>,
}

fn first_cfg_test_attr_span(attrs: &[syn::Attribute]) -> Option<Span> {
    attrs.iter().find_map(|attr| {
        if matches!(attr.style, syn::AttrStyle::Outer)
            && attr.path().is_ident("cfg")
            && attr
                .parse_args::<syn::Meta>()
                .is_ok_and(|meta| cfg_is_test(&meta))
        {
            Some(attr.pound_token.span)
        } else {
            None
        }
    })
}

fn macro_delim_close(delim: &syn::MacroDelimiter) -> Span {
    match delim {
        syn::MacroDelimiter::Paren(token) => token.span.close(),
        syn::MacroDelimiter::Brace(token) => token.span.close(),
        syn::MacroDelimiter::Bracket(token) => token.span.close(),
    }
}

struct CfgTestSpanVisitor<'a> {
    lines: Vec<&'a str>,
    test_lines: Vec<bool>,
}

impl<'a> CfgTestSpanVisitor<'a> {
    fn new(source: &'a str) -> Self {
        let lines: Vec<&'a str> = source.lines().collect();
        let count = lines.len();
        Self {
            lines,
            test_lines: vec![false; count],
        }
    }

    fn mark_if_cfg_test(&mut self, attrs: &[syn::Attribute], end_span: Option<Span>) -> bool {
        let Some(start_span) = first_cfg_test_attr_span(attrs) else {
            return false;
        };
        if let Some(end_span) = end_span {
            self.mark_span(start_span, end_span);
        }
        true
    }

    fn mark_span(&mut self, start_span: Span, end_span: Span) {
        if self.lines.is_empty() {
            return;
        }
        let start = start_span.start();
        let end = end_span.end();
        let mut first = start.line.saturating_sub(1);
        let mut last = end
            .line
            .saturating_sub(1)
            .min(self.lines.len().saturating_sub(1));

        if let Some(line_text) = self.lines.get(first)
            && !line_text
                .chars()
                .take(start.column)
                .all(char::is_whitespace)
        {
            first = first.saturating_add(1);
        }

        if let Some(line_text) = self.lines.get(last) {
            let rest: String = line_text.chars().skip(end.column).collect();
            let trimmed = rest.trim();
            if !trimmed.is_empty() && !trimmed.starts_with("//") {
                let Some(prev) = last.checked_sub(1) else {
                    return;
                };
                last = prev;
            }
        }

        if first <= last
            && let Some(slice) = self.test_lines.get_mut(first..=last)
        {
            for slot in slice {
                *slot = true;
            }
        }
    }

    fn non_test_count(&self) -> usize {
        self.test_lines.iter().filter(|is_test| !**is_test).count()
    }
}

impl<'ast> Visit<'ast> for CfgTestSpanVisitor<'_> {
    fn visit_item_const(&mut self, node: &'ast syn::ItemConst) {
        if !self.mark_if_cfg_test(&node.attrs, Some(node.semi_token.span)) {
            syn::visit::visit_item_const(self, node);
        }
    }

    fn visit_item_enum(&mut self, node: &'ast syn::ItemEnum) {
        if !self.mark_if_cfg_test(&node.attrs, Some(node.brace_token.span.close())) {
            syn::visit::visit_item_enum(self, node);
        }
    }

    fn visit_item_extern_crate(&mut self, node: &'ast syn::ItemExternCrate) {
        if !self.mark_if_cfg_test(&node.attrs, Some(node.semi_token.span)) {
            syn::visit::visit_item_extern_crate(self, node);
        }
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if !self.mark_if_cfg_test(&node.attrs, Some(node.block.brace_token.span.close())) {
            syn::visit::visit_item_fn(self, node);
        }
    }

    fn visit_item_foreign_mod(&mut self, node: &'ast syn::ItemForeignMod) {
        if !self.mark_if_cfg_test(&node.attrs, Some(node.brace_token.span.close())) {
            syn::visit::visit_item_foreign_mod(self, node);
        }
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        if !self.mark_if_cfg_test(&node.attrs, Some(node.brace_token.span.close())) {
            syn::visit::visit_item_impl(self, node);
        }
    }

    fn visit_item_macro(&mut self, node: &'ast syn::ItemMacro) {
        let end = node
            .semi_token
            .as_ref()
            .map_or_else(|| macro_delim_close(&node.mac.delimiter), |tok| tok.span);
        if !self.mark_if_cfg_test(&node.attrs, Some(end)) {
            syn::visit::visit_item_macro(self, node);
        }
    }

    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        let end = node
            .content
            .as_ref()
            .map(|(brace, _)| brace.span.close())
            .or_else(|| node.semi.as_ref().map(|tok| tok.span));
        if !self.mark_if_cfg_test(&node.attrs, end) {
            syn::visit::visit_item_mod(self, node);
        }
    }

    fn visit_item_static(&mut self, node: &'ast syn::ItemStatic) {
        if !self.mark_if_cfg_test(&node.attrs, Some(node.semi_token.span)) {
            syn::visit::visit_item_static(self, node);
        }
    }

    fn visit_item_struct(&mut self, node: &'ast syn::ItemStruct) {
        let end = node
            .semi_token
            .as_ref()
            .map(|tok| tok.span)
            .or_else(|| match &node.fields {
                syn::Fields::Named(fields) => Some(fields.brace_token.span.close()),
                syn::Fields::Unnamed(fields) => Some(fields.paren_token.span.close()),
                syn::Fields::Unit => None,
            });
        if !self.mark_if_cfg_test(&node.attrs, end) {
            syn::visit::visit_item_struct(self, node);
        }
    }

    fn visit_item_trait(&mut self, node: &'ast syn::ItemTrait) {
        if !self.mark_if_cfg_test(&node.attrs, Some(node.brace_token.span.close())) {
            syn::visit::visit_item_trait(self, node);
        }
    }

    fn visit_item_trait_alias(&mut self, node: &'ast syn::ItemTraitAlias) {
        if !self.mark_if_cfg_test(&node.attrs, Some(node.semi_token.span)) {
            syn::visit::visit_item_trait_alias(self, node);
        }
    }

    fn visit_item_type(&mut self, node: &'ast syn::ItemType) {
        if !self.mark_if_cfg_test(&node.attrs, Some(node.semi_token.span)) {
            syn::visit::visit_item_type(self, node);
        }
    }

    fn visit_item_union(&mut self, node: &'ast syn::ItemUnion) {
        if !self.mark_if_cfg_test(&node.attrs, Some(node.fields.brace_token.span.close())) {
            syn::visit::visit_item_union(self, node);
        }
    }

    fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
        if !self.mark_if_cfg_test(&node.attrs, Some(node.semi_token.span)) {
            syn::visit::visit_item_use(self, node);
        }
    }

    fn visit_impl_item_const(&mut self, node: &'ast syn::ImplItemConst) {
        if !self.mark_if_cfg_test(&node.attrs, Some(node.semi_token.span)) {
            syn::visit::visit_impl_item_const(self, node);
        }
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        if !self.mark_if_cfg_test(&node.attrs, Some(node.block.brace_token.span.close())) {
            syn::visit::visit_impl_item_fn(self, node);
        }
    }

    fn visit_impl_item_macro(&mut self, node: &'ast syn::ImplItemMacro) {
        let end = node
            .semi_token
            .as_ref()
            .map_or_else(|| macro_delim_close(&node.mac.delimiter), |tok| tok.span);
        if !self.mark_if_cfg_test(&node.attrs, Some(end)) {
            syn::visit::visit_impl_item_macro(self, node);
        }
    }

    fn visit_impl_item_type(&mut self, node: &'ast syn::ImplItemType) {
        if !self.mark_if_cfg_test(&node.attrs, Some(node.semi_token.span)) {
            syn::visit::visit_impl_item_type(self, node);
        }
    }

    fn visit_trait_item_const(&mut self, node: &'ast syn::TraitItemConst) {
        if !self.mark_if_cfg_test(&node.attrs, Some(node.semi_token.span)) {
            syn::visit::visit_trait_item_const(self, node);
        }
    }

    fn visit_trait_item_fn(&mut self, node: &'ast syn::TraitItemFn) {
        let end = node
            .default
            .as_ref()
            .map(|block| block.brace_token.span.close())
            .or_else(|| node.semi_token.as_ref().map(|tok| tok.span));
        if !self.mark_if_cfg_test(&node.attrs, end) {
            syn::visit::visit_trait_item_fn(self, node);
        }
    }

    fn visit_trait_item_macro(&mut self, node: &'ast syn::TraitItemMacro) {
        let end = node
            .semi_token
            .as_ref()
            .map_or_else(|| macro_delim_close(&node.mac.delimiter), |tok| tok.span);
        if !self.mark_if_cfg_test(&node.attrs, Some(end)) {
            syn::visit::visit_trait_item_macro(self, node);
        }
    }

    fn visit_trait_item_type(&mut self, node: &'ast syn::TraitItemType) {
        if !self.mark_if_cfg_test(&node.attrs, Some(node.semi_token.span)) {
            syn::visit::visit_trait_item_type(self, node);
        }
    }
}

fn non_test_loc_parsed(source: &str, file: &syn::File) -> usize {
    let mut visitor = CfgTestSpanVisitor::new(source);
    if visitor.lines.is_empty() {
        return 0;
    }
    visitor.visit_file(file);
    visitor.non_test_count()
}

/// Scans a single Rust source file and returns the number of physical lines
/// outside `#[cfg(test)]` AST items. Fails closed (counts all lines) if the
/// source cannot be parsed as Rust.
#[must_use]
pub fn non_test_loc(source: &str) -> usize {
    let total_lines = source.lines().count();
    if total_lines == 0 {
        return 0;
    }
    let Ok(file) = syn::parse_file(source) else {
        return total_lines;
    };
    non_test_loc_parsed(source, &file)
}

/// A `mod name;` declaration: its resolved file candidates and whether it is
/// test-only (`#[cfg(test)]` on it or an enclosing inline module).
pub(crate) struct ModDecl {
    pub(crate) candidates: Vec<PathBuf>,
    pub(crate) test_only: bool,
}

/// Collects the out-of-line module declarations of one file.
struct ModDecls<'a> {
    dir: PathBuf,
    file_dir: &'a Path,
    inline: Vec<String>,
    test_depth: usize,
    found: Vec<ModDecl>,
}

impl<'ast> Visit<'ast> for ModDecls<'_> {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        let test = has_cfg_test(&node.attrs);
        let test_only = test || self.test_depth > 0;
        let name = node.ident.to_string();
        if node.content.is_none() {
            let explicit = node.attrs.iter().find_map(|attr| {
                if let syn::Meta::NameValue(pair) = &attr.meta
                    && pair.path.is_ident("path")
                    && let syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(path),
                        ..
                    }) = &pair.value
                {
                    Some(path.value())
                } else {
                    None
                }
            });
            let candidates = if let Some(path) = explicit {
                let base = if self.inline.is_empty() {
                    self.file_dir.to_path_buf()
                } else {
                    self.inline
                        .iter()
                        .fold(self.dir.clone(), |dir, m| dir.join(m))
                };
                vec![base.join(path)]
            } else {
                let dir = self
                    .inline
                    .iter()
                    .fold(self.dir.clone(), |dir, m| dir.join(m));
                vec![
                    dir.join(format!("{name}.rs")),
                    dir.join(&name).join("mod.rs"),
                ]
            };
            self.found.push(ModDecl {
                candidates,
                test_only,
            });
            return;
        }
        self.inline.push(name);
        if test {
            self.test_depth = self.test_depth.saturating_add(1);
        }
        syn::visit::visit_item_mod(self, node);
        if test {
            self.test_depth = self.test_depth.saturating_sub(1);
        }
        self.inline.pop();
    }
}

pub(crate) fn module_decls(path: &Path, file: &syn::File) -> Vec<ModDecl> {
    let file_dir = path.parent().unwrap_or_else(|| Path::new(""));
    let mod_rs = path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|name| matches!(name, "lib.rs" | "main.rs" | "mod.rs" | "build.rs"));
    let dir = if mod_rs {
        file_dir.to_path_buf()
    } else {
        file_dir.join(path.file_stem().unwrap_or_default())
    };
    let mut decls = ModDecls {
        dir,
        file_dir,
        inline: Vec::new(),
        test_depth: 0,
        found: Vec::new(),
    };
    decls.visit_file(file);
    decls.found
}

/// Normalizes `a/b/../c` to `a/c` without touching the file system.
pub(crate) fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            Component::Normal(_) | Component::RootDir | Component::Prefix(_) => out.push(part),
        }
    }
    out
}

fn normal_components(rel: &Path) -> Vec<&str> {
    rel.components()
        .filter_map(|part| match part {
            Component::Normal(segment) => segment.to_str(),
            Component::Prefix(_)
            | Component::RootDir
            | Component::CurDir
            | Component::ParentDir => None,
        })
        .collect()
}

/// Returns true only for crate-level integration tests (`crates/<crate>/tests/...`),
/// never for production modules named `tests` under `crates/<crate>/src/...`.
fn is_crate_integration_test(rel: &Path) -> bool {
    matches!(
        normal_components(rel).as_slice(),
        ["crates", _, "tests", ..]
    )
}

/// Returns true for Cargo target root files (`lib.rs`, `main.rs`, `build.rs`,
/// `src/bin/*.rs`, `examples/*.rs`, `benches/*.rs`), which can never be
/// exempted as `#[cfg(test)]`-only module files.
fn is_crate_root(rel: &Path) -> bool {
    matches!(
        normal_components(rel).as_slice(),
        ["crates", _, "build.rs"]
            | ["crates", _, "src", "lib.rs" | "main.rs"]
            | ["crates", _, "src", "bin", _]
            | ["crates", _, "src", "bin", _, "main.rs"]
            | ["crates", _, "examples" | "benches", _]
            | ["crates", _, "examples" | "benches", _, "main.rs"]
    )
}

/// Fail-closed test-file classifier for the LOC cap:
///
/// 1. Crate-level integration tests (`crates/<crate>/tests/...`) are test files
///    unless also reached via a non-test `mod` edge.
/// 2. A `#[cfg(test)]`-declared module file (and its transitive submodules) is
///    excluded ONLY if it is NOT a crate root AND NOT reachable via any
///    non-test `mod` edge.
fn find_loc_test_files(files: &BTreeMap<PathBuf, syn::File>) -> BTreeSet<PathBuf> {
    let decls: BTreeMap<&PathBuf, Vec<ModDecl>> = files
        .iter()
        .map(|(path, file)| (path, module_decls(path, file)))
        .collect();

    let mut has_incoming: BTreeSet<PathBuf> = BTreeSet::new();
    for found in decls.values() {
        for decl in found {
            for candidate in &decl.candidates {
                let candidate = normalize(candidate);
                if files.contains_key(&candidate) {
                    has_incoming.insert(candidate);
                }
            }
        }
    }

    let mut non_test: BTreeSet<PathBuf> = files
        .keys()
        .filter(|path| {
            is_crate_root(path)
                || (!is_crate_integration_test(path) && !has_incoming.contains(*path))
        })
        .cloned()
        .collect();

    loop {
        let mut grew = false;
        for (parent, found) in &decls {
            if !non_test.contains(*parent) {
                continue;
            }
            for decl in found {
                if decl.test_only {
                    continue;
                }
                for candidate in &decl.candidates {
                    let candidate = normalize(candidate);
                    if files.contains_key(&candidate) && non_test.insert(candidate) {
                        grew = true;
                    }
                }
            }
        }
        if !grew {
            break;
        }
    }

    let mut test_files: BTreeSet<PathBuf> = files
        .keys()
        .filter(|path| is_crate_integration_test(path) && !non_test.contains(*path))
        .cloned()
        .collect();

    loop {
        let mut grew = false;
        for (parent, found) in &decls {
            let parent_is_test = test_files.contains(*parent);
            for decl in found {
                if !(decl.test_only || parent_is_test) {
                    continue;
                }
                for candidate in &decl.candidates {
                    let candidate = normalize(candidate);
                    if files.contains_key(&candidate)
                        && !is_crate_root(&candidate)
                        && !non_test.contains(&candidate)
                        && test_files.insert(candidate)
                    {
                        grew = true;
                    }
                }
            }
        }
        if !grew {
            break;
        }
    }

    test_files
}

/// Counts non-test lines for every non-test `.rs` file under
/// `<workspace_root>/crates` (`target/` directories, crate-level `tests/`
/// directories, and exclusively `#[cfg(test)]`-reachable module files are
/// excluded). Paths are `/`-separated relative to `workspace_root`.
///
/// # Errors
///
/// Returns [`ScanError`] if `crates/` cannot be walked or a file cannot be
/// read or parsed.
pub fn loc_counts(workspace_root: &Path) -> Result<BTreeMap<String, usize>, ScanError> {
    let mut sources: BTreeMap<PathBuf, String> = BTreeMap::new();
    let mut parsed: BTreeMap<PathBuf, syn::File> = BTreeMap::new();
    let walker = walkdir::WalkDir::new(workspace_root.join("crates"))
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| entry.file_name() != "target");
    for entry in walker {
        let entry = entry.map_err(ScanError::Walk)?;
        let path = entry.path();
        if !entry.file_type().is_file() || path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        let rel = path
            .strip_prefix(workspace_root)
            .unwrap_or(path)
            .to_path_buf();
        let source =
            std::fs::read_to_string(path).map_err(|err| ScanError::Io(path.to_path_buf(), err))?;
        let file =
            syn::parse_file(&source).map_err(|err| ScanError::Parse(path.to_path_buf(), err))?;
        sources.insert(rel.clone(), source);
        parsed.insert(rel, file);
    }
    let test_files: BTreeSet<PathBuf> = find_loc_test_files(&parsed);
    let mut out = BTreeMap::new();
    for (rel, source) in &sources {
        if test_files.contains(rel) {
            continue;
        }
        let key = rel.to_string_lossy().replace('\\', "/");
        let count = parsed.get(rel).map_or_else(
            || non_test_loc(source),
            |file| non_test_loc_parsed(source, file),
        );
        out.insert(key, count);
    }
    Ok(out)
}

/// Parses `loc-baseline.json` and validates its structure and cap pin.
///
/// # Errors
///
/// Returns [`BaselineError`] if the JSON is malformed, `max_non_test_loc` does
/// not match [`MAX_NON_TEST_LOC`], `files` is missing, or any entry is not an
/// integer strictly greater than [`MAX_NON_TEST_LOC`].
pub fn parse_loc_baseline(baseline_json: &str) -> Result<LocBaseline, BaselineError> {
    let root: serde_json::Value =
        serde_json::from_str(baseline_json).map_err(|err| BaselineError(err.to_string()))?;
    let max_non_test_loc = root
        .get("max_non_test_loc")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| {
            BaselineError("loc-baseline.json: missing integer `max_non_test_loc`".to_owned())
        })?;
    if max_non_test_loc != MAX_NON_TEST_LOC {
        return Err(BaselineError(format!(
            "loc-baseline.json: `max_non_test_loc` is {max_non_test_loc}, expected {MAX_NON_TEST_LOC}"
        )));
    }
    let files_obj = root
        .get("files")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| BaselineError("loc-baseline.json: missing object `files`".to_owned()))?;
    let mut files = BTreeMap::new();
    for (path, ceiling) in files_obj {
        let ceiling = ceiling
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .filter(|value| *value > MAX_NON_TEST_LOC)
            .ok_or_else(|| {
                BaselineError(format!(
                    "loc-baseline.json: `files.\"{path}\"` must be an integer > {MAX_NON_TEST_LOC}; delete entries at or below the cap"
                ))
            })?;
        files.insert(path.clone(), ceiling);
    }
    Ok(LocBaseline {
        max_non_test_loc,
        files,
    })
}

/// Compares measured non-test LOC counts against `loc-baseline.json`:
///
/// - Any file not in `loc-baseline.json` with `loc > MAX_NON_TEST_LOC` fails
///   with `loc-cap-exceeded`.
/// - Any file in `loc-baseline.json` with `loc > ceiling` fails with
///   `loc-cap-exceeded`.
/// - Files shrinking at or below their ceiling pass (lowering `loc-baseline.json`
///   is encouraged in cleanup PRs, and `baseline-shrink-only.sh` guarantees
///   ceilings never rise).
///
/// # Errors
///
/// Returns [`BaselineError`] if `baseline_json` is malformed.
pub fn check_loc_cap(
    found: &BTreeMap<String, usize>,
    baseline_json: &str,
) -> Result<Vec<Violation>, BaselineError> {
    let baseline = parse_loc_baseline(baseline_json)?;
    let mut out = Vec::new();
    for (path, &loc) in found {
        if let Some(&ceiling) = baseline.files.get(path) {
            if loc > ceiling {
                out.push(violation(
                    "loc-cap-exceeded",
                    format!(
                        "{path} has {loc} non-test LOC, exceeding its frozen ceiling of {ceiling} \
                         in crates/norn-skeleton-guard/loc-baseline.json (cap: {}). \
                         Split the file into smaller modules; ceilings are shrink-only.",
                        baseline.max_non_test_loc
                    ),
                ));
            }
        } else if loc > baseline.max_non_test_loc {
            out.push(violation(
                "loc-cap-exceeded",
                format!(
                    "{path} has {loc} non-test LOC, exceeding the {} non-test LOC cap. \
                     Split the file into smaller modules; new entries in loc-baseline.json are forbidden.",
                    baseline.max_non_test_loc
                ),
            ));
        }
    }
    out.sort();
    Ok(out)
}

/// Builds the canonical `loc-baseline.json` payload matching every file in
/// `found` that currently exceeds [`MAX_NON_TEST_LOC`].
#[must_use]
pub fn loc_baseline_json(found: &BTreeMap<String, usize>) -> serde_json::Value {
    let files: serde_json::Map<String, serde_json::Value> = found
        .iter()
        .filter(|(_, loc)| **loc > MAX_NON_TEST_LOC)
        .map(|(path, loc)| (path.clone(), serde_json::Value::from(*loc)))
        .collect();
    serde_json::json!({
        "_comment": "Frozen shrink-only non-test LOC ceilings for files exceeding 1,000 non-test LOC (src/loc_cap.rs). Ceilings may only shrink or be removed; no new entries allowed (CI: scripts/baseline-shrink-only.sh).",
        "max_non_test_loc": MAX_NON_TEST_LOC,
        "files": files,
    })
}
