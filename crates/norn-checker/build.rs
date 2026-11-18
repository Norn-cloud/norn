//! Compile code records and mandatory guidance into one diagnostic registry,
//! and project the fixed, admitted inheritance-step AST into Ascent syntax.

use std::{collections::BTreeMap, fmt::Write as _, fs, path::Path};

#[cfg(not(test))]
use norn_datalog::{Budget, Literal, Mode, RelationKind, Term, overlap, validate};
#[cfg(not(test))]
use quote::{format_ident, quote};

/// Validate every entry and render the sorted static registry.
///
/// # Errors
/// Refuses unknown files, malformed entries, duplicate identities and invalid metadata.
pub(crate) fn generate(directory: &Path, guidance_path: &Path) -> Result<String, String> {
    if !directory
        .symlink_metadata()
        .map_err(|e| e.to_string())?
        .is_dir()
    {
        return Err("diagnostic registry is not a regular directory".to_owned());
    }
    let mut guidance = read_guidance(guidance_path)?;
    let mut files = fs::read_dir(directory)
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    files.sort_by_key(std::fs::DirEntry::file_name);
    if files.is_empty() {
        return Err("diagnostic registry is empty".to_owned());
    }
    let mut result = String::from(
        "/// Every registered diagnostic, sorted by code.\npub static REGISTRY: &[CodeEntry] = &[\n",
    );
    let strings = regex_lite::Regex::new(r#""(?:\\.|[^"\\])*""#).map_err(|e| e.to_string())?;
    for file in files {
        if !file.file_type().map_err(|e| e.to_string())?.is_file() {
            return Err("diagnostic entry is not a regular file".to_owned());
        }
        let text = fs::read_to_string(file.path()).map_err(|e| e.to_string())?;
        let entry: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        let object = entry
            .as_object()
            .ok_or("diagnostic entry must be an object")?;
        // Five string keys and five string values are exactly ten JSON string tokens.
        // Extra tokens expose duplicate keys that Value would otherwise discard.
        if object.len() != 5 || strings.find_iter(&text).count() != 10 {
            return Err(
                "diagnostic entry must contain exactly five unique string fields".to_owned(),
            );
        }
        let field = |key| {
            object
                .get(key)
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| format!("missing or non-string diagnostic field: {key}"))
        };
        let code = field("code")?;
        let tier = field("tier")?;
        let severity = field("severity")?;
        let message = field("message")?;
        let since = field("since")?;
        let (fix, no_fix) = guidance
            .remove(code)
            .ok_or_else(|| format!("{code} requires a fix or no_fix reason"))?;

        let segments: Vec<_> = code.split('.').collect();
        if segments.len() != 3
            || segments.iter().any(|part| {
                part.is_empty()
                    || !part
                        .bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
            })
            || file.file_name().to_str() != Some(format!("{code}.json").as_str())
        {
            return Err("diagnostic filename and stable dotted identity must match".to_owned());
        }
        if !matches!(tier, "T0" | "T1" | "T2" | "T3" | "T4" | "T5" | "T6" | "T7")
            || message.trim().is_empty()
            || since.trim().is_empty()
        {
            return Err("invalid tier, message or since version".to_owned());
        }
        let severity = match severity {
            "error" => "Error",
            "warning" => "Warning",
            "info" => "Info",
            _ => return Err("invalid diagnostic severity".to_owned()),
        };
        writeln!(result, "CodeEntry {{ code: {code:?}, tier: Tier::{tier}, severity: DiagnosticSeverity::{severity}, message: {message:?}, since: {since:?}, fix: {fix}, no_fix: {no_fix} }},").map_err(|e| e.to_string())?;
    }
    if !guidance.is_empty() {
        return Err("guidance names an unregistered diagnostic".to_owned());
    }
    result.push_str("];\n");
    Ok(result)
}

fn read_guidance(path: &Path) -> Result<BTreeMap<String, (String, String)>, String> {
    let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let entries: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let entries = entries.as_array().ok_or("guidance must be an array")?;
    let strings = regex_lite::Regex::new(r#""(?:\\.|[^"\\])*""#).map_err(|e| e.to_string())?;
    if strings.find_iter(&text).count() != entries.len().saturating_mul(4) {
        return Err("guidance fields must be unique strings".to_owned());
    }
    let mut result = BTreeMap::new();
    for entry in entries {
        let fields = entry.as_object().ok_or("guidance must be an object")?;
        if fields.len() != 2 {
            return Err("guidance requires code and exactly one fix or no_fix".to_owned());
        }
        let code = fields
            .get("code")
            .and_then(serde_json::Value::as_str)
            .ok_or("guidance requires code")?;
        let (key, value) = match (fields.get("fix"), fields.get("no_fix")) {
            (Some(value), None) => ("fix", value),
            (None, Some(value)) => ("no_fix", value),
            _ => return Err("guidance requires exactly one fix or no_fix".to_owned()),
        };
        let value = value
            .as_str()
            .filter(|value| !value.trim().is_empty())
            .ok_or("guidance must not be blank")?;
        let literal = format!("Some({value:?})");
        let pair = if key == "fix" {
            (literal, "None".to_owned())
        } else {
            ("None".to_owned(), literal)
        };
        if result.insert(code.to_owned(), pair).is_some() {
            return Err("duplicate guidance identity".to_owned());
        }
    }
    Ok(result)
}

#[cfg(not(test))]
#[allow(
    clippy::too_many_lines,
    reason = "one bounded fixed-AST admission/projection boundary"
)]
fn emit_inheritance_ascent() -> Result<(), Box<dyn std::error::Error>> {
    let program = overlap::registered_inheritance_step_v1();
    // The same admission gate is used by every backend. This tiny profile also
    // bounds the build adapter before it allocates token sequences.
    let plan = validate(
        &program,
        Budget {
            max_predicates: 5,
            max_rules: 2,
            max_atoms: 6,
            max_terms: 16,
            max_arity: 3,
            max_rule_variables: 3,
            max_body_atoms: 2,
            max_text_bytes: 1_024,
            max_work: 20_000,
        },
    )?;
    let refused = || std::io::Error::other("unsupported fixed inheritance AST schema");
    let schema = [
        ("query_root", 2, RelationKind::Input),
        ("frontier", 2, RelationKind::Input),
        ("extends", 3, RelationKind::Input),
        ("next", 2, RelationKind::Derived),
        ("inheritance_cycle", 2, RelationKind::Derived),
    ];
    if program.predicates.len() != schema.len()
        || program.rules.len() != 2
        || plan.groups.iter().any(|group| group.recursive)
        || !program
            .predicates
            .iter()
            .zip(schema)
            .all(|(predicate, (name, arity, kind))| {
                predicate.name == name && predicate.arity == arity && predicate.kind == kind
            })
    {
        return Err(refused().into());
    }
    // No arbitrary source, general-purpose compiler, constants, negation or
    // executable callbacks. Every admitted column is a catalog-local usize ID.
    let identifier = |name: &str| {
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            || name.as_bytes().first().is_none_or(u8::is_ascii_digit)
        {
            Err(refused())
        } else {
            Ok(format_ident!("{}", name))
        }
    };
    let atom_tokens = |atom: &norn_datalog::Atom| {
        let name = identifier(&atom.predicate)?;
        let arguments = atom
            .terms
            .iter()
            .map(|term| match term {
                Term::Variable(variable) => identifier(variable),
                Term::Constant(_) | Term::Aggregate(_) => Err(refused()),
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok::<_, std::io::Error>(quote! { #name(#(#arguments),*) })
    };
    let relations = program
        .predicates
        .iter()
        .map(|predicate| {
            let name = identifier(&predicate.name)?;
            let columns: Vec<_> = (0..predicate.arity).map(|_| quote! { usize }).collect();
            Ok::<_, std::io::Error>(quote! { relation #name(#(#columns),*); })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let rules = program
        .rules
        .iter()
        .map(|rule| {
            let head = atom_tokens(&rule.head)?;
            let body = rule
                .body
                .iter()
                .map(|literal| match literal {
                    Literal::Positive(atom) => atom_tokens(atom),
                    Literal::Negative(_) => Err(refused()),
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, std::io::Error>(quote! { #head <-- #(#body),*; })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let cycle_rule_id = &program
        .rules
        .iter()
        .find(|rule| rule.head.predicate == "inheritance_cycle" && rule.mode == Mode::Validation)
        .ok_or_else(refused)?
        .id;
    let output = quote! {
        pub(crate) const CYCLE_RULE_ID: &str = #cycle_rule_id;
        ascent::ascent! {
            pub(super) struct RegisteredInheritanceStep;
            #(#relations)*
            #(#rules)*
        }
    }
    .to_string();
    for forbidden in ["Instant", "web_time", "run_timeout"] {
        if output.contains(forbidden) {
            return Err(
                std::io::Error::other("clock token in generated inheritance source").into(),
            );
        }
    }
    let directory = std::path::PathBuf::from(std::env::var_os("OUT_DIR").ok_or_else(refused)?);
    std::fs::write(directory.join("inheritance_ascent.rs"), output)?;
    Ok(())
}

#[cfg(not(test))]
fn main() -> Result<(), String> {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=conformance/diagnostic-codes");
    emit_inheritance_ascent().map_err(|e| e.to_string())?;
    println!("cargo:rerun-if-changed=conformance/diagnostic-guidance.json");
    let generated = generate(
        Path::new("conformance/diagnostic-codes"),
        Path::new("conformance/diagnostic-guidance.json"),
    )?;
    let output = std::env::var("OUT_DIR").map_err(|e| e.to_string())?;
    fs::write(Path::new(&output).join("diagnostic_registry.rs"), generated)
        .map_err(|e| e.to_string())
}
