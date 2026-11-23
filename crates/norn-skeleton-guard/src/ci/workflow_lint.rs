//! The workflow-lint baseline only shrinks (#1348 CI-R2).
//!
//! `core.sh` `cheap` runs zizmor over `.github/workflows` with the gated
//! `crates/norn-skeleton-guard/zizmor.yml`, and any finding not ignored fails
//! there. This module checks the other direction: every ignore, either a
//! `zizmor.yml` entry (`<workflow>:<line>`) or a `# zizmor: ignore[rule]`
//! comment in a workflow, must still cover a finding of its rule in zizmor's
//! `--no-ignores` report. An ignore that covers nothing is stale and must be
//! deleted in the same PR, so the baseline never outlives its findings.

use std::path::Path;

use super::yaml;

/// Where an ignore is written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ignore {
    /// Rule (zizmor audit id).
    pub rule: String,
    /// Workflow file name, as zizmor reports it (`ci-policy.yml`).
    pub file: String,
    /// 1-based line the ignore applies to.
    pub line: usize,
    /// `zizmor.yml` or `comment`.
    pub source: &'static str,
}

/// One finding's rule, workflow file name and 1-based line span.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    /// Rule (zizmor audit id).
    pub rule: String,
    /// Workflow file name.
    pub file: String,
    /// First line of the primary location.
    pub start: usize,
    /// Last line of the primary location.
    pub end: usize,
}

fn file_name(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_owned()
}

/// The ignores in a zizmor config (`rules.<rule>.ignore: [<file>:<line>[:<col>]]`).
///
/// # Errors
///
/// A message if the config is not YAML or an entry has no line.
pub fn config_ignores(config: &str) -> Result<Vec<Ignore>, String> {
    let root = yaml::parse(config)?;
    let mut out = Vec::new();
    for (rule, body) in root.get("rules").map_or_default(yaml::Value::entries) {
        for entry in body.get("ignore").map_or_default(yaml::Value::items) {
            let text = entry
                .as_str()
                .ok_or_else(|| format!("zizmor.yml {rule}: ignore entries are strings"))?;
            let mut parts = text.split(':');
            let file = parts.next().unwrap_or_default();
            let line = parts
                .next()
                .and_then(|line| line.parse::<usize>().ok())
                .ok_or_else(|| {
                    format!("zizmor.yml {rule}: `{text}` must name a line (`<workflow>:<line>`); a whole-file ignore is refused")
                })?;
            out.push(Ignore {
                rule: rule.clone(),
                file: file.to_owned(),
                line,
                source: "zizmor.yml",
            });
        }
    }
    Ok(out)
}

/// The `# zizmor: ignore[rule, ..]` comments of one workflow.
#[must_use]
pub fn comment_ignores(file: &str, text: &str) -> Vec<Ignore> {
    let marker = "# zizmor: ignore[";
    let mut out = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let Some(start) = line.find(marker) else {
            continue;
        };
        let rest = line
            .get(start.saturating_add(marker.len())..)
            .unwrap_or_default();
        let rules = rest.split(']').next().unwrap_or_default();
        for rule in rules
            .split(',')
            .map(str::trim)
            .filter(|rule| !rule.is_empty())
        {
            out.push(Ignore {
                rule: rule.to_owned(),
                file: file.to_owned(),
                line: index.saturating_add(1),
                source: "comment",
            });
        }
    }
    out
}

/// The findings of a zizmor `--format json` report, by primary location.
///
/// # Errors
///
/// A message if the report is not the expected JSON shape.
pub fn findings(report: &str) -> Result<Vec<Finding>, String> {
    let value: serde_json::Value =
        serde_json::from_str(report).map_err(|e| format!("zizmor report: {e}"))?;
    let mut out = Vec::new();
    for finding in value.as_array().ok_or("zizmor report: expected an array")? {
        let rule = finding
            .get("ident")
            .and_then(serde_json::Value::as_str)
            .ok_or("zizmor report: finding without `ident`")?;
        let primary = finding
            .get("locations")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .find(|location| {
                location
                    .pointer("/symbolic/kind")
                    .and_then(serde_json::Value::as_str)
                    == Some("Primary")
            })
            .ok_or_else(|| format!("zizmor report: `{rule}` finding without a primary location"))?;
        let path = primary
            .pointer("/symbolic/key/Local/verbatim_path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("zizmor report: `{rule}` finding outside a local workflow"))?;
        let row = |point: &str| {
            primary
                .pointer(&format!("/concrete/location/{point}/row"))
                .and_then(serde_json::Value::as_u64)
                .and_then(|row| usize::try_from(row).ok())
                .map(|row| row.saturating_add(1))
                .ok_or_else(|| format!("zizmor report: `{rule}` finding without a {point}"))
        };
        out.push(Finding {
            rule: rule.to_owned(),
            file: file_name(path),
            start: row("start_point")?,
            end: row("end_point")?,
        });
    }
    Ok(out)
}

/// Each ignore that covers no finding of its rule (same file, line inside the
/// finding's span), as a message saying to delete it.
#[must_use]
pub fn stale_ignores(ignores: &[Ignore], findings: &[Finding]) -> Vec<String> {
    ignores
        .iter()
        .filter(|ignore| {
            !findings.iter().any(|finding| {
                finding.rule == ignore.rule
                    && finding.file == ignore.file
                    && (finding.start..=finding.end).contains(&ignore.line)
            })
        })
        .map(|ignore| match ignore.source {
            "comment" => format!(
                ".github/workflows/{}:{}: `# zizmor: ignore[{}]` no longer matches a finding; delete it (the workflow-lint baseline only shrinks)",
                ignore.file, ignore.line, ignore.rule
            ),
            _ => format!(
                "crates/norn-skeleton-guard/zizmor.yml: `{}` ignore `{}:{}` no longer matches a finding; delete it (the workflow-lint baseline only shrinks)",
                ignore.rule, ignore.file, ignore.line
            ),
        })
        .collect()
}

/// Checks every ignore under `root` against zizmor's `--no-ignores` report.
#[must_use]
pub fn check(root: &Path, report: &str) -> Vec<String> {
    let mut errors = Vec::new();
    let mut ignores =
        match std::fs::read_to_string(root.join("crates/norn-skeleton-guard/zizmor.yml"))
            .map_err(|e| format!("read zizmor.yml: {e}"))
            .and_then(|config| config_ignores(&config))
        {
            Ok(ignores) => ignores,
            Err(error) => return vec![error],
        };
    let mut workflows: Vec<_> =
        std::fs::read_dir(root.join(".github/workflows")).map_or_default(|dir| {
            dir.filter_map(Result::ok)
                .map(|entry| entry.path())
                .collect()
        });
    workflows.sort();
    for path in workflows {
        let name = path
            .file_name()
            .map_or_default(|name| name.to_string_lossy().into_owned());
        let yaml = path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("yml") || ext.eq_ignore_ascii_case("yaml"));
        if !yaml {
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(text) => ignores.extend(comment_ignores(&name, &text)),
            Err(error) => errors.push(format!("read {}: {error}", path.display())),
        }
    }
    match findings(report) {
        Ok(found) => errors.extend(stale_ignores(&ignores, &found)),
        Err(error) => errors.push(error),
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(entries: &[(&str, &str, u64, u64)]) -> String {
        let findings: Vec<serde_json::Value> = entries
            .iter()
            .map(|(rule, file, start, end)| {
                serde_json::json!({
                    "ident": rule,
                    "locations": [
                        { "symbolic": { "kind": "Related", "key": { "Local": { "verbatim_path": format!(".github/workflows/{file}") } } },
                          "concrete": { "location": { "start_point": { "row": 0 }, "end_point": { "row": 0 } } } },
                        { "symbolic": { "kind": "Primary", "key": { "Local": { "verbatim_path": format!(".github/workflows/{file}") } } },
                          "concrete": { "location": { "start_point": { "row": start.saturating_sub(1) }, "end_point": { "row": end.saturating_sub(1) } } } }
                    ]
                })
            })
            .collect();
        serde_json::Value::Array(findings).to_string()
    }

    const CONFIG: &str =
        "rules:\n  template-injection:\n    ignore:\n      - ci.yml:10\n      - ci.yml:20:5\n";
    const WORKFLOW: &str = "on:\n  pull_request_target: # zizmor: ignore[dangerous-triggers]\njobs:\n  a:\n    steps:\n      - run: | # zizmor: ignore[github-env, template-injection]\n";

    fn all_ignores() -> Vec<Ignore> {
        let mut ignores = config_ignores(CONFIG).expect("config");
        ignores.extend(comment_ignores("ci.yml", WORKFLOW));
        ignores
    }

    #[test]
    fn every_ignore_that_still_covers_a_finding_passes() {
        let found = findings(&report(&[
            ("template-injection", "ci.yml", 10, 10),
            ("template-injection", "ci.yml", 19, 21),
            ("dangerous-triggers", "ci.yml", 2, 2),
            ("github-env", "ci.yml", 6, 12),
            ("template-injection", "ci.yml", 6, 6),
        ]))
        .expect("report");
        assert_eq!(all_ignores().len(), 5);
        assert_eq!(stale_ignores(&all_ignores(), &found), Vec::<String>::new());
    }

    #[test]
    fn a_fixed_finding_forces_its_ignore_out() {
        // The line-20 injection and the github-env write were fixed: both ignores are stale.
        let found = findings(&report(&[
            ("template-injection", "ci.yml", 10, 10),
            ("dangerous-triggers", "ci.yml", 2, 2),
            ("template-injection", "ci.yml", 6, 6),
        ]))
        .expect("report");
        let stale = stale_ignores(&all_ignores(), &found);
        assert_eq!(stale.len(), 2, "{stale:?}");
        assert!(
            stale
                .iter()
                .any(|m| m.contains("zizmor.yml: `template-injection` ignore `ci.yml:20`")),
            "{stale:?}"
        );
        assert!(
            stale
                .iter()
                .any(|m| m.contains("ci.yml:6: `# zizmor: ignore[github-env]`")),
            "{stale:?}"
        );
        // An ignore of another rule on the right line covers nothing.
        let other = findings(&report(&[("unpinned-uses", "ci.yml", 10, 10)])).expect("report");
        assert_eq!(
            stale_ignores(&config_ignores(CONFIG).expect("config")[..1], &other).len(),
            1
        );
    }

    #[test]
    fn whole_file_ignores_and_bad_reports_are_refused() {
        assert!(
            config_ignores("rules:\n  template-injection:\n    ignore:\n      - ci.yml\n").is_err()
        );
        assert!(findings("{}").is_err());
        assert!(findings(r#"[{"ident": "x", "locations": []}]"#).is_err());
    }
}
