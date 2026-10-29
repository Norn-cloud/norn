//! Public-core CI: candidate checks and base-owned policy over candidate data.
pub mod git;
pub mod provenance;
pub mod yaml;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
type Obligations = BTreeMap<String, BTreeSet<String>>;

/// Validate every workflow using only candidate data, plus exact provenance.
/// A fork never runs on a self-hosted runner or receives a repository secret.
#[must_use]
pub fn run(root: &Path, base: &Path) -> Vec<String> {
    let mut errors = provenance::check_against(root, base).errors();
    if !errors.is_empty() {
        return errors;
    }
    if let Err(error) = retained_obligations(root, base) {
        errors.push(error);
    }
    if let Err(error) = artifact_promotion_variants(root) {
        errors.push(error);
    }
    // The base-owned lane pins the complete workflow contract, including permissions,
    // triggers, credential-free checkout and every required test command.
    let previous = match git::tracked(base) {
        Ok(files) => files,
        Err(error) => return vec![error],
    };
    let workflow = |path: &&String| path.starts_with(".github/workflows/");
    let old: BTreeSet<_> = previous.iter().filter(workflow).collect();
    let current = match git::tracked(root) {
        Ok(files) => files,
        Err(error) => return vec![error],
    };
    let new: BTreeSet<_> = current.iter().filter(workflow).collect();
    if old != new {
        errors.push("workflow inventory differs from the trusted base".to_owned());
    }
    for path in old.intersection(&new) {
        match (
            std::fs::read(base.join(path)),
            std::fs::read(root.join(path)),
        ) {
            (Ok(before), Ok(after)) if before == after => {}
            _ => errors.push(format!(
                "{path}: workflow contract differs from the trusted base"
            )),
        }
    }
    for path in [
        "scripts/ci/core.sh",
        "scripts/ci/validate-runner-policy.py",
        "scripts/proof_enrollment.py",
        "verification/coverage-v1/validate.py",
        "scripts/verify-promoted-subsystems.sh",
        "scripts/check-proof-holes.py",
        "crates/norn-skeleton-guard/baseline.json",
        "crates/norn-checker/tests/non_code_literals.baseline.json",
    ] {
        match (
            std::fs::read(base.join(path)),
            std::fs::read(root.join(path)),
        ) {
            (Ok(before), Ok(after)) if before == after => {}
            _ => errors.push(format!(
                "{path}: gate contract differs from the trusted base"
            )),
        }
    }
    let files = match git::tracked(root) {
        Ok(files) => files,
        Err(error) => return vec![error],
    };
    for path in files
        .iter()
        .filter(|path| path.starts_with(".github/workflows/"))
    {
        let result = std::fs::read_to_string(root.join(path))
            .map_err(|e| e.to_string())
            .and_then(|text| yaml::parse(&text));
        match result {
            Ok(doc) => {
                if let Some(yaml::Value::Map(jobs)) = doc.get("jobs") {
                    for (name, job) in jobs {
                        if !matches!(
                            job.get("runs-on").and_then(yaml::Value::as_str),
                            Some("ubuntu-latest" | "ubuntu-24.04")
                        ) {
                            errors.push(format!(
                                "{path}::{name}: only GitHub-hosted runners are admitted"
                            ));
                        }
                    }
                } else {
                    errors.push(format!("{path}: missing jobs"));
                }
            }
            Err(error) => errors.push(format!("{path}: {error}")),
        }
    }
    errors
}

fn retained_obligations(root: &Path, base: &Path) -> Result<(), String> {
    let path = "verification/coverage-v1/coverage.json";
    let load = |directory: &Path| -> Result<serde_json::Value, String> {
        let bytes = std::fs::read(directory.join(path)).map_err(|error| error.to_string())?;
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())
    };
    let previous = load(base)?;
    let current = load(root)?;
    let subsystems = |document: &serde_json::Value| -> Result<Obligations, String> {
        document
            .get("subsystems")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "missing source enrollment".to_owned())?
            .iter()
            .map(|subsystem| {
                let id = subsystem
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| "missing subsystem id".to_owned())?;
                let obligations = subsystem
                    .get("obligations")
                    .and_then(serde_json::Value::as_array)
                    .ok_or_else(|| "missing subsystem obligations".to_owned())?
                    .iter()
                    .map(|id| {
                        id.as_str()
                            .map(str::to_owned)
                            .ok_or_else(|| "invalid obligation id".to_owned())
                    })
                    .collect::<Result<BTreeSet<_>, _>>()?;
                Ok((id.to_owned(), obligations))
            })
            .collect()
    };
    let after = subsystems(&current)?;
    for (id, required) in subsystems(&previous)? {
        if !after
            .get(&id)
            .is_some_and(|actual| required.is_subset(actual))
        {
            return Err(format!("{id}: retained core obligations were removed"));
        }
    }
    Ok(())
}

fn artifact_promotion_variants(root: &Path) -> Result<(), String> {
    let ir = std::fs::read_to_string(root.join("crates/norn-ir/src/lib.rs"))
        .map_err(|error| format!("artifact promotion IR: {error}"))?;
    let proof = std::fs::read_to_string(
        root.join("verification/stripmine-2026-08-22/proofs/fstar/ArtifactPromotionProof.fst"),
    )
    .map_err(|error| format!("artifact promotion proof: {error}"))?;
    compare_artifact_variants(&ir, &proof)
}

fn compare_artifact_variants(ir: &str, proof: &str) -> Result<(), String> {
    let syntax = syn::parse_file(ir).map_err(|error| format!("ArtifactKind syntax: {error}"))?;
    let expected = syntax
        .items
        .into_iter()
        .find_map(|item| match item {
            syn::Item::Enum(item) if item.ident == "ArtifactKind" => Some(
                item.variants
                    .into_iter()
                    .map(|variant| variant.ident.to_string())
                    .collect::<BTreeSet<_>>(),
            ),
            _ => None,
        })
        .ok_or("ArtifactKind enum missing")?;
    if proof.contains("(*") || proof.contains("*)") {
        return Err("artifact promotion proof contains unsupported block comments".into());
    }
    let uncommented = proof
        .lines()
        .map(|line| line.split_once("//").map_or(line, |(code, _)| code))
        .collect::<Vec<_>>()
        .join("\n");
    let mut declarations = uncommented.lines().enumerate().filter(|(_, line)| {
        line.strip_prefix("let authority_bearing")
            .and_then(|suffix| suffix.chars().next())
            .is_some_and(|c| c.is_whitespace() || c == '(')
    });
    let (start, _) = declarations
        .next()
        .ok_or("artifact promotion authority_bearing missing")?;
    if declarations.next().is_some() {
        return Err("duplicate artifact promotion authority_bearing declaration".into());
    }
    let function = uncommented
        .lines()
        .skip(start)
        .enumerate()
        .take_while(|(index, line)| *index == 0 || !line.starts_with("let "))
        .map(|(_, line)| line)
        .collect::<Vec<_>>()
        .join("\n");
    let (_, arms) = function
        .split_once("match kind with")
        .ok_or("artifact promotion requires an explicit match on kind")?;
    let mut patterns = arms.split('|');
    if patterns.next().is_none_or(|prefix| !prefix.trim().is_empty()) {
        return Err("artifact promotion requires explicit constructor patterns".into());
    }
    let mut actual = BTreeSet::new();
    for arm in patterns {
        let pattern = match arm.split_once("->") {
            Some((pattern, outcome)) if matches!(outcome.trim(), "true" | "false") => pattern,
            Some(_) => return Err("artifact promotion requires Boolean match arms".into()),
            None => arm,
        };
        let name = pattern
            .trim()
            .strip_prefix("ArtifactKind_")
            .filter(|name| {
                !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            })
            .ok_or("artifact promotion requires explicit constructor patterns")?;
        if !actual.insert(name.to_owned()) {
            return Err(format!("duplicate artifact promotion variant {name}"));
        }
    }
    if actual != expected {
        return Err(format!(
            "artifact promotion variants differ from IR: missing={:?}, extra={:?}",
            expected.difference(&actual).collect::<Vec<_>>(),
            actual.difference(&expected).collect::<Vec<_>>()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::compare_artifact_variants;

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
}
