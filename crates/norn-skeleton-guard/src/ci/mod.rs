//! Public-core CI: candidate checks and base-owned policy over candidate data.
pub mod git;
pub mod provenance;
pub mod yaml;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
type Obligations = BTreeMap<String, BTreeSet<String>>;

/// The only repository whose own branches may change workflows or gate files.
const MAINTAINED_REPOSITORY: &str = "Norn-cloud/norn";

/// Validate every workflow using only candidate data, plus exact provenance.
/// A fork never runs on a self-hosted runner or receives a repository secret.
#[must_use]
pub fn run(root: &Path, base: &Path) -> Vec<String> {
    run_with_evidence(root, base, None)
}

/// [`run`] for the trusted lane: workflow and gate contract changes are admitted
/// only with `evidence` that an admin approved the exact candidate commit
/// (see [`maintainer_approval`]). Every other check is unchanged.
#[must_use]
pub fn run_with_evidence(root: &Path, base: &Path, evidence: Option<&Path>) -> Vec<String> {
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
    let mut contract = Vec::new();
    if old != new {
        contract.push("workflow inventory differs from the trusted base".to_owned());
    }
    for path in old.intersection(&new) {
        match (
            std::fs::read(base.join(path)),
            std::fs::read(root.join(path)),
        ) {
            (Ok(before), Ok(after)) if before == after => {}
            _ => contract.push(format!(
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
            _ => contract.push(format!(
                "{path}: gate contract differs from the trusted base"
            )),
        }
    }
    let approval = evidence
        .filter(|_| !contract.is_empty())
        .map(|path| approval_of(path, root));
    errors.extend(contract_errors(contract, approval));
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

/// Contract changes stay errors unless an admin approved the exact head.
fn contract_errors(contract: Vec<String>, approval: Option<Result<String, String>>) -> Vec<String> {
    match approval {
        _ if contract.is_empty() => contract,
        Some(Ok(_)) => Vec::new(),
        Some(Err(reason)) => contract
            .into_iter()
            .chain([format!("maintainer path: {reason}")])
            .collect(),
        None => contract,
    }
}

/// [`maintainer_approval`] of the evidence file for the candidate checkout's HEAD.
fn approval_of(evidence: &Path, root: &Path) -> Result<String, String> {
    let bytes = std::fs::read(evidence).map_err(|error| format!("maintainer evidence: {error}"))?;
    let document =
        serde_json::from_slice(&bytes).map_err(|error| format!("maintainer evidence: {error}"))?;
    maintainer_approval(&document, &git::head(root)?)
}

/// The admin who commented `/approve-gates <head>` for the exact candidate commit,
/// from the API evidence that the base-owned trusted workflow records
/// (`repository`, `head_repository`, `head_sha`, and the pull request's
/// `/approve-gates` `comments` with each author's `permission`). Edited comments
/// do not count; deleted ones are gone from the API. Fork heads stay data-only:
/// they never change workflows or gates, approved or not.
///
/// # Errors
/// Returns why the candidate is not admitted: a fork or unknown head repository,
/// evidence for another commit, malformed evidence, or no unedited admin comment
/// naming the candidate head.
pub fn maintainer_approval(
    evidence: &serde_json::Value,
    candidate_head: &str,
) -> Result<String, String> {
    let text = |value: &serde_json::Value, key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    if text(evidence, "repository").as_deref() != Some(MAINTAINED_REPOSITORY)
        || text(evidence, "head_repository").as_deref() != Some(MAINTAINED_REPOSITORY)
    {
        return Err(format!(
            "only {MAINTAINED_REPOSITORY} branches may change workflows or gates"
        ));
    }
    let head = text(evidence, "head_sha")
        .filter(|sha| {
            sha.len() == 40 && sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        })
        .ok_or("maintainer evidence names no exact head commit")?;
    if head != candidate_head {
        return Err(format!(
            "evidence is for {head}, the candidate is {candidate_head}"
        ));
    }
    let command = format!("/approve-gates {head}");
    let mut approver = None;
    for comment in evidence
        .get("comments")
        .and_then(serde_json::Value::as_array)
        .ok_or("maintainer evidence has no comments")?
    {
        let (Some(user), Some(body), Some(created), Some(updated)) = (
            text(comment, "user"),
            text(comment, "body"),
            text(comment, "created_at"),
            text(comment, "updated_at"),
        ) else {
            return Err("maintainer evidence has a malformed comment".to_owned());
        };
        let admin = text(comment, "permission").as_deref() == Some("admin");
        if approver.is_none() && admin && created == updated && body.trim() == command {
            approver = Some(user);
        }
    }
    approver.ok_or_else(|| format!("no admin commented `{command}` unedited"))
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
    if patterns
        .next()
        .is_none_or(|prefix| !prefix.trim().is_empty())
    {
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
    use super::{compare_artifact_variants, contract_errors, maintainer_approval};
    use serde_json::{Value, json};

    const HEAD: &str = "1111111111111111111111111111111111111111";
    const OLDER: &str = "2222222222222222222222222222222222222222";
    const AT: &str = "2026-10-30T12:00:00Z";

    fn comment(user: &str, body: &str, permission: &str) -> Value {
        json!({"user": user, "body": body, "created_at": AT, "updated_at": AT, "permission": permission})
    }

    fn approval(comments: &[Value]) -> Result<String, String> {
        let evidence = json!({"repository": "Norn-cloud/norn", "head_repository": "Norn-cloud/norn", "head_sha": HEAD, "comments": comments});
        maintainer_approval(&evidence, HEAD)
    }

    #[test]
    fn maintainer_approval_needs_an_unedited_admin_comment_on_the_exact_head() {
        let exact = format!("/approve-gates {HEAD}");
        assert_eq!(
            approval(&[comment("owner", &format!("{exact}\n"), "admin")]),
            Ok("owner".to_owned())
        );
        for permission in ["write", "maintain", "triage", "read", "none"] {
            assert!(approval(&[comment("someone", &exact, permission)]).is_err());
        }
        for body in [
            format!("/approve-gates {OLDER}"),
            "/approve-gates 1111111".to_owned(),
            format!("lgtm {exact}"),
            "/approve-gates".to_owned(),
        ] {
            assert!(
                approval(&[comment("owner", &body, "admin")]).is_err(),
                "{body}"
            );
        }
        let mut edited = comment("owner", &exact, "admin");
        edited["updated_at"] = json!("2026-10-30T12:05:00Z");
        assert!(approval(&[edited.clone()]).is_err());
        assert!(approval(&[edited, comment("owner", &exact, "admin")]).is_ok());
        assert!(approval(&[]).is_err());
        assert!(approval(&[json!({"user": "owner", "body": exact})]).is_err());
    }

    #[test]
    fn maintainer_approval_binds_repository_and_candidate() {
        let comments = [comment("owner", &format!("/approve-gates {HEAD}"), "admin")];
        let evidence = |repository: &str, head_repository: Value, head: &str| json!({"repository": repository, "head_repository": head_repository, "head_sha": head, "comments": comments});
        assert!(
            maintainer_approval(
                &evidence("Norn-cloud/norn", json!("Norn-cloud/norn"), HEAD),
                HEAD
            )
            .is_ok()
        );
        for document in [
            evidence("Norn-cloud/norn", json!("fork/norn"), HEAD),
            evidence("Norn-cloud/norn", Value::Null, HEAD),
            evidence("fork/norn", json!("fork/norn"), HEAD),
        ] {
            assert!(maintainer_approval(&document, HEAD).is_err());
        }
        for malformed in ["A".repeat(40), "1".repeat(39), String::new()] {
            let document = evidence("Norn-cloud/norn", json!("Norn-cloud/norn"), &malformed);
            assert_eq!(
                maintainer_approval(&document, &malformed),
                Err("maintainer evidence names no exact head commit".to_owned())
            );
        }
        assert!(
            maintainer_approval(
                &evidence("Norn-cloud/norn", json!("Norn-cloud/norn"), HEAD),
                OLDER
            )
            .is_err()
        );
    }

    #[test]
    fn contract_changes_need_an_approval() {
        let contract =
            || vec!["scripts/ci/core.sh: gate contract differs from the trusted base".to_owned()];
        assert_eq!(contract_errors(contract(), None), contract());
        assert!(contract_errors(contract(), Some(Ok("maintainer".to_owned()))).is_empty());
        assert_eq!(
            contract_errors(contract(), Some(Err("fork".to_owned()))).len(),
            2
        );
        assert!(contract_errors(Vec::new(), Some(Err("fork".to_owned()))).is_empty());
    }

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
