//! Artifact promotion governance parity between `ArtifactKind` in `norn-ir` and
//! its closed `authority_bearing` proof (inline Verus or legacy F*).

use std::collections::BTreeSet;
use std::path::Path;

use proc_macro2::{Delimiter, Spacing, TokenStream, TokenTree as TT};

const VERUS_SOURCE: &str = "crates/norn-ir/src/artifact_governance.rs";
const FSTAR_PROOF: &str =
    "verification/stripmine-2026-08-22/proofs/fstar/ArtifactPromotionProof.fst";
const SIG_ERR: &str = "authority_bearing must be `pub open spec fn authority_bearing(kind: ArtifactKind) -> bool { ... }`";
const PAT_ERR: &str = "artifact promotion requires explicit `ArtifactKind::<Variant>` patterns";

pub(super) fn artifact_promotion_variants(root: &Path) -> Result<(), String> {
    let read = |rel: &str, label: &str| {
        std::fs::read_to_string(root.join(rel))
            .map_err(|err| format!("artifact promotion {label}: {err}"))
    };
    let ir = read("crates/norn-ir/src/lib.rs", "IR")?;
    match std::fs::symlink_metadata(root.join(VERUS_SOURCE)) {
        Ok(meta) if meta.file_type().is_file() => {
            verify_verus_lib_wiring(&ir)?;
            compare_artifact_variants_verus(&read(VERUS_SOURCE, "source")?)
        }
        Ok(_) => Err(format!("{VERUS_SOURCE} must be a regular file")),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            compare_artifact_variants_fstar(&ir, &read(FSTAR_PROOF, "proof")?)
        }
        Err(err) => Err(format!("artifact promotion layout probe: {err}")),
    }
}

fn artifact_kind_variants(syntax: &syn::File) -> Result<Option<BTreeSet<String>>, String> {
    let mut enums = syntax.items.iter().filter_map(|item| {
        let syn::Item::Enum(e) = item else {
            return None;
        };
        (e.ident == "ArtifactKind")
            .then(|| e.variants.iter().map(|v| v.ident.to_string()).collect())
    });
    let first = enums.next();
    if enums.next().is_some() {
        return Err("duplicate ArtifactKind enum declaration".to_owned());
    }
    Ok(first)
}

fn path_last_in(path: &syn::Path, names: &[&str]) -> bool {
    path.segments.last().is_some_and(|seg| {
        let raw = seg.ident.to_string();
        names.contains(&raw.strip_prefix("r#").unwrap_or(&raw))
    })
}

fn verify_verus_lib_wiring(ir: &str) -> Result<(), String> {
    let lib = syn::parse_file(ir).map_err(|err| format!("ArtifactKind syntax: {err}"))?;
    if artifact_kind_variants(&lib)?.is_some() {
        return Err("lib.rs must not define ArtifactKind".to_owned());
    }
    let mut mods = lib.items.iter().filter_map(|item| {
        let syn::Item::Mod(m) = item else { return None };
        (m.ident == "artifact_governance").then_some(m)
    });
    let m = mods
        .next()
        .ok_or("lib.rs missing `mod artifact_governance;`")?;
    if mods.next().is_some()
        || m.content.is_some()
        || m.attrs
            .iter()
            .any(|a| path_last_in(a.path(), &["cfg", "cfg_attr", "path"]))
    {
        return Err(
            "lib.rs `mod artifact_governance;` must be unique, external, unconditional".to_owned(),
        );
    }
    Ok(())
}

fn compare_artifact_variants_verus(src: &str) -> Result<(), String> {
    let syntax = syn::parse_file(src).map_err(|err| format!("ArtifactKind syntax: {err}"))?;
    let expected = artifact_kind_variants(&syntax)?.ok_or("ArtifactKind enum missing")?;
    let actual = parse_verus_match_arms(extract_verus_authority_bearing_body(&syntax)?)?;
    verify_variants(&expected, &actual)
}

fn has_exact_verus_cfg(attrs: &[syn::Attribute]) -> bool {
    let mut saw = false;
    for a in attrs
        .iter()
        .filter(|a| path_last_in(a.path(), &["cfg", "cfg_attr"]))
    {
        if !matches!(a.style, syn::AttrStyle::Outer)
            || !a.path().is_ident("cfg")
            || !a
                .parse_args::<syn::Meta>()
                .is_ok_and(|m| matches!(m, syn::Meta::Path(p) if p.is_ident("verus_keep_ghost")))
        {
            return false;
        }
        saw = true;
    }
    saw
}

fn is_ident(tok: &TT, want: &str) -> bool {
    matches!(tok, TT::Ident(id) if id == want)
}

fn is_punct(tok: &TT, ch: char, spacing: Option<Spacing>) -> bool {
    matches!(tok, TT::Punct(p) if p.as_char() == ch && spacing.is_none_or(|s| p.spacing() == s))
}

fn extract_verus_authority_bearing_body(syntax: &syn::File) -> Result<TokenStream, String> {
    let (mut saw_verus, mut found) = (false, None);
    for item in &syntax.items {
        let syn::Item::Macro(m) = item else { continue };
        if m.ident.is_some()
            || !m.mac.path.is_ident("verus")
            || !matches!(m.mac.delimiter, syn::MacroDelimiter::Brace(_))
            || !has_exact_verus_cfg(&m.attrs)
        {
            continue;
        }
        saw_verus = true;
        let toks: Vec<TT> = m.mac.tokens.clone().into_iter().collect();
        for (i, t) in toks.iter().enumerate() {
            if is_ident(t, "fn")
                && i.checked_add(1)
                    .and_then(|p| toks.get(p))
                    .is_some_and(|n| is_ident(n, "authority_bearing"))
                && found
                    .replace(parse_authority_bearing_decl(&toks, i)?)
                    .is_some()
            {
                return Err("duplicate artifact promotion authority_bearing declaration".to_owned());
            }
        }
    }
    if !saw_verus {
        return Err("missing `#[cfg(verus_keep_ghost)] verus! { ... }` block".to_owned());
    }
    found.ok_or_else(|| "artifact promotion authority_bearing missing".to_owned())
}

fn parse_authority_bearing_decl(tokens: &[TT], fn_idx: usize) -> Result<TokenStream, String> {
    let pub_idx = fn_idx.checked_sub(3).ok_or(SIG_ERR)?;
    let [p, o, s, _, _, params, d, g, r, body, ..] = tokens.get(pub_idx..).ok_or(SIG_ERR)? else {
        return Err(SIG_ERR.to_owned());
    };
    let (TT::Group(params), TT::Group(body)) = (params, body) else {
        return Err(SIG_ERR.to_owned());
    };
    let mut start = pub_idx;
    while let Some(h) = start.checked_sub(2)
        && let Some([hash, TT::Group(br)]) = tokens.get(h..start)
        && is_punct(hash, '#', None)
        && br.delimiter() == Delimiter::Bracket
    {
        let meta = syn::parse2::<syn::Meta>(br.stream()).map_err(|e| format!("attr: {e}"))?;
        if path_last_in(meta.path(), &["cfg", "cfg_attr"]) {
            return Err("authority_bearing must not carry cfg or cfg_attr".to_owned());
        }
        start = h;
    }
    let bad_prev = start
        .checked_sub(1)
        .and_then(|i| tokens.get(i))
        .is_some_and(|t| {
            !matches!(t, TT::Group(gr) if gr.delimiter() == Delimiter::Brace)
                && !is_punct(t, ';', None)
        });
    let pvec: Vec<TT> = params.stream().into_iter().collect();
    let ps = match pvec.split_last() {
        Some((last, rest)) if is_punct(last, ',', None) => rest,
        Some(_) | None => pvec.as_slice(),
    };
    let ok = !bad_prev
        && is_ident(p, "pub")
        && is_ident(o, "open")
        && is_ident(s, "spec")
        && params.delimiter() == Delimiter::Paren
        && matches!(ps, [k, c, t] if is_ident(k, "kind") && is_punct(c, ':', None) && is_ident(t, "ArtifactKind"))
        && is_punct(d, '-', Some(Spacing::Joint))
        && is_punct(g, '>', None)
        && is_ident(r, "bool")
        && body.delimiter() == Delimiter::Brace;
    ok.then(|| body.stream()).ok_or_else(|| SIG_ERR.to_owned())
}

fn parse_verus_match_arms(body_tokens: TokenStream) -> Result<BTreeSet<String>, String> {
    let m = syn::parse2::<syn::ExprMatch>(body_tokens)
        .map_err(|err| format!("authority_bearing body must be `match kind {{ ... }}`: {err}"))?;
    if !m.attrs.is_empty()
        || !matches!(m.expr.as_ref(), syn::Expr::Path(p) if p.attrs.is_empty() && p.qself.is_none() && p.path.is_ident("kind"))
    {
        return Err("artifact promotion requires an explicit match on `kind`".to_owned());
    }
    let mut actual = BTreeSet::new();
    for arm in &m.arms {
        if !arm.attrs.is_empty() || arm.guard.is_some() {
            return Err("artifact promotion match arms must have no attrs or guards".to_owned());
        }
        if !matches!(arm.body.as_ref(), syn::Expr::Lit(l) if l.attrs.is_empty() && matches!(l.lit, syn::Lit::Bool(_)))
        {
            return Err("artifact promotion requires Boolean match arms".to_owned());
        }
        collect_verus_pat(&arm.pat, &mut actual)?;
    }
    Ok(actual)
}

fn collect_verus_pat(pat: &syn::Pat, actual: &mut BTreeSet<String>) -> Result<(), String> {
    if let syn::Pat::Or(pat_or) = pat {
        if !pat_or.attrs.is_empty() {
            return Err(PAT_ERR.to_owned());
        }
        for c in &pat_or.cases {
            collect_verus_pat(c, actual)?;
        }
        return Ok(());
    }
    let syn::Pat::Path(p) = pat else {
        return Err(PAT_ERR.to_owned());
    };
    let mut segs = p.path.segments.iter();
    let (Some(ty), Some(var), None) = (segs.next(), segs.next(), segs.next()) else {
        return Err(PAT_ERR.to_owned());
    };
    if !p.attrs.is_empty()
        || p.qself.is_some()
        || p.path.leading_colon.is_some()
        || ty.ident != "ArtifactKind"
        || !ty.arguments.is_none()
        || !var.arguments.is_none()
    {
        return Err(PAT_ERR.to_owned());
    }
    insert_variant(actual, var.ident.to_string())
}

fn compare_artifact_variants_fstar(ir: &str, proof: &str) -> Result<(), String> {
    let syntax = syn::parse_file(ir).map_err(|err| format!("ArtifactKind syntax: {err}"))?;
    let expected = artifact_kind_variants(&syntax)?.ok_or("ArtifactKind enum missing")?;
    if proof.contains("(*") || proof.contains("*)") {
        return Err("artifact promotion proof contains unsupported block comments".to_owned());
    }
    let lines: Vec<&str> = proof
        .lines()
        .map(|l| l.split_once("//").map_or(l, |(c, _)| c))
        .collect();
    let mut decls = lines.iter().enumerate().filter(|(_, l)| {
        l.strip_prefix("let authority_bearing")
            .and_then(|s| s.chars().next())
            .is_some_and(|c| c.is_whitespace() || c == '(')
    });
    let (start, _) = decls
        .next()
        .ok_or("artifact promotion authority_bearing missing")?;
    if decls.next().is_some() {
        return Err("duplicate artifact promotion authority_bearing declaration".to_owned());
    }
    let func = lines
        .into_iter()
        .skip(start)
        .enumerate()
        .take_while(|(i, l)| *i == 0 || !l.starts_with("let "))
        .map(|(_, l)| l)
        .collect::<Vec<_>>()
        .join("\n");
    let (_, arms) = func
        .split_once("match kind with")
        .ok_or("artifact promotion requires an explicit match on kind")?;
    let mut patterns = arms.split('|');
    if patterns.next().is_none_or(|p| !p.trim().is_empty()) {
        return Err("artifact promotion requires explicit constructor patterns".to_owned());
    }
    let mut actual = BTreeSet::new();
    for arm in patterns {
        let pattern = match arm.split_once("->") {
            Some((pat, out)) if matches!(out.trim(), "true" | "false") => pat,
            Some(_) => return Err("artifact promotion requires Boolean match arms".to_owned()),
            None => arm,
        };
        let name = pattern
            .trim()
            .strip_prefix("ArtifactKind_")
            .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
            .ok_or("artifact promotion requires explicit constructor patterns")?;
        insert_variant(&mut actual, name.to_owned())?;
    }
    verify_variants(&expected, &actual)
}

fn insert_variant(actual: &mut BTreeSet<String>, name: String) -> Result<(), String> {
    if actual.contains(&name) {
        return Err(format!("duplicate artifact promotion variant {name}"));
    }
    actual.insert(name);
    Ok(())
}

fn verify_variants(expected: &BTreeSet<String>, actual: &BTreeSet<String>) -> Result<(), String> {
    if actual != expected {
        return Err(format!(
            "artifact promotion variants differ from IR: missing={:?}, extra={:?}",
            expected.difference(actual).collect::<Vec<_>>(),
            actual.difference(expected).collect::<Vec<_>>()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        artifact_promotion_variants, compare_artifact_variants_fstar as compare_artifact_variants,
        compare_artifact_variants_verus,
    };

    const IR: &str = "pub enum ArtifactKind { Entity, SourceSchema, Law }";
    const PROOF: &str = "let authority_bearing kind =\n  match kind with\n  | ArtifactKind_Entity\n  | ArtifactKind_Law -> true\n  | ArtifactKind_SourceSchema -> false\nlet next = ()\n";

    #[test]
    fn artifact_promotion_covers_the_closed_ir_enum() {
        assert!(compare_artifact_variants(IR, PROOF).is_ok());
    }

    #[test]
    fn artifact_promotion_rejects_new_ir_variants() {
        let ir = "pub enum ArtifactKind { Entity, SourceSchema, Law, Future }";
        assert!(compare_artifact_variants(ir, PROOF).is_err());
    }

    #[test]
    fn artifact_promotion_rejects_removed_ir_variants() {
        assert!(compare_artifact_variants("pub enum ArtifactKind { Entity, Law }", PROOF).is_err());
    }

    #[test]
    fn artifact_promotion_rejects_the_missing_law_case() {
        let proof = PROOF.replace("  | ArtifactKind_Law -> true\n", "");
        assert!(compare_artifact_variants(IR, &proof).is_err());
    }

    #[test]
    fn artifact_promotion_rejects_a_wildcard_instead_of_enumeration() {
        let proof = PROOF.replace("ArtifactKind_Law", "_");
        assert!(compare_artifact_variants(IR, &proof).is_err());
    }

    #[test]
    fn artifact_promotion_rejects_duplicate_variants() {
        let proof = PROOF.replace(
            "ArtifactKind_Law -> true",
            "ArtifactKind_Law\n  | ArtifactKind_Law -> true",
        );
        assert!(compare_artifact_variants(IR, &proof).is_err());
    }

    #[test]
    fn artifact_promotion_rejects_a_commented_function() {
        let proof = format!("(*\n{PROOF}*)\nlet authority_bearing kind = true\nlet next = ()\n");
        assert!(compare_artifact_variants(IR, &proof).is_err());
    }

    #[test]
    fn artifact_promotion_rejects_a_commented_constructor() {
        let proof = PROOF.replace(
            "  | ArtifactKind_Law -> true",
            "(*\n  | ArtifactKind_Law -> true\n*)",
        );
        assert!(compare_artifact_variants(IR, &proof).is_err());
    }

    #[test]
    fn artifact_promotion_ignores_similarly_named_functions() {
        let reference = PROOF.replace("let authority_bearing ", "let authority_bearing_reference ");
        let wildcard = "let authority_bearing kind = match kind with | _ -> true\nlet next = ()";
        assert!(compare_artifact_variants(IR, &format!("{reference}{wildcard}")).is_err());
    }

    #[test]
    fn artifact_promotion_rejects_same_line_wildcards_and_duplicates() {
        for extra in ["_", "ArtifactKind_Law"] {
            let proof = PROOF.replace("-> true", &format!("-> true | {extra} -> true"));
            assert!(compare_artifact_variants(IR, &proof).is_err());
        }
    }

    const VERUS_17_SOURCE: &str = "\
pub enum ArtifactKind {
    Entity,
    Relation,
    Action,
    Rule,
    Connector,
    Widget,
    Customization,
    Setting,
    BusinessPack,
    Policy,
    Derived,
    SourceSchema,
    Mapping,
    Law,
    DefinedConcept,
    Labels,
    Model,
}

#[cfg(verus_keep_ghost)]
verus! {
    /// Closed classification of all 17 `ArtifactKind` variants.
    pub open spec fn authority_bearing(kind: ArtifactKind) -> bool {
        match kind {
            ArtifactKind::Entity
            | ArtifactKind::DefinedConcept
            | ArtifactKind::Labels
            | ArtifactKind::Model
            | ArtifactKind::Relation
            | ArtifactKind::Action
            | ArtifactKind::Rule
            | ArtifactKind::Law
            | ArtifactKind::Connector
            | ArtifactKind::Widget
            | ArtifactKind::Customization
            | ArtifactKind::Setting
            | ArtifactKind::BusinessPack
            | ArtifactKind::Policy
            | ArtifactKind::Derived
            | ArtifactKind::Mapping => true,
            ArtifactKind::SourceSchema => false,
        }
    }

    proof fn active_authority_bearing_rejected(kind: ArtifactKind)
        requires authority_bearing(kind),
        ensures true,
    {}
}
";

    #[test]
    fn verus_artifact_promotion_covers_the_closed_17_variant_ir_enum() {
        assert!(compare_artifact_variants_verus(VERUS_17_SOURCE).is_ok());
    }

    #[test]
    fn verus_artifact_promotion_rejects_missing_and_extra_variants() {
        let missing_arm = VERUS_17_SOURCE.replace("            | ArtifactKind::Law\n", "");
        assert!(compare_artifact_variants_verus(&missing_arm).is_err());

        let new_ir_variant = VERUS_17_SOURCE.replace("    Model,\n", "    Model,\n    Future,\n");
        assert!(compare_artifact_variants_verus(&new_ir_variant).is_err());

        let extra_match_variant = VERUS_17_SOURCE.replace("    Law,\n", "");
        assert!(compare_artifact_variants_verus(&extra_match_variant).is_err());
    }

    #[test]
    fn verus_artifact_promotion_rejects_wildcards_duplicates_guards_and_non_bools() {
        let wildcard = VERUS_17_SOURCE.replace("ArtifactKind::Law", "_");
        assert!(compare_artifact_variants_verus(&wildcard).is_err());

        for extra in ["_", "ArtifactKind::Law", "other"] {
            let same_line =
                VERUS_17_SOURCE.replace("=> true,", &format!("=> true, {extra} => true,"));
            assert!(compare_artifact_variants_verus(&same_line).is_err());
        }

        let duplicate = VERUS_17_SOURCE.replace(
            "| ArtifactKind::Law\n",
            "| ArtifactKind::Law\n            | ArtifactKind::Law\n",
        );
        assert!(compare_artifact_variants_verus(&duplicate).is_err());

        let block_comment = VERUS_17_SOURCE.replace(
            "| ArtifactKind::Law\n",
            "/*\n            | ArtifactKind::Law\n*/\n",
        );
        assert!(compare_artifact_variants_verus(&block_comment).is_err());

        let non_bool = VERUS_17_SOURCE.replace("=> false,", "=> other,");
        assert!(compare_artifact_variants_verus(&non_bool).is_err());

        let guarded = VERUS_17_SOURCE.replace(
            "ArtifactKind::SourceSchema => false,",
            "ArtifactKind::SourceSchema if true => false,",
        );
        assert!(compare_artifact_variants_verus(&guarded).is_err());

        let wrong_scrutinee = VERUS_17_SOURCE.replace("match kind {", "match other {");
        assert!(compare_artifact_variants_verus(&wrong_scrutinee).is_err());

        let extra_stmt =
            VERUS_17_SOURCE.replace("match kind {", "let _ = kind;\n        match kind {");
        assert!(compare_artifact_variants_verus(&extra_stmt).is_err());

        let nested_match = VERUS_17_SOURCE.replace(
            "ArtifactKind::SourceSchema => false,",
            "ArtifactKind::SourceSchema => match kind { _ => false },",
        );
        assert!(compare_artifact_variants_verus(&nested_match).is_err());
    }

    #[test]
    fn verus_artifact_promotion_rejects_decoys_duplicates_and_bad_macro_or_fn_headers() {
        let decoy_literal = format!(
            "pub enum ArtifactKind {{ Entity, SourceSchema }}\nconst DECOY: &str = r#\"{VERUS_17_SOURCE}\"#;\n"
        );
        assert!(compare_artifact_variants_verus(&decoy_literal).is_err());

        let decoy_in_verus = "\
pub enum ArtifactKind { Entity, SourceSchema }
#[cfg(verus_keep_ghost)]
verus! {
    const DECOY: &str = \"pub open spec fn authority_bearing(kind: ArtifactKind) -> bool { match kind { ArtifactKind::Entity => true, ArtifactKind::SourceSchema => false } }\";
    pub open spec fn authority_bearing(kind: ArtifactKind) -> bool { match kind { _ => true } }
}
";
        assert!(compare_artifact_variants_verus(decoy_in_verus).is_err());

        let unguarded_macro = VERUS_17_SOURCE.replace("#[cfg(verus_keep_ghost)]\n", "");
        assert!(compare_artifact_variants_verus(&unguarded_macro).is_err());

        let disabled_macro = VERUS_17_SOURCE.replace(
            "#[cfg(verus_keep_ghost)]\n",
            "#[cfg(false)]\n#[cfg(verus_keep_ghost)]\n",
        );
        assert!(compare_artifact_variants_verus(&disabled_macro).is_err());

        let disabled_fn = VERUS_17_SOURCE.replace(
            "pub open spec fn authority_bearing",
            "#[cfg(false)]\n    pub open spec fn authority_bearing",
        );
        assert!(compare_artifact_variants_verus(&disabled_fn).is_err());

        let closed_fn = VERUS_17_SOURCE.replace(
            "pub open spec fn authority_bearing",
            "pub closed spec fn authority_bearing",
        );
        assert!(compare_artifact_variants_verus(&closed_fn).is_err());

        let with_recommends =
            VERUS_17_SOURCE.replace("-> bool {", "-> bool\n        recommends true,\n    {");
        assert!(compare_artifact_variants_verus(&with_recommends).is_err());

        let similar_plus_wildcard = VERUS_17_SOURCE
            .replace(
                "pub open spec fn authority_bearing(",
                "pub open spec fn authority_bearing_reference(",
            )
            .replace(
                "proof fn active_authority_bearing_rejected",
                "pub open spec fn authority_bearing(kind: ArtifactKind) -> bool { match kind { _ => true } }\n    proof fn active_authority_bearing_rejected",
            );
        assert!(compare_artifact_variants_verus(&similar_plus_wildcard).is_err());

        let duplicate_decl = VERUS_17_SOURCE.replace(
            "proof fn active_authority_bearing_rejected",
            "pub open spec fn authority_bearing(kind: ArtifactKind) -> bool { match kind { ArtifactKind::Entity => true } }\n    proof fn active_authority_bearing_rejected",
        );
        assert!(compare_artifact_variants_verus(&duplicate_decl).is_err());
    }

    #[test]
    fn artifact_promotion_variants_dispatches_legacy_and_verus_layouts() {
        let legacy = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(legacy.path().join("crates/norn-ir/src")).expect("mkdir");
        std::fs::create_dir_all(
            legacy
                .path()
                .join("verification/stripmine-2026-08-22/proofs/fstar"),
        )
        .expect("mkdir");
        std::fs::write(legacy.path().join("crates/norn-ir/src/lib.rs"), IR).expect("write");
        std::fs::write(
            legacy
                .path()
                .join("verification/stripmine-2026-08-22/proofs/fstar/ArtifactPromotionProof.fst"),
            PROOF,
        )
        .expect("write");
        assert!(artifact_promotion_variants(legacy.path()).is_ok());

        let verus = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(verus.path().join("crates/norn-ir/src")).expect("mkdir");
        std::fs::write(
            verus
                .path()
                .join("crates/norn-ir/src/artifact_governance.rs"),
            VERUS_17_SOURCE,
        )
        .expect("write");

        for valid_lib in [
            "mod artifact_governance;\npub use artifact_governance::ArtifactKind;\n",
            "/// Governance.\n#[allow(dead_code)]\npub mod artifact_governance;\n",
        ] {
            std::fs::write(verus.path().join("crates/norn-ir/src/lib.rs"), valid_lib)
                .expect("write");
            assert!(
                artifact_promotion_variants(verus.path()).is_ok(),
                "{valid_lib}"
            );
        }

        let conflicting = format!("mod artifact_governance;\n{IR}\n");
        for invalid_lib in [
            conflicting.as_str(),
            "",
            "mod artifact_governance {}\n",
            "#[cfg(feature = \"extra\")]\nmod artifact_governance;\n",
            "#[cfg_attr(all(), path = \"other.rs\")]\nmod artifact_governance;\n",
            "#[path = \"other.rs\"]\nmod artifact_governance;\n",
            "#[cfg(test)]\nmod artifact_governance;\nmod artifact_governance;\n",
        ] {
            std::fs::write(verus.path().join("crates/norn-ir/src/lib.rs"), invalid_lib)
                .expect("write");
            assert!(
                artifact_promotion_variants(verus.path()).is_err(),
                "{invalid_lib}"
            );
        }

        // A directory at artifact_governance.rs must fail closed, never fall back to F*.
        let dir_case = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(
            dir_case
                .path()
                .join("crates/norn-ir/src/artifact_governance.rs"),
        )
        .expect("mkdir");
        std::fs::write(dir_case.path().join("crates/norn-ir/src/lib.rs"), IR).expect("write");
        let err = artifact_promotion_variants(dir_case.path()).expect_err("directory refused");
        assert!(err.contains("must be a regular file"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn artifact_promotion_variants_refuses_symlinked_verus_source() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("crates/norn-ir/src")).expect("mkdir");
        std::fs::write(
            dir.path().join("crates/norn-ir/src/lib.rs"),
            "mod artifact_governance;\n",
        )
        .expect("write");
        let target = dir.path().join("outside.rs");
        std::fs::write(&target, VERUS_17_SOURCE).expect("write");
        let link = dir.path().join("crates/norn-ir/src/artifact_governance.rs");
        symlink(&target, &link).expect("symlink");

        let err = artifact_promotion_variants(dir.path()).expect_err("symlink refused");
        assert!(err.contains("must be a regular file"), "{err}");

        // A dangling symlink must also fail closed rather than falling back to F*.
        std::fs::remove_file(&target).expect("remove target");
        let err = artifact_promotion_variants(dir.path()).expect_err("dangling symlink refused");
        assert!(err.contains("must be a regular file"), "{err}");
    }
}
