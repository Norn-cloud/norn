//! Golden corpus parity and standalone profile checker tests for `norn-authoring`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use norn_authoring::{
    AuthoringProgram, BoundedVfs, FileReport, PINNED_EFFECT_DIST_PREFIX, Profile, ProfileChecker,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ExpectedDiagnostic {
    code: String,
    start: String,
    end: String,
    construct: String,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ExpectedReport {
    profile: String,
    diagnostics: Vec<ExpectedDiagnostic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    uses: Option<Vec<String>>,
}

fn tool_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/norn-ts")
}

fn list_ts_fixtures(dir: &Path) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut names = Vec::new();
    for entry_res in std::fs::read_dir(dir)? {
        let entry = entry_res?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if name.as_bytes().ends_with(b".ts") {
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
}

fn mount_corpus_dir(
    vfs: &mut BoundedVfs,
    disk_dir: &Path,
    virtual_prefix: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut entries = Vec::new();
    for entry_res in std::fs::read_dir(disk_dir)? {
        let entry = entry_res?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if name.as_bytes().ends_with(b".ts") {
            entries.push((name, entry.path()));
        }
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    for (name, full_path) in entries {
        let content = std::fs::read_to_string(&full_path)?;
        vfs.add_file(&format!("{virtual_prefix}/{name}"), content)?;
    }
    Ok(())
}

fn observed_report(report: &FileReport, with_uses: bool) -> ExpectedReport {
    let diagnostics = report
        .diagnostics
        .iter()
        .map(|diag| ExpectedDiagnostic {
            code: diag.code.clone(),
            start: format!("{}:{}", diag.start.line, diag.start.column),
            end: format!("{}:{}", diag.end.line, diag.end.column),
            construct: diag.construct.clone(),
        })
        .collect();
    let uses = with_uses.then(|| {
        report
            .uses
            .iter()
            .map(|usage| format!("{} {}", usage.at, usage.symbol))
            .collect()
    });
    ExpectedReport {
        profile: report.profile.clone(),
        diagnostics,
        uses,
    }
}

fn expected_golden_path(dir: &Path, ts_name: &str) -> PathBuf {
    let json_name = ts_name.strip_suffix(".ts").map_or_else(
        || format!("{ts_name}.expected.json"),
        |stem| format!("{stem}.expected.json"),
    );
    dir.join(json_name)
}

fn load_golden(path: &Path) -> Result<ExpectedReport, Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&text)?)
}

fn verify_accept_corpus(
    corpus_dir: &Path,
    accept_names: &[String],
    by_fixture: &BTreeMap<String, &FileReport>,
    profile: &Profile,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut used_rows: BTreeSet<String> = BTreeSet::new();
    for name in accept_names {
        let fixture = format!("accept/{name}");
        let report = by_fixture
            .get(&fixture)
            .ok_or_else(|| format!("missing report for {fixture}"))?;
        assert!(
            report.diagnostics.is_empty(),
            "accept fixture `{fixture}` reported unexpected diagnostics: {:#?}",
            report.diagnostics
        );
        let golden_path = expected_golden_path(&corpus_dir.join("accept"), name);
        let expected = load_golden(&golden_path)?;
        let actual = observed_report(report, true);
        assert_eq!(actual, expected, "accept fixture `{fixture}` mismatch");
        for usage in &report.uses {
            used_rows.insert(usage.row.clone());
        }
    }
    let required_rows: BTreeSet<String> = profile
        .admitted
        .iter()
        .map(|entry| entry.row.clone())
        .collect();
    let uncovered: Vec<String> = required_rows.difference(&used_rows).cloned().collect();
    assert!(
        uncovered.is_empty(),
        "accept corpus did not exercise all symbol-backed rows: {uncovered:?}"
    );
    Ok(())
}

fn verify_reject_corpus(
    corpus_dir: &Path,
    reject_names: &[String],
    by_fixture: &BTreeMap<String, &FileReport>,
    profile: &Profile,
) -> Result<(), Box<dyn std::error::Error>> {
    for name in reject_names {
        let fixture = format!("reject/{name}");
        let report = by_fixture
            .get(&fixture)
            .ok_or_else(|| format!("missing report for {fixture}"))?;
        assert!(
            !report.diagnostics.is_empty(),
            "reject fixture `{fixture}` must emit at least one diagnostic"
        );
        let type_errors: Vec<_> = report
            .diagnostics
            .iter()
            .filter(|diag| diag.code == "NORN-FLOW-TYPE")
            .collect();
        assert!(
            type_errors.is_empty(),
            "reject fixture `{fixture}` had unexpected NORN-FLOW-TYPE errors: {type_errors:#?}"
        );
        let golden_path = expected_golden_path(&corpus_dir.join("reject"), name);
        let expected = load_golden(&golden_path)?;
        let actual = observed_report(report, false);
        assert_eq!(actual, expected, "reject fixture `{fixture}` mismatch");
    }
    for construct in &profile.rejected_constructs {
        assert_ne!(construct.fixtures, Vec::<String>::new());
        for fixture in &construct.fixtures {
            let report = by_fixture
                .get(fixture)
                .ok_or_else(|| format!("missing report for {fixture}"))?;
            let has_code = report
                .diagnostics
                .iter()
                .any(|diag| diag.code == construct.code);
            assert!(
                has_code,
                "construct `{}` expected `{}` in `{fixture}`, got {:?}",
                construct.construct,
                construct.code,
                report
                    .diagnostics
                    .iter()
                    .map(|diag| diag.code.as_str())
                    .collect::<Vec<_>>()
            );
        }
    }
    Ok(())
}

#[test]
fn golden_corpus_parity_matches_ts_profile_checker() -> Result<(), Box<dyn std::error::Error>> {
    let root = tool_root();
    let corpus_dir = root.join("corpus");
    let effect_dir = root.join("node_modules/effect");
    assert!(
        effect_dir.join("package.json").is_file(),
        "pinned effect package must be installed at {} before running corpus parity",
        effect_dir.display()
    );

    let profile = Profile::load()?;
    let checker = ProfileChecker::new(profile.clone());

    let mut vfs = BoundedVfs::default();
    let effect_count = vfs.mount_pinned_effect_dir(&effect_dir, &profile)?;
    assert!(effect_count > 0, "expected pinned effect declarations");

    mount_corpus_dir(&mut vfs, &corpus_dir.join("support"), "/corpus/support")?;
    mount_corpus_dir(&mut vfs, &corpus_dir.join("accept"), "/corpus/accept")?;
    mount_corpus_dir(&mut vfs, &corpus_dir.join("reject"), "/corpus/reject")?;

    let accept_names = list_ts_fixtures(&corpus_dir.join("accept"))?;
    let reject_names = list_ts_fixtures(&corpus_dir.join("reject"))?;
    assert!(!accept_names.is_empty(), "expected accept corpus fixtures");
    assert!(!reject_names.is_empty(), "expected reject corpus fixtures");

    let mut root_paths: Vec<String> =
        Vec::with_capacity(accept_names.len().saturating_add(reject_names.len()));
    for name in &accept_names {
        root_paths.push(format!("/corpus/accept/{name}"));
    }
    for name in &reject_names {
        root_paths.push(format!("/corpus/reject/{name}"));
    }
    let root_slices: Vec<&str> = root_paths.iter().map(String::as_str).collect();

    let program = AuthoringProgram::new(vfs, &root_slices, profile.clone())?
        .with_effect_dist_prefix(PINNED_EFFECT_DIST_PREFIX);
    let reports = checker.check_program(&program, &root_slices)?;
    assert_eq!(reports.len(), root_paths.len());

    let by_fixture: BTreeMap<String, &FileReport> = reports
        .iter()
        .map(|report| {
            let key = report
                .file
                .strip_prefix("corpus/")
                .unwrap_or(report.file.as_str())
                .to_owned();
            (key, report)
        })
        .collect();

    verify_accept_corpus(&corpus_dir, &accept_names, &by_fixture, &profile)?;
    verify_reject_corpus(&corpus_dir, &reject_names, &by_fixture, &profile)?;
    Ok(())
}

#[test]
fn export_path_probes_resolve_to_recorded_identities() -> Result<(), Box<dyn std::error::Error>> {
    let root = tool_root();
    let effect_dir = root.join("node_modules/effect");
    assert!(
        effect_dir.join("package.json").is_file(),
        "pinned effect package must be installed at {}",
        effect_dir.display()
    );

    let profile = Profile::load()?;
    let checker = ProfileChecker::new(profile.clone());

    let mut probes: Vec<(&str, &str)> = Vec::new();
    for entry in &profile.admitted {
        probes.push((entry.probe.as_str(), entry.symbol.as_str()));
    }
    for entry in &profile.rejected {
        probes.push((entry.probe.as_str(), entry.symbol.as_str()));
    }

    let mut lines = vec![
        "import * as effect from \"effect\"".to_owned(),
        "import * as ai from \"effect/unstable/ai\"".to_owned(),
        "import * as norn from \"@norn/authoring\"".to_owned(),
    ];
    for (idx, &(probe, _)) in probes.iter().enumerate() {
        lines.push(format!("export const p{idx} = {probe}"));
    }
    let expected: Vec<Option<String>> = probes
        .iter()
        .map(|&(_, symbol)| Some(symbol.to_owned()))
        .collect();
    lines.push(String::new());
    let source = lines.join("\n");

    let mut vfs = BoundedVfs::default();
    vfs.mount_pinned_effect_dir(&effect_dir, &profile)?;
    vfs.add_file("/probe.ts", source)?;

    let program = AuthoringProgram::new(vfs, &["/probe.ts"], profile)?
        .with_effect_dist_prefix(PINNED_EFFECT_DIST_PREFIX);
    let resolved = checker.resolve_initializers(&program, "/probe.ts")?;
    let actual: Vec<Option<String>> = resolved.into_iter().map(|(_, id)| id).collect();
    assert_eq!(actual, expected);
    Ok(())
}

#[test]
fn standalone_in_memory_profile_checks_enforced_without_disk_packages()
-> Result<(), Box<dyn std::error::Error>> {
    let profile = Profile::load()?;
    let checker = ProfileChecker::new(profile);

    let mut vfs = BoundedVfs::default();
    vfs.add_file("/support/bad.ts", "export const now = Date.now()\n")?;
    vfs.add_file(
        "/app/entry.ts",
        r#"import { Norn } from "@norn/authoring"
import { now } from "../support/bad.ts"
import "../support/bad.ts"

let count = 0
export const badNum = -1e400
export const refNow = now
export const fnHandle = Norn.fn<{ readonly x: number }, { readonly y: number }>({
  id: "demo.fn",
  version: "1",
})
"#,
    )?;

    let reports = checker.check_vfs(vfs, &["/app/entry.ts"], None)?;
    let report = reports.first().ok_or("expected one FileReport")?;
    let codes: Vec<&str> = report
        .diagnostics
        .iter()
        .map(|diag| diag.code.as_str())
        .collect();
    assert!(
        codes.contains(&"NORN-FLOW-IMPORT"),
        "expected transitive NORN-FLOW-IMPORT, got: {codes:?}"
    );
    assert!(
        codes.contains(&"NORN-FLOW-MODULE"),
        "expected side-effect NORN-FLOW-MODULE, got: {codes:?}"
    );
    assert!(
        codes.contains(&"NORN-FLOW-CAPTURE"),
        "expected mutable let NORN-FLOW-CAPTURE, got: {codes:?}"
    );
    assert!(
        codes.contains(&"NORN-FLOW-EXPRESSION"),
        "expected non-finite -1e400 NORN-FLOW-EXPRESSION, got: {codes:?}"
    );
    assert!(
        report
            .uses
            .iter()
            .any(|usage| usage.symbol == "@norn/authoring#Norn.fn"),
        "expected admitted Norn.fn symbol use, got: {:?}",
        report.uses
    );
    Ok(())
}
