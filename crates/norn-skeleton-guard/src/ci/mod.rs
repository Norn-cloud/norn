//! Public-core CI: candidate checks and base-owned policy over candidate data.
pub mod git;
pub mod provenance;
pub mod yaml;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
type Obligations = BTreeMap<String, BTreeSet<String>>;

/// The only repository whose own branches may change workflows or gate files.
const MAINTAINED_REPOSITORY: &str = "Norn-cloud/norn";

/// Gate files pinned to the trusted base, besides every workflow.
const GATE_FILES: [&str; 11] = [
    "scripts/ci/core.sh",
    ".config/nextest.toml",
    "scripts/ci/validate-runner-policy.py",
    "scripts/proof_enrollment.py",
    "verification/coverage-v1/validate.py",
    "verification/coverage-v1/negative_canaries.py",
    "scripts/coverage_manifest.py",
    "scripts/verify-promoted-subsystems.sh",
    "scripts/check-proof-holes.py",
    "crates/norn-skeleton-guard/baseline.json",
    "crates/norn-checker/tests/non_code_literals.baseline.json",
];

/// Exact opt-in contract: event payloads and arbitrary variables cannot select a runner.
/// Boat runs merge groups, and pull requests only from this repository's own branches:
/// a fork head never reaches a Boat VM. The pull-request arm also requires Boat CI's
/// public-repository allowlist (`boat ci public`): author, actor and re-runner must be
/// that user, so Dependabot or another user's pull request falls back to hosted.
const BOAT_RUNNER: &str = "${{ ((github.event_name == 'merge_group' && vars.NORN_MERGE_RUNNER == 'boat') || (github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository && github.event.pull_request.user.login == 'Youssef-Tarek-Ali-26' && github.actor == 'Youssef-Tarek-Ali-26' && github.triggering_actor == 'Youssef-Tarek-Ali-26' && vars.NORN_PR_RUNNER == 'boat')) && 'boat' || 'ubuntu-24.04' }}";

fn runner_is_admitted(path: &str, job: &str, runner: Option<&str>) -> bool {
    matches!(runner, Some("ubuntu-latest" | "ubuntu-24.04"))
        || (path == ".github/workflows/ci-policy.yml"
            && matches!(job, "core" | "plan" | "test" | "phase" | "full")
            && runner == Some(BOAT_RUNNER))
}

/// The longest merge-queue group the trusted lane walks; longer groups fail closed.
const MAX_MERGE_GROUP: usize = 64;

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
    policy_contract(
        root,
        base,
        evidence,
        provenance::check_against(root, base).errors(),
    )
}

fn policy_contract(
    root: &Path,
    base: &Path,
    evidence: Option<&Path>,
    mut errors: Vec<String>,
) -> Vec<String> {
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
    for path in GATE_FILES {
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
                        if !runner_is_admitted(
                            path,
                            name,
                            job.get("runs-on").and_then(yaml::Value::as_str),
                        ) {
                            errors.push(format!(
                                "{path}::{name}: runner must be hosted or the exact merge-only Boat opt-in"
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

/// [`maintainer_approval`] of the evidence file for the candidate checkout's HEAD,
/// or [`merge_group_approval`] when the evidence describes a merge-queue group.
fn approval_of(evidence: &Path, root: &Path) -> Result<String, String> {
    let bytes = std::fs::read(evidence).map_err(|error| format!("maintainer evidence: {error}"))?;
    let document: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|error| format!("maintainer evidence: {error}"))?;
    if document.get("merge_group").is_some() {
        return merge_group_approval(&document, root);
    }
    maintainer_approval(&document, &git::head(root)?)
}

/// A full lowercase commit SHA from `value[key]`.
fn commit_sha(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|sha| {
            sha.len() == 40 && sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        })
        .map(str::to_owned)
}

/// The approvers of a merge-queue group's gate changes.
///
/// The candidate checkout is the group's head commit. Walking first parents
/// from it down to the group's base, every step must be a two-parent merge
/// `(previous, pull request head)`, which is how the queue builds groups with
/// the merge method. A step that changes a workflow or gate file must leave
/// them exactly as at that pull request's head, and that head must carry an
/// unedited admin `/approve-gates <head>` comment ([`maintainer_approval`]).
/// Steps that change no gate file need no evidence. So the landing gates are
/// always the trusted base's or an approved head's, never an unreviewed merge.
///
/// # Errors
/// Returns why the group is not admitted: malformed evidence, a candidate
/// that is not the group head, a step that is not such a merge, a group longer
/// than 64 merges, gate files that differ from the approved head, or
/// a gate-changing pull request without an approval.
pub fn merge_group_approval(evidence: &serde_json::Value, root: &Path) -> Result<String, String> {
    if evidence
        .get("repository")
        .and_then(serde_json::Value::as_str)
        != Some(MAINTAINED_REPOSITORY)
    {
        return Err(format!(
            "merge group evidence is not for {MAINTAINED_REPOSITORY}"
        ));
    }
    let group = evidence
        .get("merge_group")
        .ok_or("merge group evidence is missing")?;
    let (Some(base), Some(head)) = (commit_sha(group, "base_sha"), commit_sha(group, "head_sha"))
    else {
        return Err("merge group evidence names no exact base and head commits".to_owned());
    };
    let candidate = git::head(root)?;
    if head != candidate {
        return Err(format!(
            "evidence is for {head}, the candidate is {candidate}"
        ));
    }
    let pull_requests = evidence
        .get("pull_requests")
        .and_then(serde_json::Value::as_array)
        .ok_or("merge group evidence has no pull requests")?;
    let gates: Vec<&str> = [".github/workflows"]
        .into_iter()
        .chain(GATE_FILES)
        .collect();
    let mut approvers = Vec::new();
    let mut commit = head;
    for _ in 0..MAX_MERGE_GROUP {
        if commit == base {
            return Ok(approvers.join(", "));
        }
        let [previous, pull_head] =
            <[String; 2]>::try_from(git::parents(root, &commit)?).map_err(|parents| {
                format!(
                    "merge group commit {commit} is not a two-parent merge ({} parents)",
                    parents.len()
                )
            })?;
        if !git::same_paths(root, &previous, &commit, &gates)? {
            if !git::same_paths(root, &pull_head, &commit, &gates)? {
                return Err(format!(
                    "merge group commit {commit} has gates that differ from pull request head {pull_head}"
                ));
            }
            // Several pull requests can share a head; any approved one admits it.
            let mut outcome = Err(format!(
                "no pull request evidence for gate-changing head {pull_head}"
            ));
            for pull in pull_requests
                .iter()
                .filter(|pull| commit_sha(pull, "head_sha").as_deref() == Some(pull_head.as_str()))
            {
                let mut document = pull.clone();
                if let Some(map) = document.as_object_mut() {
                    map.insert("repository".to_owned(), MAINTAINED_REPOSITORY.into());
                }
                outcome = maintainer_approval(&document, &pull_head);
                if outcome.is_ok() {
                    break;
                }
            }
            approvers.push(outcome?);
        }
        commit = previous;
    }
    if commit == base {
        return Ok(approvers.join(", "));
    }
    Err(format!(
        "merge group is longer than {MAX_MERGE_GROUP} merges or misses its base"
    ))
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
    let head =
        commit_sha(evidence, "head_sha").ok_or("maintainer evidence names no exact head commit")?;
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

/// The split coverage: subsystem metadata and independently addressed obligation entries.
const COVERAGE_MANIFEST: &str = "verification/coverage-v1/manifest";
const COVERAGE_SUBSYSTEMS: &str = "verification/coverage-v1/manifest/subsystems";

fn retained_obligations(root: &Path, base: &Path) -> Result<(), String> {
    let after = enrolled_obligations(root)?;
    for (id, required) in enrolled_obligations(base)? {
        if !after
            .get(&id)
            .is_some_and(|actual| required.is_subset(actual))
        {
            return Err(format!("{id}: retained core obligations were removed"));
        }
    }
    Ok(())
}

fn enrolled_obligations(root: &Path) -> Result<Obligations, String> {
    let manifest = root.join(COVERAGE_MANIFEST);
    if !manifest.join("index.json").is_file() {
        return Err(format!("{COVERAGE_MANIFEST}/index.json is missing"));
    }
    let mut enrolled = Obligations::new();
    let entries = std::fs::read_dir(root.join(COVERAGE_SUBSYSTEMS))
        .map_err(|error| format!("missing source enrollment: {error}"))?;
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let name = entry.file_name();
        let raw = name.to_str().ok_or("invalid subsystem name")?;
        let id = raw;
        if id.is_empty()
            || !id
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        {
            return Err(format!("{COVERAGE_SUBSYSTEMS}: unknown entry {raw}"));
        }
        if !entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            return Err(format!("{COVERAGE_SUBSYSTEMS}: unknown entry {raw}"));
        }
        let required = entry_obligations(&entry.path())?;
        if enrolled.insert(id.to_owned(), required).is_some() {
            return Err(format!("duplicate subsystem {id}"));
        }
    }
    Ok(enrolled)
}

fn entry_obligations(directory: &Path) -> Result<BTreeSet<String>, String> {
    use sha2::{Digest, Sha256};
    let read = |path: &Path| -> Result<serde_json::Value, String> {
        if !path
            .symlink_metadata()
            .map_err(|e| e.to_string())?
            .is_file()
        {
            return Err(format!("{} is not a regular entry", path.display()));
        }
        serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())
    };
    let metadata = read(&directory.join("index.json"))?;
    if metadata.get("obligations") != Some(&serde_json::json!({"$entries": "obligations"})) {
        return Err("missing subsystem obligations entry field".to_owned());
    }
    let folder = directory.join("obligations");
    match folder.symlink_metadata() {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Err("obligations is not a regular directory".to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(error) => return Err(error.to_string()),
    }
    let mut result = BTreeSet::new();
    for entry in std::fs::read_dir(folder).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let value = read(&entry.path())?;
        let id = value
            .get("value")
            .and_then(serde_json::Value::as_str)
            .ok_or("invalid obligation entry value")?;
        if value.as_object().is_none_or(|object| object.len() != 2)
            || value
                .get("order")
                .and_then(serde_json::Value::as_u64)
                .is_none()
        {
            return Err("invalid obligation entry envelope".to_owned());
        }
        let mut hash = Sha256::new();
        hash.update(b"entry:string\0");
        hash.update(id.as_bytes());
        let expected = format!("{:x}.json", hash.finalize());
        if entry.file_name().to_str() != Some(expected.as_str()) || !result.insert(id.to_owned()) {
            return Err("duplicate or mismatched obligation entry identity".to_owned());
        }
    }
    Ok(result)
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
        .find_map(|item| {
            let syn::Item::Enum(item) = item else {
                return None;
            };
            (item.ident == "ArtifactKind").then(|| {
                item.variants
                    .into_iter()
                    .map(|variant| variant.ident.to_string())
                    .collect::<BTreeSet<_>>()
            })
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
    #[test]
    fn runner_routing_admits_boat_only_for_merge_groups_and_same_repository_pulls() {
        let path = ".github/workflows/ci-policy.yml";
        let legacy = r#"${{ github.event_name == 'merge_group' && vars.NORN_MERGE_RUNNER == 'self-hosted' && fromJSON(format('["self-hosted","norn-merge","norn-run-{0}"]', github.run_id)) || 'ubuntu-24.04' }}"#;
        for job in ["core", "plan", "test", "phase", "full"] {
            assert!(super::runner_is_admitted(
                path,
                job,
                Some(super::BOAT_RUNNER)
            ));
            for removed in [
                "github.event_name == 'merge_group' && ",
                " && vars.NORN_MERGE_RUNNER == 'boat'",
                "github.event_name == 'pull_request' && ",
                "github.event.pull_request.head.repo.full_name == github.repository && ",
                " && vars.NORN_PR_RUNNER == 'boat'",
                " && github.event.pull_request.user.login == 'Youssef-Tarek-Ali-26'",
                " && github.actor == 'Youssef-Tarek-Ali-26'",
                " && github.triggering_actor == 'Youssef-Tarek-Ali-26'",
            ] {
                let changed = super::BOAT_RUNNER.replace(removed, "");
                assert!(!super::runner_is_admitted(path, job, Some(&changed)));
            }
            for event in [
                "pull_request",
                "pull_request_target",
                "issue_comment",
                "push",
                "workflow_dispatch",
            ] {
                let changed = super::BOAT_RUNNER.replace("merge_group", event);
                assert!(!super::runner_is_admitted(path, job, Some(&changed)));
                let changed = super::BOAT_RUNNER.replace("'pull_request'", &format!("'{event}'"));
                assert!(
                    event == "pull_request"
                        || !super::runner_is_admitted(path, job, Some(&changed))
                );
            }
            let merge_only = "${{ github.event_name == 'merge_group' && vars.NORN_MERGE_RUNNER == 'boat' && 'boat' || 'ubuntu-24.04' }}";
            assert!(!super::runner_is_admitted(path, job, Some(merge_only)));
            for runner in ["boat", "self-hosted", legacy] {
                assert!(!super::runner_is_admitted(path, job, Some(runner)));
            }
            for (before, after) in [
                ("== 'boat'", "== 'self-hosted'"),
                ("|| 'ubuntu-24.04'", "|| 'boat'"),
                ("== github.repository", "!= ''"),
                ("vars.NORN_PR_RUNNER", "vars.NORN_MERGE_RUNNER"),
                ("'Youssef-Tarek-Ali-26'", "'dependabot[bot]'"),
                (
                    "github.actor == 'Youssef-Tarek-Ali-26'",
                    "github.actor != 'dependabot[bot]'",
                ),
            ] {
                let changed = super::BOAT_RUNNER.replace(before, after);
                assert!(!super::runner_is_admitted(path, job, Some(&changed)));
            }
        }
        for job in [
            "trusted-policy",
            "rights-capture",
            "rights-revocation",
            "unknown",
        ] {
            assert!(!super::runner_is_admitted(
                path,
                job,
                Some(super::BOAT_RUNNER)
            ));
        }
        assert!(!super::runner_is_admitted(
            ".github/workflows/other.yml",
            "core",
            Some(super::BOAT_RUNNER)
        ));
        assert!(super::runner_is_admitted(
            path,
            "trusted-policy",
            Some("ubuntu-24.04")
        ));
    }

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

/// Base-owned admission: rights review and gate approval are independent authorities.
#[must_use]
pub fn run_with_rights(
    root: &Path,
    base: &Path,
    head: &str,
    repository: &str,
    gate_evidence: Option<&Path>,
    rights_evidence: &Path,
) -> Vec<String> {
    check_with_rights(root, base, head, repository, gate_evidence, rights_evidence).errors
}

/// Complete trusted gate result, retained outside tracked records.
#[derive(serde::Serialize)]
pub struct TrustedReport {
    /// Exact bytes, rights record inventory and admission identity.
    pub provenance: provenance::Report,
    /// Policy and review refusals; an empty list is necessary for success.
    pub errors: Vec<String>,
}

/// Check trusted review and contract policy, reporting the identity actually inspected.
#[must_use]
pub fn check_with_rights(
    root: &Path,
    base: &Path,
    head: &str,
    repository: &str,
    gate_evidence: Option<&Path>,
    rights_evidence: &Path,
) -> TrustedReport {
    let source = provenance::BoundSource {
        source: root,
        head,
        base,
        repository,
    };
    let provenance = provenance::check_bound(root, &source, false);
    let mut errors = provenance.errors();
    let rights = std::fs::read(rights_evidence)
        .map_err(|e| e.to_string())
        .and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()))
        .and_then(|doc| rights_admission(&doc, root, base, head));
    if let Err(e) = rights {
        errors.push(format!("rights acknowledgment: {e}"));
    }
    let errors = policy_contract(root, base, gate_evidence, errors);
    TrustedReport { provenance, errors }
}

/// Validate one global rights ledger for an immutable head and tree.
/// The latest recorded acknowledgment is never replaced by an older review
/// when its source comment is removed. Every pending event blocks the target.
///
/// # Errors
/// Refuses wrong identities, missing history, unreconciled or withdrawn reviews.
pub fn rights_acknowledgment(
    evidence: &serde_json::Value,
    head: &str,
    tree: &str,
) -> Result<String, String> {
    if evidence
        .get("repository")
        .and_then(serde_json::Value::as_str)
        != Some(MAINTAINED_REPOSITORY)
        || commit_sha(evidence, "head_sha").as_deref() != Some(head)
        || evidence
            .get("head_repository")
            .and_then(serde_json::Value::as_str)
            .is_none()
        || commit_sha(evidence, "base_sha").is_none()
    {
        return Err("wrong repository/base/head rights identity".into());
    }
    let entries = rights_entries(evidence, head, tree)?;
    let mut ids = BTreeSet::new();
    let mut boundary = None;
    let mut latest: Option<(u64, bool, String, String)> = None;
    for entry in entries {
        let id = entry
            .get("id")
            .and_then(serde_json::Value::as_u64)
            .ok_or("missing rights receipt ID")?;
        if !ids.insert(id) {
            return Err("duplicate rights receipt ID".into());
        }
        let at = entry
            .get("recorded_at")
            .and_then(serde_json::Value::as_str)
            .filter(|at| rights_timestamp(at))
            .ok_or("invalid rights receipt timestamp")?;
        match entry.get("kind").and_then(serde_json::Value::as_str) {
            Some("revoke" | "withdraw") => {
                boundary = Some(boundary.map_or(at, |prior: &str| prior.max(at)));
            }
            Some("ack") => {
                let review = rights_review(entry, head)?;
                if latest.as_ref().is_none_or(|prior| review.0 > prior.0) {
                    latest = Some(review);
                }
            }
            Some("ignore") => {}
            _ => return Err("invalid rights ledger decision".into()),
        }
    }
    match latest {
        Some((_, true, reviewer, created)) if boundary.is_none_or(|at| created.as_str() > at) => {
            Ok(reviewer)
        }
        _ => Err(format!(
            "missing, edited, deleted or revoked /ack-rights {head}"
        )),
    }
}

/// Canonical digest used by the base-owned checkpoint and publisher.
///
/// # Errors
/// Refuses numbers outside GitHub's safe integer range or oversized framing.
pub fn rights_digest(value: &serde_json::Value) -> Result<String, String> {
    use sha2::{Digest as _, Sha256};
    fn count(hash: &mut Sha256, value: usize) -> Result<(), String> {
        hash.update(
            u64::try_from(value)
                .map_err(|error| format!("checkpoint length overflow: {error}"))?
                .to_be_bytes(),
        );
        Ok(())
    }
    fn walk(hash: &mut Sha256, value: &serde_json::Value) -> Result<(), String> {
        match value {
            serde_json::Value::Null => hash.update([0]),
            serde_json::Value::Bool(value) => hash.update([if *value { 2 } else { 1 }]),
            serde_json::Value::Number(value) => {
                let number = value
                    .as_i64()
                    .filter(|value| value.unsigned_abs() <= 9_007_199_254_740_991)
                    .ok_or("unsafe checkpoint number")?
                    .to_string();
                hash.update([3]);
                count(hash, number.len())?;
                hash.update(number.as_bytes());
            }
            serde_json::Value::String(value) => {
                hash.update([4]);
                count(hash, value.len())?;
                hash.update(value.as_bytes());
            }
            serde_json::Value::Array(values) => {
                hash.update([5]);
                count(hash, values.len())?;
                for value in values {
                    walk(hash, value)?;
                }
            }
            serde_json::Value::Object(values) => {
                let ordered: std::collections::BTreeMap<_, _> = values.iter().collect();
                hash.update([6]);
                count(hash, ordered.len())?;
                for (key, value) in ordered {
                    walk(hash, &serde_json::Value::String(key.clone()))?;
                    walk(hash, value)?;
                }
            }
        }
        Ok(())
    }
    let mut hash = Sha256::new();
    hash.update(b"norn-rights-checkpoint-v4\0");
    walk(&mut hash, value)?;
    Ok(hex::encode(hash.finalize()))
}

fn rights_projection(value: &serde_json::Value, digest: Option<&str>) -> Result<(), String> {
    let statuses = value
        .get("verdicts")
        .and_then(serde_json::Value::as_array)
        .ok_or("missing publication history")?;
    let version = value
        .get("failure_version")
        .and_then(serde_json::Value::as_u64)
        .ok_or("missing failure version")?;
    let latest = statuses.iter().max_by_key(|status| {
        status
            .get("id")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default()
    });
    let failure = statuses
        .iter()
        .filter(|status| {
            matches!(
                status.get("state").and_then(serde_json::Value::as_str),
                Some("failure" | "error")
            )
        })
        .filter_map(|status| status.get("id").and_then(serde_json::Value::as_u64))
        .max()
        .unwrap_or_default();
    if version != failure {
        return Err("wrong failure version".into());
    }
    if let Some(status) = latest
        .filter(|status| status.get("state").and_then(serde_json::Value::as_str) == Some("success"))
    {
        let row: serde_json::Value = serde_json::from_str(
            status
                .get("description")
                .and_then(serde_json::Value::as_str)
                .ok_or("missing success checkpoint")?,
        )
        .map_err(|error| format!("unbound success checkpoint: {error}"))?;
        if row.get("v").and_then(serde_json::Value::as_u64) != Some(4)
            || row.get("f").and_then(serde_json::Value::as_u64) != Some(failure)
            || !row
                .get("d")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|value| {
                    value.len() == 64
                        && value
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                })
            || digest.is_some_and(|expected| {
                row.get("d").and_then(serde_json::Value::as_str) != Some(expected)
            })
        {
            return Err("stale success cannot override a newer failure or ledger".into());
        }
    }
    Ok(())
}

fn rights_publication(evidence: &serde_json::Value, root: &Path, head: &str) -> Result<(), String> {
    let publication = evidence
        .get("publication")
        .ok_or("missing publication checkpoint")?;
    let tree = git::tree_oid(root, head)?;
    if commit_sha(publication, "head_sha").as_deref() != Some(head)
        || commit_sha(publication, "tree_sha").as_deref() != Some(tree.as_str())
    {
        return Err("wrong publication target".into());
    }
    let mut targets = serde_json::Map::new();
    for (head, trees) in evidence
        .get("rights")
        .and_then(serde_json::Value::as_object)
        .ok_or("missing publication rights")?
    {
        let trees = trees
            .as_object()
            .filter(|trees| trees.len() == 1)
            .ok_or("ambiguous publication tree")?;
        let ledger = trees.values().next().ok_or("missing target checkpoint")?;
        let digest = ledger
            .get("checkpoint")
            .and_then(|value| value.get("digest"))
            .and_then(serde_json::Value::as_str)
            .ok_or("missing target digest")?;
        targets.insert(head.clone(), digest.into());
    }
    if publication.get("targets") != Some(&serde_json::Value::Object(targets)) {
        return Err("wrong publication coverage".into());
    }
    let data = serde_json::json!({"head_sha":head,"tree_sha":tree,"failure_version":publication.get("failure_version"),"targets":publication.get("targets")});
    let digest = rights_digest(&data)?;
    if publication
        .get("digest")
        .and_then(serde_json::Value::as_str)
        != Some(digest.as_str())
    {
        return Err("publication digest mismatch".into());
    }
    rights_projection(publication, Some(&digest))
}

fn rights_entries<'a>(
    evidence: &'a serde_json::Value,
    head: &str,
    tree: &str,
) -> Result<&'a Vec<serde_json::Value>, String> {
    let targets = evidence
        .get("rights")
        .and_then(|v| v.get(head))
        .and_then(serde_json::Value::as_object)
        .ok_or("missing target rights ledger")?;
    let ledger = targets
        .get(tree)
        .filter(|_| targets.len() == 1)
        .ok_or("wrong rights tree identity")?;
    if commit_sha(ledger, "head_sha").as_deref() != Some(head)
        || commit_sha(ledger, "tree_sha").as_deref() != Some(tree)
    {
        return Err("wrong rights ledger target".into());
    }
    let data = serde_json::json!({"head_sha":ledger.get("head_sha"),"tree_sha":ledger.get("tree_sha"),
        "pending":ledger.get("pending"),"entries":ledger.get("entries"),"failure_version":ledger.get("failure_version")});
    let digest = rights_digest(&data)?;
    if ledger
        .get("checkpoint")
        .and_then(|value| value.get("digest"))
        .and_then(serde_json::Value::as_str)
        != Some(digest.as_str())
    {
        return Err("target checkpoint digest mismatch".into());
    }
    rights_projection(ledger, None)?;
    let pending = ledger
        .get("pending")
        .and_then(serde_json::Value::as_array)
        .ok_or("missing pending rights history")?;
    if !pending.is_empty() {
        return Err("unreconciled target rights events".into());
    }
    ledger
        .get("entries")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "missing rights ledger entries".into())
}

fn rights_review(
    entry: &serde_json::Value,
    head: &str,
) -> Result<(u64, bool, String, String), String> {
    let id = entry
        .get("comment_id")
        .and_then(serde_json::Value::as_u64)
        .ok_or("missing rights comment ID")?;
    let created = entry
        .get("created_at")
        .and_then(serde_json::Value::as_str)
        .filter(|at| rights_timestamp(at))
        .ok_or("invalid rights review timestamp")?;
    let comment = entry.get("comment").unwrap_or(&serde_json::Value::Null);
    let valid = comment.get("id").and_then(serde_json::Value::as_u64) == Some(id)
        && comment
            .get("permission")
            .and_then(serde_json::Value::as_str)
            == Some("admin")
        && comment
            .get("created_at")
            .and_then(serde_json::Value::as_str)
            == Some(created)
        && comment
            .get("updated_at")
            .and_then(serde_json::Value::as_str)
            == Some(created)
        && comment
            .get("body")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|body| body.trim() == format!("/ack-rights {head}"));
    let user = comment
        .get("user")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    Ok((
        id,
        valid && !user.is_empty(),
        user.to_owned(),
        created.to_owned(),
    ))
}

// GitHub API timestamps use canonical UTC seconds, so lexical order is time order.
fn rights_timestamp(at: &str) -> bool {
    at.len() == 20
        && at.bytes().enumerate().all(|(index, byte)| match index {
            4 | 7 => byte == b'-',
            10 => byte == b'T',
            13 | 16 => byte == b':',
            19 => byte == b'Z',
            _ => byte.is_ascii_digit(),
        })
}

/// Verify constituent reviews and independently recompute each clean merge.
///
/// # Errors
/// Refuses uncovered heads and any integration-only content or metadata.
pub fn rights_admission(
    evidence: &serde_json::Value,
    root: &Path,
    base_root: &Path,
    head: &str,
) -> Result<(), String> {
    if git::head(root)? != head {
        return Err("candidate differs from rights head".into());
    }
    let base = git::head(base_root)?;
    rights_publication(evidence, root, head)?;
    if let Some(group) = evidence.get("merge_group") {
        if evidence
            .get("repository")
            .and_then(serde_json::Value::as_str)
            != Some(MAINTAINED_REPOSITORY)
            || commit_sha(group, "head_sha").as_deref() != Some(head)
            || commit_sha(group, "base_sha").as_deref() != Some(base.as_str())
        {
            return Err("wrong merge-group rights identity".into());
        }
        let pulls = evidence
            .get("pull_requests")
            .and_then(serde_json::Value::as_array)
            .ok_or("missing constituent rights evidence")?;
        let mut current = head.to_owned();
        let mut covered = BTreeSet::new();
        for _ in 0..MAX_MERGE_GROUP {
            if current == base {
                break;
            }
            let [previous, pull] = <[String; 2]>::try_from(git::parents(root, &current)?)
                .map_err(|_parents| "rights group requires two-parent merges")?;
            let mut found = false;
            for constituent in pulls
                .iter()
                .filter(|p| commit_sha(p, "head_sha").as_deref() == Some(pull.as_str()))
            {
                let mut doc = constituent.clone();
                doc.as_object_mut()
                    .ok_or("malformed constituent rights evidence")?
                    .insert("repository".into(), MAINTAINED_REPOSITORY.into());
                doc.as_object_mut()
                    .ok_or("malformed rights evidence")?
                    .insert(
                        "rights".into(),
                        evidence
                            .get("rights")
                            .cloned()
                            .ok_or("missing global rights ledger")?,
                    );
                if rights_acknowledgment(&doc, &pull, &git::tree_oid(root, &pull)?).is_ok() {
                    found = true;
                }
            }
            if !found {
                return Err(format!("unacknowledged constituent head {pull}"));
            }
            covered.insert(pull.clone());
            git::clean_merge(root, &previous, &pull, &current)?;
            current = previous;
        }
        if current != base
            || pulls
                .iter()
                .any(|p| commit_sha(p, "head_sha").is_none_or(|h| !covered.contains(&h)))
        {
            return Err("unaccounted merge-group head or excessive group length".into());
        }
    } else {
        if commit_sha(evidence, "base_sha").as_deref() != Some(base.as_str()) {
            return Err("rights base differs from trusted base".into());
        }
        rights_acknowledgment(evidence, head, &git::tree_oid(root, head)?)?;
    }
    Ok(())
}

/// Candidate diagnostic gate. The trusted lane separately requires live rights evidence.
#[must_use]
pub fn run_local(root: &Path) -> Vec<String> {
    let head = match git::head(root) {
        Ok(h) => h,
        Err(e) => return vec![e],
    };
    let source = provenance::BoundSource {
        source: root,
        head: &head,
        base: root,
        repository: MAINTAINED_REPOSITORY,
    };
    policy_contract(
        root,
        root,
        None,
        provenance::check_bound(root, &source, false).errors(),
    )
}
