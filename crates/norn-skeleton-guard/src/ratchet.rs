//! Shrink-only ratchets: per-file counts of known violations under `crates/`.
//!
//! Each rule counts its violations per file, and `baseline.json` `ratchets`
//! records the count for every file that has any. [`check_ratchets`] fails
//! when a file has more violations than its baseline count (a regression) and
//! also when it has fewer (the baseline must be lowered in the same PR), so a
//! count can only go down. `scripts/baseline-shrink-only.sh` rejects a PR whose
//! baseline count rises relative to the base branch.
//!
//! What each rule counts is spelled out in [`RULES`]; the same text is the fix
//! a failing check prints.
//!
//! # Test code
//!
//! A file is test code when a path component under `crates/` is `tests`, or
//! when a test file or a `#[cfg(test)]` item declares it as a module (`mod x;`
//! resolved to `x.rs` or `x/mod.rs`, or through `#[path]`). Inside other files,
//! test code is any item under `#[cfg(test)]` (also `cfg(all(test, ..))`) and
//! the body of a test function.
//!
//! A test function has an attribute whose last path segment is `test`,
//! `rstest`, `test_case` or ends in `_test` (`#[test]`, `#[tokio::test]`,
//! `#[wasm_bindgen_test]`). Test functions written inside a macro body (for
//! example `proptest! { .. }`) are not parsed as items, so only their sleeps
//! and `#[ignore]`s are counted.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use proc_macro2::{Delimiter, TokenStream, TokenTree};
use syn::punctuated::Punctuated;
use syn::visit::Visit;

use crate::{BaselineError, ScanError, Violation, violation};

/// `#[ignore]` without a linked issue.
pub const IGNORE_WITHOUT_ISSUE: &str = "ignore-without-issue";
/// A sleep inside test code.
pub const TEST_SLEEP: &str = "test-sleep";
/// A test function that checks nothing.
pub const TEST_WITHOUT_ASSERTION: &str = "test-without-assertion";
/// An `allow` or `expect` of `clippy::too_many_lines`.
pub const TOO_MANY_LINES_ALLOW: &str = "too-many-lines-allow";
/// A lint allowed by an inner attribute (`#![allow(..)]`), counted per file
/// and lint: its baseline keys are `<path>: <lint>`.
pub const INNER_ALLOW: &str = "inner-allow";

/// Every ratchet rule, with the counted violation and its fix.
pub const RULES: &[(&str, &str)] = &[
    (
        IGNORE_WITHOUT_ISSUE,
        "an `#[ignore]` whose reason does not link an issue (`#1234` or `.../issues/1234`). Fix: \
         make the test pass and drop the `#[ignore]`, or give it the tracking issue: \
         `#[ignore = \"#1234: why it is ignored\"]`.",
    ),
    (
        TEST_SLEEP,
        "a `thread::sleep` or `time::sleep` (std, tokio or an imported `sleep`) in test code. Fix: \
         wait on the operation or its signal instead (join the handle, await the future, receive \
         on the channel); for timers use `tokio::time::pause` and `advance`.",
    ),
    (
        TEST_WITHOUT_ASSERTION,
        "a test function that checks nothing. A test checks something when it has \
         `#[should_panic]`, or its body has an `assert*!`/`prop_assert*!`/`debug_assert*!` macro \
         other than `assert!(true)`, a call to a function or method named `assert*`, `panic!` or \
         `unreachable!`, `.unwrap()`/`.expect()`/`.unwrap_err()`/`.expect_err()`, or `?` in a test \
         that returns a `Result`. Fix: add the check the test exists for, or delete the test.",
    ),
    (
        TOO_MANY_LINES_ALLOW,
        "an `allow` or `expect` of `clippy::too_many_lines` (outer, inner or in `cfg_attr`). Fix: \
         split the function into named steps instead of allowing the lint.",
    ),
    (
        INNER_ALLOW,
        "a lint allowed for a whole crate or module by an inner attribute (`#![allow(..)]`, \
         `#![expect(..)]`, also in `cfg_attr`), counted per file and lint. Fix: fix what the lint \
         reports, or put an `#[allow(lint, reason = \"..\")]` on the one item that needs it.",
    ),
];

/// Violation counts: rule, then path relative to the workspace root, then
/// count. Files without a violation of a rule are absent.
pub type RatchetCounts = BTreeMap<String, BTreeMap<String, usize>>;

/// One token of a flattened macro body.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Tok {
    Ident(String),
    Punct(char),
    Lit(String),
    Open(char),
    Close,
}

fn flatten(tokens: TokenStream, out: &mut Vec<Tok>) {
    for tree in tokens {
        match tree {
            TokenTree::Group(group) => {
                out.push(Tok::Open(match group.delimiter() {
                    Delimiter::Parenthesis => '(',
                    Delimiter::Bracket => '[',
                    Delimiter::Brace => '{',
                    Delimiter::None => ' ',
                }));
                flatten(group.stream(), out);
                out.push(Tok::Close);
            }
            TokenTree::Ident(ident) => out.push(Tok::Ident(ident.to_string())),
            TokenTree::Punct(punct) => out.push(Tok::Punct(punct.as_char())),
            TokenTree::Literal(lit) => out.push(Tok::Lit(lit.to_string())),
        }
    }
}

fn tokens_of(tokens: &TokenStream) -> Vec<Tok> {
    let mut out = Vec::new();
    flatten(tokens.clone(), &mut out);
    out
}

fn is_ident(tok: Option<&Tok>, name: &str) -> bool {
    matches!(tok, Some(Tok::Ident(found)) if found == name)
}

fn is_punct(tok: Option<&Tok>, c: char) -> bool {
    matches!(tok, Some(Tok::Punct(found)) if *found == c)
}

/// Whether `toks[at..]` starts with `first :: second`.
fn path_pair_at(toks: &[Tok], at: usize, first: &[&str], second: &str) -> bool {
    let tok = |offset: usize| at.checked_add(offset).and_then(|index| toks.get(index));
    matches!(tok(0), Some(Tok::Ident(name)) if first.contains(&name.as_str()))
        && is_punct(tok(1), ':')
        && is_punct(tok(2), ':')
        && is_ident(tok(3), second)
}

fn bump(counts: &mut BTreeMap<&'static str, usize>, rule: &'static str) {
    let count = counts.entry(rule).or_insert(0);
    *count = count.saturating_add(1);
}

/// Whether an `#[ignore]` reason names an issue: `#` or `/issues/` followed by
/// a digit.
#[must_use]
pub fn links_issue(reason: &str) -> bool {
    let digit_after = |marker: &str| {
        reason.match_indices(marker).any(|(start, _)| {
            reason
                .get(start.saturating_add(marker.len())..)
                .and_then(|tail| tail.chars().next())
                .is_some_and(|c| c.is_ascii_digit())
        })
    };
    digit_after("#") || digit_after("/issues/")
}

fn last_segment(path: &syn::Path) -> String {
    path.segments
        .last()
        .map(|segment| segment.ident.to_string())
        .unwrap_or_default()
}

fn is_test_attr(attr: &syn::Attribute) -> bool {
    let name = last_segment(attr.path());
    name == "test" || name == "rstest" || name == "test_case" || name.ends_with("_test")
}

fn nested_metas(list: &syn::MetaList) -> Vec<syn::Meta> {
    list.parse_args_with(Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated)
        .map(|metas| metas.into_iter().collect())
        .unwrap_or_default()
}

/// `cfg(test)`, `cfg(all(test, ..))`: the item exists only in test builds.
fn cfg_is_test(meta: &syn::Meta) -> bool {
    match meta {
        syn::Meta::Path(path) => path.is_ident("test"),
        syn::Meta::List(list) if list.path.is_ident("all") => {
            nested_metas(list).iter().any(cfg_is_test)
        }
        syn::Meta::List(_) | syn::Meta::NameValue(_) => false,
    }
}

fn has_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg")
            && attr
                .parse_args::<syn::Meta>()
                .is_ok_and(|meta| cfg_is_test(&meta))
    })
}

/// Counts `#[ignore]`s without an issue link and `too_many_lines` allows in
/// one attribute meta, looking through `cfg_attr`.
fn count_meta(meta: &syn::Meta, counts: &mut BTreeMap<&'static str, usize>) {
    let path = meta.path();
    if path.is_ident("ignore") {
        let linked = match meta {
            syn::Meta::NameValue(pair) => matches!(
                &pair.value,
                syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(reason), .. })
                    if links_issue(&reason.value())
            ),
            syn::Meta::Path(_) | syn::Meta::List(_) => false,
        };
        if !linked {
            bump(counts, IGNORE_WITHOUT_ISSUE);
        }
        return;
    }
    let syn::Meta::List(list) = meta else {
        return;
    };
    if path.is_ident("cfg_attr") {
        for nested in nested_metas(list).iter().skip(1) {
            count_meta(nested, counts);
        }
    } else if path.is_ident("allow") || path.is_ident("expect") {
        for nested in nested_metas(list) {
            let segments: Vec<String> = nested
                .path()
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect();
            if segments == ["clippy", "too_many_lines"] {
                bump(counts, TOO_MANY_LINES_ALLOW);
            }
        }
    }
}

/// Every lint an `allow`/`expect`/`warn`/`deny`/`forbid` attribute meta names,
/// with that level (`expect` reads as `allow`), looking through `cfg_attr`.
pub(crate) fn lint_levels(meta: &syn::Meta) -> Vec<(&'static str, String)> {
    let syn::Meta::List(list) = meta else {
        return Vec::new();
    };
    if list.path.is_ident("cfg_attr") {
        return nested_metas(list)
            .iter()
            .skip(1)
            .flat_map(lint_levels)
            .collect();
    }
    let level = match last_segment(&list.path).as_str() {
        "allow" | "expect" => "allow",
        "warn" => "warn",
        "deny" => "deny",
        "forbid" => "forbid",
        _ => return Vec::new(),
    };
    nested_metas(list)
        .iter()
        .filter_map(|nested| match nested {
            syn::Meta::Path(path) => Some((level, lint_name(path))),
            syn::Meta::List(_) | syn::Meta::NameValue(_) => None,
        })
        .collect()
}

fn lint_name(path: &syn::Path) -> String {
    path.segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

/// Counts what a macro body hides from the syntax tree: `#[ignore]`s,
/// `too_many_lines` allows and, in test code, sleeps.
fn count_macro_tokens(
    toks: &[Tok],
    in_test: bool,
    sleep_aliases: &BTreeSet<String>,
    counts: &mut BTreeMap<&'static str, usize>,
) {
    for (at, tok) in toks.iter().enumerate() {
        let next = |offset: usize| at.checked_add(offset).and_then(|index| toks.get(index));
        if *tok == Tok::Punct('#')
            && next(1) == Some(&Tok::Open('['))
            && is_ident(next(2), "ignore")
        {
            let linked = is_punct(next(3), '=')
                && matches!(next(4), Some(Tok::Lit(reason)) if links_issue(reason));
            if !linked {
                bump(counts, IGNORE_WITHOUT_ISSUE);
            }
        }
        if path_pair_at(toks, at, &["clippy"], "too_many_lines") {
            bump(counts, TOO_MANY_LINES_ALLOW);
        }
        if in_test {
            let qualified = path_pair_at(toks, at, &["thread", "time"], "sleep");
            let previous = at.checked_sub(1).and_then(|index| toks.get(index));
            let bare = matches!(tok, Tok::Ident(name) if sleep_aliases.contains(name))
                && !is_punct(previous, ':')
                && !is_punct(previous, '.')
                && next(1) == Some(&Tok::Open('('));
            if qualified || bare {
                bump(counts, TEST_SLEEP);
            }
        }
    }
}

/// Whether a flattened macro body checks something (see
/// [`TEST_WITHOUT_ASSERTION`]); `?` inside macro bodies is not counted.
fn macro_tokens_assert(toks: &[Tok]) -> bool {
    toks.iter().enumerate().any(|(at, tok)| {
        let next = |offset: usize| at.checked_add(offset).and_then(|index| toks.get(index));
        let previous = at.checked_sub(1).and_then(|index| toks.get(index));
        let Tok::Ident(name) = tok else {
            return false;
        };
        let bang = is_punct(next(1), '!');
        let call = next(1) == Some(&Tok::Open('('));
        if bang && name == "assert" {
            let trivially_true = next(2) == Some(&Tok::Open('('))
                && is_ident(next(3), "true")
                && next(4) == Some(&Tok::Close);
            return !trivially_true;
        }
        (bang && (name.contains("assert") || name == "panic" || name == "unreachable"))
            || (call && name.starts_with("assert"))
            || (is_punct(previous, '.') && is_unwrap(name))
    })
}

fn is_unwrap(name: &str) -> bool {
    matches!(name, "unwrap" | "expect" | "unwrap_err" | "expect_err")
}

/// Finds a check in a test function body.
struct AssertFinder {
    returns_value: bool,
    found: bool,
}

impl<'ast> Visit<'ast> for AssertFinder {
    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        let name = last_segment(&node.path);
        let toks = tokens_of(&node.tokens);
        let trivially_true = name == "assert" && toks == [Tok::Ident("true".to_owned())];
        if (name.contains("assert") && !trivially_true)
            || name == "panic"
            || name == "unreachable"
            || macro_tokens_assert(&toks)
        {
            self.found = true;
        }
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        let method = node.method.to_string();
        if is_unwrap(&method) || method.starts_with("assert") {
            self.found = true;
        }
        syn::visit::visit_expr_method_call(self, node);
    }

    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if let syn::Expr::Path(path) = &*node.func
            && last_segment(&path.path).starts_with("assert")
        {
            self.found = true;
        }
        syn::visit::visit_expr_call(self, node);
    }

    fn visit_expr_try(&mut self, node: &'ast syn::ExprTry) {
        if self.returns_value {
            self.found = true;
        }
        syn::visit::visit_expr_try(self, node);
    }
}

fn test_checks_something(node: &syn::ItemFn) -> bool {
    if node
        .attrs
        .iter()
        .any(|attr| last_segment(attr.path()) == "should_panic")
    {
        return true;
    }
    let returns_value = match &node.sig.output {
        syn::ReturnType::Default => false,
        syn::ReturnType::Type(_, ty) => {
            !matches!(&**ty, syn::Type::Tuple(tuple) if tuple.elems.is_empty())
        }
    };
    let mut finder = AssertFinder {
        returns_value,
        found: false,
    };
    finder.visit_block(&node.block);
    finder.found
}

/// Names a `use` brings `thread::sleep` / `time::sleep` in under.
#[derive(Default)]
struct SleepImports {
    aliases: BTreeSet<String>,
}

impl SleepImports {
    fn walk(&mut self, prefix: &mut Vec<String>, tree: &syn::UseTree) {
        let parent_is_clock = |prefix: &[String]| {
            prefix
                .last()
                .is_some_and(|last| last == "thread" || last == "time")
        };
        match tree {
            syn::UseTree::Path(path) => {
                prefix.push(path.ident.to_string());
                self.walk(prefix, &path.tree);
                prefix.pop();
            }
            syn::UseTree::Name(name) => {
                if name.ident == "sleep" && parent_is_clock(prefix) {
                    self.aliases.insert("sleep".to_owned());
                }
            }
            syn::UseTree::Rename(rename) => {
                if rename.ident == "sleep" && parent_is_clock(prefix) {
                    self.aliases.insert(rename.rename.to_string());
                }
            }
            syn::UseTree::Group(group) => {
                for item in &group.items {
                    self.walk(prefix, item);
                }
            }
            syn::UseTree::Glob(_) => {
                if parent_is_clock(prefix) {
                    self.aliases.insert("sleep".to_owned());
                }
            }
        }
    }
}

impl<'ast> Visit<'ast> for SleepImports {
    fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
        self.walk(&mut Vec::new(), &node.tree);
    }
}

/// Counts every rule over one parsed file.
struct RuleCounter {
    test_depth: usize,
    sleep_aliases: BTreeSet<String>,
    counts: BTreeMap<&'static str, usize>,
    inner_allows: BTreeMap<String, usize>,
}

impl RuleCounter {
    fn in_test(&self) -> bool {
        self.test_depth > 0
    }

    fn scoped(&mut self, test: bool, visit: impl FnOnce(&mut Self)) {
        if test {
            self.test_depth = self.test_depth.saturating_add(1);
        }
        visit(self);
        if test {
            self.test_depth = self.test_depth.saturating_sub(1);
        }
    }
}

impl<'ast> Visit<'ast> for RuleCounter {
    fn visit_attribute(&mut self, node: &'ast syn::Attribute) {
        count_meta(&node.meta, &mut self.counts);
        if matches!(node.style, syn::AttrStyle::Inner(_)) {
            for (level, lint) in lint_levels(&node.meta) {
                if level == "allow" {
                    let count = self.inner_allows.entry(lint).or_insert(0);
                    *count = count.saturating_add(1);
                }
            }
        }
    }

    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        self.scoped(has_cfg_test(&node.attrs), |this| {
            syn::visit::visit_item_mod(this, node);
        });
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        self.scoped(has_cfg_test(&node.attrs), |this| {
            syn::visit::visit_item_impl(this, node);
        });
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        let is_test = node.attrs.iter().any(is_test_attr);
        if is_test && !test_checks_something(node) {
            bump(&mut self.counts, TEST_WITHOUT_ASSERTION);
        }
        self.scoped(is_test || has_cfg_test(&node.attrs), |this| {
            syn::visit::visit_item_fn(this, node);
        });
    }

    fn visit_path(&mut self, node: &'ast syn::Path) {
        if self.in_test() {
            let names: Vec<String> = node
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect();
            let qualified = names.len() >= 2
                && names.last().is_some_and(|last| last == "sleep")
                && names
                    .iter()
                    .rev()
                    .nth(1)
                    .is_some_and(|parent| parent == "thread" || parent == "time");
            let bare = node.leading_colon.is_none()
                && names.len() == 1
                && names
                    .first()
                    .is_some_and(|name| self.sleep_aliases.contains(name));
            if qualified || bare {
                bump(&mut self.counts, TEST_SLEEP);
            }
        }
        syn::visit::visit_path(self, node);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        let toks = tokens_of(&node.tokens);
        let in_test = self.in_test();
        count_macro_tokens(&toks, in_test, &self.sleep_aliases, &mut self.counts);
        syn::visit::visit_macro(self, node);
    }
}

/// Counts every rule in one Rust source; `test_file` marks the whole file as
/// test code (see the module docs).
///
/// # Errors
///
/// [`syn::Error`] if `source` does not parse.
pub fn count_source(
    source: &str,
    test_file: bool,
) -> Result<BTreeMap<&'static str, usize>, syn::Error> {
    let file = syn::parse_file(source)?;
    let (mut counts, inner_allows) = count_file(&file, test_file);
    let total = inner_allows
        .values()
        .fold(0_usize, |sum, n| sum.saturating_add(*n));
    if total > 0 {
        counts.insert(INNER_ALLOW, total);
    }
    Ok(counts)
}

/// Rule counts and, per lint, the inner allows of one file.
type FileCounts = (BTreeMap<&'static str, usize>, BTreeMap<String, usize>);

fn count_file(file: &syn::File, test_file: bool) -> FileCounts {
    let mut imports = SleepImports::default();
    imports.visit_file(file);
    let mut counter = RuleCounter {
        test_depth: usize::from(test_file),
        sleep_aliases: imports.aliases,
        counts: BTreeMap::new(),
        inner_allows: BTreeMap::new(),
    };
    counter.visit_file(file);
    (counter.counts, counter.inner_allows)
}

/// A `mod name;` declaration: its resolved file candidates and whether it is
/// test-only (`#[cfg(test)]` on it or an enclosing inline module).
struct ModDecl {
    candidates: Vec<PathBuf>,
    test_only: bool,
}

/// Collects the out-of-line module declarations of one file.
struct ModDecls<'a> {
    /// Directory child modules of the file live in.
    dir: PathBuf,
    /// Directory the file itself is in (`#[path]` is relative to it).
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

fn module_decls(path: &Path, file: &syn::File) -> Vec<ModDecl> {
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
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            std::path::Component::Normal(_)
            | std::path::Component::RootDir
            | std::path::Component::Prefix(_) => out.push(part),
        }
    }
    out
}

/// Counts every rule over every `.rs` file under `<workspace_root>/crates`
/// (`target` directories excluded). Paths are relative to the workspace root.
///
/// # Errors
///
/// Returns [`ScanError`] if the directory cannot be walked or a file cannot
/// be read or parsed.
pub fn ratchet_counts(workspace_root: &Path) -> Result<RatchetCounts, ScanError> {
    let mut files: BTreeMap<PathBuf, syn::File> = BTreeMap::new();
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
        files.insert(rel, file);
    }
    let decls: BTreeMap<&PathBuf, Vec<ModDecl>> = files
        .iter()
        .map(|(path, file)| (path, module_decls(path, file)))
        .collect();
    let mut test_files: BTreeSet<PathBuf> = files
        .keys()
        .filter(|path| path.components().any(|part| part.as_os_str() == "tests"))
        .cloned()
        .collect();
    loop {
        let mut grew = false;
        for (path, found) in &decls {
            let parent_is_test = test_files.contains(*path);
            for decl in found {
                if !(decl.test_only || parent_is_test) {
                    continue;
                }
                for candidate in &decl.candidates {
                    let candidate = normalize(candidate);
                    if files.contains_key(&candidate) && test_files.insert(candidate) {
                        grew = true;
                    }
                }
            }
        }
        if !grew {
            break;
        }
    }
    let mut out: RatchetCounts = RULES
        .iter()
        .map(|(rule, _)| ((*rule).to_owned(), BTreeMap::new()))
        .collect();
    for (path, file) in &files {
        let key = path.to_string_lossy().replace('\\', "/");
        let (counts, inner_allows) = count_file(file, test_files.contains(path));
        for (rule, count) in counts {
            out.entry(rule.to_owned())
                .or_default()
                .insert(key.clone(), count);
        }
        for (lint, count) in inner_allows {
            out.entry(INNER_ALLOW.to_owned())
                .or_default()
                .insert(format!("{key}: {lint}"), count);
        }
    }
    Ok(out)
}

/// Reads `baseline.json` `ratchets`: every rule of [`RULES`] maps paths to
/// positive counts.
///
/// # Errors
///
/// Returns [`BaselineError`] if `ratchets` is missing, names an unknown rule,
/// lacks a rule, or holds a count that is not a positive integer.
pub fn baseline_ratchets(baseline_json: &str) -> Result<RatchetCounts, BaselineError> {
    let root: serde_json::Value =
        serde_json::from_str(baseline_json).map_err(|err| BaselineError(err.to_string()))?;
    let ratchets = root
        .get("ratchets")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| BaselineError("missing object `ratchets`".to_owned()))?;
    let known: BTreeSet<&str> = RULES.iter().map(|(rule, _)| *rule).collect();
    let mut out = RatchetCounts::new();
    for (rule, files) in ratchets {
        if !known.contains(rule.as_str()) {
            return Err(BaselineError(format!("unknown ratchet rule `{rule}`")));
        }
        let files = files
            .as_object()
            .ok_or_else(|| BaselineError(format!("`ratchets.{rule}` must be an object")))?;
        let mut counts = BTreeMap::new();
        for (path, count) in files {
            let count = count
                .as_u64()
                .and_then(|count| usize::try_from(count).ok())
                .filter(|count| *count > 0)
                .ok_or_else(|| {
                    BaselineError(format!(
                        "`ratchets.{rule}.{path}` must be a positive integer; delete entries at 0"
                    ))
                })?;
            counts.insert(path.clone(), count);
        }
        out.insert(rule.clone(), counts);
    }
    for rule in known {
        if !out.contains_key(rule) {
            return Err(BaselineError(format!("missing ratchet rule `{rule}`")));
        }
    }
    Ok(out)
}

/// Compares found counts with the baseline: a file above its count fails
/// with the rule's fix, and a file below it (or gone) fails until the baseline
/// is lowered to the found count in the same PR.
///
/// # Errors
///
/// Returns [`BaselineError`] if the baseline's `ratchets` are malformed.
pub fn check_ratchets(
    found: &RatchetCounts,
    baseline_json: &str,
) -> Result<Vec<Violation>, BaselineError> {
    let baseline = baseline_ratchets(baseline_json)?;
    let empty = BTreeMap::new();
    let mut out = Vec::new();
    for (rule, fix) in RULES {
        let found = found.get(*rule).unwrap_or(&empty);
        let allowed = baseline.get(*rule).unwrap_or(&empty);
        let paths: BTreeSet<&String> = found.keys().chain(allowed.keys()).collect();
        for path in paths {
            let now = found.get(path).copied().unwrap_or(0);
            let was = allowed.get(path).copied().unwrap_or(0);
            if now > was {
                out.push(violation(
                    "ratchet-exceeded",
                    format!(
                        "{path} has {now} × {rule}, baseline.json allows {was}. Counted: {fix} \
                         The baseline only goes down; never raise it."
                    ),
                ));
            } else if now < was {
                let set = if now == 0 {
                    format!("delete `ratchets.{rule}.\"{path}\"`")
                } else {
                    format!("set `ratchets.{rule}.\"{path}\"` to {now}")
                };
                out.push(violation(
                    "ratchet-stale",
                    format!(
                        "{path} is down to {now} × {rule} from {was}: {set} in \
                         crates/norn-skeleton-guard/baseline.json in this PR."
                    ),
                ));
            }
        }
    }
    Ok(out)
}

/// The `ratchets` object for `baseline.json` that matches `found` exactly
/// (rules always present, zero counts omitted).
#[must_use]
pub fn ratchets_json(found: &RatchetCounts) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    for (rule, _) in RULES {
        let files: serde_json::Map<String, serde_json::Value> = found
            .get(*rule)
            .into_iter()
            .flatten()
            .filter(|(_, count)| **count > 0)
            .map(|(path, count)| (path.clone(), serde_json::Value::from(*count)))
            .collect();
        out.insert((*rule).to_owned(), serde_json::Value::Object(files));
    }
    serde_json::Value::Object(out)
}
