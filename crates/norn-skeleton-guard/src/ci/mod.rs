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
const GATE_FILES: [&str; 12] = [
    "scripts/ci/core.sh",
    "scripts/ci/implementation_receipts.py",
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
/// Boat runs merge groups and same-repository pull requests: a fork head never reaches
/// a Boat VM. The pull-request arm requires Boat CI's public-repository allowlist
/// (`boat ci public`): author, actor and re-runner must be that user, so Dependabot or
/// another user's pull request runs hosted instead of queueing forever. Trusted-policy
/// jobs stay hosted.
const BOAT_RUNNER: &str = "${{ ((github.event_name == 'merge_group' && vars.NORN_MERGE_RUNNER == 'boat') || (github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository && github.event.pull_request.user.login == 'Youssef-Tarek-Ali-26' && github.actor == 'Youssef-Tarek-Ali-26' && github.triggering_actor == 'Youssef-Tarek-Ali-26' && vars.NORN_PR_RUNNER == 'boat')) && 'boat' || 'ubuntu-24.04' }}";

fn runner_is_admitted(path: &str, job: &str, runner: Option<&str>) -> bool {
    matches!(runner, Some("ubuntu-latest" | "ubuntu-24.04"))
        || (path == ".github/workflows/ci-policy.yml"
            && matches!(job, "core" | "plan" | "test" | "full")
            && runner == Some(BOAT_RUNNER))
}

/// The pull-request arm every admitted concurrency group starts with: one group per
/// workflow and pull request, so a newer push supersedes only that PR's older run.
const PR_GROUP: &str = "${{ github.workflow }}-${{ github.event_name == 'pull_request' && format('pr-{0}', github.event.pull_request.number) || ";

/// The admitted workflow-level concurrency shapes of a `pull_request` workflow, as
/// (group tail after [`PR_GROUP`], `cancel-in-progress`). Every other event gets a
/// group of its own run, so nothing but a superseded PR run is cancelled or held
/// back; the second shape also supersedes older main pushes (each main commit was
/// already gated by its merge group), which needs `push` limited to `main`.
const PR_CONCURRENCY: [(&str, &str, bool); 2] = [
    (
        "format('run-{0}', github.run_id) }}",
        "${{ github.event_name == 'pull_request' }}",
        false,
    ),
    (
        "github.event_name == 'push' && 'main-push' || format('run-{0}', github.run_id) }}",
        "${{ github.event_name == 'pull_request' || github.event_name == 'push' }}",
        true,
    ),
];

/// True when the workflow's `on` names `pull_request` (string, list or mapping form).
fn triggers_pull_request(document: &yaml::Value) -> bool {
    match document.get("on") {
        Some(yaml::Value::Str(event)) => event == "pull_request",
        Some(yaml::Value::Seq(events)) => events
            .iter()
            .any(|event| event.as_str() == Some("pull_request")),
        Some(on @ yaml::Value::Map(_)) => on.get("pull_request").is_some(),
        None => false,
    }
}

/// A `pull_request` workflow must cancel its own superseded PR runs and nothing else:
/// workflow-level `concurrency` is exactly one of [`PR_CONCURRENCY`] (whitespace
/// aside). Workflows without a `pull_request` trigger are out of scope.
fn concurrency_error(path: &str, document: &yaml::Value) -> Option<String> {
    if !triggers_pull_request(document) {
        return None;
    }
    let squash = |value: &yaml::Value| {
        value
            .as_str()
            .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
    };
    let Some(concurrency @ yaml::Value::Map(_)) = document.get("concurrency") else {
        return Some(format!(
            "{path}: a pull_request workflow needs a workflow-level concurrency mapping that cancels its superseded PR runs"
        ));
    };
    if concurrency
        .keys()
        .any(|key| key != "group" && key != "cancel-in-progress")
    {
        return Some(format!(
            "{path}: workflow concurrency may set only group and cancel-in-progress"
        ));
    }
    let group = concurrency.get("group").and_then(squash);
    let cancel = concurrency.get("cancel-in-progress").and_then(squash);
    let Some((_, expected_cancel, main_push)) = PR_CONCURRENCY.iter().find(|(tail, _, _)| {
        group
            .as_deref()
            .and_then(|group| group.strip_prefix(PR_GROUP))
            == Some(*tail)
    }) else {
        return Some(format!(
            "{path}: concurrency group must be per workflow and pull request (github.event.pull_request.number) and give every other event its own run group"
        ));
    };
    if cancel.as_deref() != Some(*expected_cancel) {
        return Some(format!(
            "{path}: cancel-in-progress must be `{expected_cancel}`: superseded pull_request runs are cancelled and no other event is"
        ));
    }
    let push = document.get("on").and_then(|on| on.get("push"));
    let main_only = yaml::Value::Map(vec![(
        "branches".to_owned(),
        yaml::Value::Seq(vec![yaml::Value::Str("main".to_owned())]),
    )]);
    if *main_push && push != Some(&main_only) {
        return Some(format!(
            "{path}: superseding main pushes needs the push trigger limited to branches [main]"
        ));
    }
    None
}

/// A same-repository pull request that fell back to hosted runners says so on its
/// checks; fork pull requests run hosted by design and stay quiet.
const HOSTED_PR_WARNING: &str = r##"name: Warn that pull request CI fell back to hosted runners
if: ${{ runner.environment == 'github-hosted' && github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository && vars.NORN_PR_RUNNER != 'boat' }}
run: |
  echo "::warning title=PR CI on hosted runners::PR CI is running on hosted runners: the repository variable NORN_PR_RUNNER is not 'boat', so this same-repository pull request fell back from Boat."
"##;

/// Boat's base image has only `libclang-18.so.18`; clang-sys loads `libclang.so`.
const BOAT_LIBCLANG: &str = r##"name: Make libclang loadable on Boat runners
if: ${{ runner.environment == 'self-hosted' }}
run: |
  set -euo pipefail
  libclang_dev_version='1:18.1.3-1ubuntu1'
  library=/usr/lib/llvm-18/lib/libclang.so
  if [ -e "$library" ]; then
    echo "$library is already present"
  elif ! { timeout 120 sudo apt-get update -qq && timeout 300 sudo DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends "libclang-18-dev=$libclang_dev_version"; }; then
    echo "::warning::could not install libclang-18-dev=$libclang_dev_version; linking the image's libclang-18.so.18"
    sudo ln -sfn libclang-18.so.18 "$library"
  fi
  ls -l "$library" && [ -e "$library" ] || { echo "::error::$library is missing: clang-sys cannot load libclang"; exit 1; }
"##;

/// Steps `ci-policy.yml` carries exactly once per job: the Boat-capable jobs make
/// libclang loadable on Boat, and `core` warns when pull request CI fell back to
/// hosted runners.
const PINNED_STEPS: [(&str, &str); 3] = [
    ("core", HOSTED_PR_WARNING),
    ("core", BOAT_LIBCLANG),
    ("test", BOAT_LIBCLANG),
];

fn pinned_step_errors(path: &str, document: &yaml::Value) -> Vec<String> {
    if path != ".github/workflows/ci-policy.yml" {
        return Vec::new();
    }
    let mut errors = Vec::new();
    for (job, pinned) in PINNED_STEPS {
        let expected = match yaml::parse(pinned) {
            Ok(step) => step,
            Err(error) => {
                errors.push(format!("{path}::{job}: pinned step: {error}"));
                continue;
            }
        };
        let count = document
            .get("jobs")
            .and_then(|jobs| jobs.get(job))
            .and_then(|job| job.get("steps"))
            .map_or(0, |steps| {
                steps
                    .items()
                    .iter()
                    .filter(|step| **step == expected)
                    .count()
            });
        if count != 1 {
            let name = expected
                .get("name")
                .and_then(yaml::Value::as_str)
                .unwrap_or_default();
            errors.push(format!(
                "{path}::{job}: needs exactly one unchanged `{name}` step"
            ));
        }
    }
    errors
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
                errors.extend(concurrency_error(path, &doc));
                errors.extend(pinned_step_errors(path, &doc));
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
    fn runner_routing_rejects_superseded_runner_contracts() {
        let path = ".github/workflows/ci-policy.yml";
        let legacy = r#"${{ github.event_name == 'merge_group' && vars.NORN_MERGE_RUNNER == 'self-hosted' && fromJSON(format('["self-hosted","norn-merge","norn-run-{0}"]', github.run_id)) || 'ubuntu-24.04' }}"#;
        let merge_only = "${{ github.event_name == 'merge_group' && vars.NORN_MERGE_RUNNER == 'boat' && 'boat' || 'ubuntu-24.04' }}";
        for job in ["core", "plan", "test", "phase", "full", "trusted-policy"] {
            for runner in ["boat", "self-hosted", legacy, merge_only] {
                assert!(!super::runner_is_admitted(path, job, Some(runner)));
            }
            assert!(super::runner_is_admitted(path, job, Some("ubuntu-24.04")));
        }
        assert!(!super::runner_is_admitted(
            ".github/workflows/other.yml",
            "core",
            Some(super::BOAT_RUNNER)
        ));
    }

    #[test]
    fn runner_routing_admits_same_repository_allowlisted_pulls_on_boat() {
        let path = ".github/workflows/ci-policy.yml";
        let reject = |job: &str, changed: &str| {
            assert_ne!(changed, super::BOAT_RUNNER, "no-op mutation");
            assert!(
                !super::runner_is_admitted(path, job, Some(changed)),
                "{job}: {changed}"
            );
        };
        for job in ["core", "plan", "test", "full"] {
            assert!(super::runner_is_admitted(
                path,
                job,
                Some(super::BOAT_RUNNER)
            ));
            for removed in [
                "github.event_name == 'merge_group' && ",
                " && vars.NORN_MERGE_RUNNER == 'boat'",
                "github.event_name == 'pull_request' && ",
                // A fork head must never reach a Boat VM.
                "github.event.pull_request.head.repo.full_name == github.repository && ",
                " && vars.NORN_PR_RUNNER == 'boat'",
                // Boat CI runs public-repository jobs only for its allowlisted user.
                " && github.event.pull_request.user.login == 'Youssef-Tarek-Ali-26'",
                " && github.actor == 'Youssef-Tarek-Ali-26'",
                " && github.triggering_actor == 'Youssef-Tarek-Ali-26'",
            ] {
                reject(job, &super::BOAT_RUNNER.replace(removed, ""));
            }
            for event in [
                "pull_request_target",
                "issue_comment",
                "push",
                "workflow_dispatch",
            ] {
                reject(
                    job,
                    &super::BOAT_RUNNER.replace("'merge_group'", &format!("'{event}'")),
                );
                reject(
                    job,
                    &super::BOAT_RUNNER.replace("'pull_request'", &format!("'{event}'")),
                );
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
                (
                    "github.event.pull_request.user.login == 'Youssef-Tarek-Ali-26'",
                    "github.event.pull_request.user.type != 'Bot'",
                ),
            ] {
                reject(job, &super::BOAT_RUNNER.replace(before, after));
            }
        }
        // Trusted-policy evaluation stays hosted.
        for job in ["trusted-policy", "phase", "unknown"] {
            assert!(!super::runner_is_admitted(
                path,
                job,
                Some(super::BOAT_RUNNER)
            ));
        }
        assert!(!super::runner_is_admitted(
            ".github/workflows/trusted-merge-group.yml",
            "core",
            Some(super::BOAT_RUNNER)
        ));
    }

    fn concurrency(path: &str, source: &str) -> Option<String> {
        let parsed = super::yaml::parse(source);
        assert_eq!(parsed.as_ref().err(), None, "{path}: fixture did not parse");
        let document = parsed.unwrap_or(super::yaml::Value::Map(Vec::new()));
        assert!(
            document.get("jobs").is_some(),
            "{path}: fixture has no jobs"
        );
        super::concurrency_error(path, &document)
    }

    fn workflow(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.github/workflows")
            .join(name);
        std::fs::read_to_string(&path).unwrap_or_default()
    }

    #[test]
    fn every_checked_in_workflow_meets_the_pull_request_concurrency_rule() {
        for name in [
            "ci-policy.yml",
            "codeql.yml",
            "cla.yml",
            "trusted-merge-group.yml",
        ] {
            let source = workflow(name);
            assert!(!source.is_empty(), "{name} is missing");
            assert_eq!(concurrency(name, &source), None, "{name}");
        }
    }

    #[test]
    fn pull_request_concurrency_rejects_codeql_mutations() {
        let source = workflow("codeql.yml");
        let group = "  group: ${{ github.workflow }}-${{ github.event_name == 'pull_request' && format('pr-{0}', github.event.pull_request.number) || format('run-{0}', github.run_id) }}\n";
        let cancel = "  cancel-in-progress: ${{ github.event_name == 'pull_request' }}\n";
        assert!(
            source.contains(group) && source.contains(cancel),
            "codeql.yml drifted"
        );
        for (label, before, after) in [
            (
                "no concurrency",
                format!("concurrency:\n{group}{cancel}"),
                String::new(),
            ),
            (
                "no PR number in the group",
                "github.event.pull_request.number".to_owned(),
                "github.ref".to_owned(),
            ),
            (
                "one shared group",
                group.to_owned(),
                "  group: ${{ github.workflow }}\n".to_owned(),
            ),
            (
                "other events share a group",
                "format('run-{0}', github.run_id)".to_owned(),
                "github.event_name".to_owned(),
            ),
            (
                "unconditional cancel",
                cancel.to_owned(),
                "  cancel-in-progress: true\n".to_owned(),
            ),
            (
                "main pushes cancelled without the main-push group",
                "${{ github.event_name == 'pull_request' }}".to_owned(),
                "${{ github.event_name == 'pull_request' || github.event_name == 'push' }}"
                    .to_owned(),
            ),
            ("no cancel-in-progress", cancel.to_owned(), String::new()),
            (
                "cancel disabled",
                cancel.to_owned(),
                "  cancel-in-progress: false\n".to_owned(),
            ),
            (
                "group only",
                format!("concurrency:\n{group}{cancel}"),
                "concurrency: codeql\n".to_owned(),
            ),
        ] {
            let mutated = source.replacen(&before, &after, 1);
            assert_ne!(mutated, source, "{label}: no-op mutation");
            assert!(
                concurrency("codeql.yml", &mutated).is_some(),
                "{label} was admitted"
            );
        }
    }

    #[test]
    fn pull_request_concurrency_bounds_the_main_push_supersede() {
        let source = workflow("ci-policy.yml");
        for (label, before, after) in [
            (
                "every push branch superseded",
                "  push:\n    branches: [main]\n",
                "  push:\n",
            ),
            (
                "release pushes superseded",
                "branches: [main]",
                "branches: [main, release]",
            ),
            (
                "main-push cancel without its own group",
                " || github.event_name == 'push' && 'main-push'",
                "",
            ),
            (
                "merge groups cancelled",
                "github.event_name == 'pull_request' || github.event_name == 'push' }}",
                "github.event_name != 'workflow_dispatch' }}",
            ),
        ] {
            let mutated = source.replacen(before, after, 1);
            assert_ne!(mutated, source, "{label}: no-op mutation");
            assert!(
                concurrency("ci-policy.yml", &mutated).is_some(),
                "{label} was admitted"
            );
        }
    }

    #[test]
    fn pull_request_concurrency_scopes_to_pull_request_triggers() {
        let jobs = "jobs:\n  a:\n    runs-on: ubuntu-24.04\n";
        for on in [
            "on: pull_request\n",
            "on: [push, pull_request]\n",
            "on:\n  pull_request:\n",
        ] {
            assert!(
                concurrency("x.yml", &format!("{on}{jobs}")).is_some(),
                "{on}"
            );
        }
        for on in [
            "on: pull_request_target\n",
            "on: [push, workflow_run]\n",
            "on:\n  workflow_run:\n    workflows: [Public core]\n",
        ] {
            assert_eq!(concurrency("x.yml", &format!("{on}{jobs}")), None, "{on}");
        }
        let squashed = "on: pull_request\nconcurrency:\n  group: >-\n    ${{ github.workflow }}-${{ github.event_name == 'pull_request' &&\n    format('pr-{0}', github.event.pull_request.number) || format('run-{0}', github.run_id) }}\n  cancel-in-progress: ${{ github.event_name == 'pull_request' }}\n";
        assert_eq!(concurrency("x.yml", &format!("{squashed}{jobs}")), None);
        let extra = squashed.replace("concurrency:\n", "concurrency:\n  queue: max\n");
        assert!(concurrency("x.yml", &format!("{extra}{jobs}")).is_some());
    }

    fn pinned_steps(source: &str) -> Vec<String> {
        let parsed = super::yaml::parse(source);
        assert_eq!(parsed.as_ref().err(), None, "ci-policy.yml did not parse");
        let document = parsed.unwrap_or(super::yaml::Value::Map(Vec::new()));
        super::pinned_step_errors(".github/workflows/ci-policy.yml", &document)
    }

    #[test]
    fn pinned_steps_reject_a_missing_libclang_step_or_hosted_warning() {
        let source = workflow("ci-policy.yml");
        assert_eq!(pinned_steps(&source), Vec::<String>::new());
        let warning = "      - name: Warn that pull request CI fell back to hosted runners\n";
        let libclang = "      - name: Make libclang loadable on Boat runners\n";
        assert_eq!(source.matches(warning).count(), 1);
        assert_eq!(source.matches(libclang).count(), 2);
        for (label, before, after) in [
            ("no hosted warning", warning, "      - name: Renamed\n"),
            (
                "fork pull requests warn",
                " && github.event.pull_request.head.repo.full_name == github.repository && vars.NORN_PR_RUNNER != 'boat'",
                " && vars.NORN_PR_RUNNER != 'boat'",
            ),
            (
                "warning on Boat",
                "${{ runner.environment == 'github-hosted' && github.event_name == 'pull_request'",
                "${{ github.event_name == 'pull_request'",
            ),
            ("no libclang step", libclang, "      - name: Renamed\n"),
            (
                "libclang never verified",
                "ls -l \"$library\" && [ -e \"$library\" ] || { echo \"::error::$library is missing: clang-sys cannot load libclang\"; exit 1; }",
                "true",
            ),
            (
                "libclang on hosted runners",
                "if: ${{ runner.environment == 'self-hosted' }}\n        run: |\n          set -euo pipefail\n          libclang_dev_version",
                "if: ${{ always() }}\n        run: |\n          set -euo pipefail\n          libclang_dev_version",
            ),
        ] {
            let mutated = source.replacen(before, after, 1);
            assert_ne!(mutated, source, "{label}: no-op mutation");
            assert!(!pinned_steps(&mutated).is_empty(), "{label} was admitted");
        }
        let test_only = source.replacen(libclang, "      - name: Renamed\n", 2);
        assert_eq!(pinned_steps(&test_only).len(), 2);
        assert!(
            super::pinned_step_errors(
                ".github/workflows/codeql.yml",
                &super::yaml::Value::Map(Vec::new())
            )
            .is_empty()
        );
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
