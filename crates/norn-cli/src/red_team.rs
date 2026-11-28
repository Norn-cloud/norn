//! Auto-wired `RED-TEAM-1` advisory analysis for `norn check`.
//!
//! Runs `norn_checker_wasm::analyze_red_team_json` on every admitted artifact
//! set checked via `norn check` (`--artifacts` and `--source`) and attaches the
//! deterministic report under the additive `"redTeam"` field without altering
//! `admitted`, `receipt`, `receiptSha256`, or the command exit code.

use serde_json::Value;

/// Check `artifacts_json` against `context_json` through the shared checker
/// ABI and, when admitted, attach the `RED-TEAM-1` advisory report under
/// `"redTeam"`.
///
/// Refused artifact sets are returned verbatim without running red-team
/// analysis so invalid envelopes and zero-CI refusals keep their exact shape.
pub(crate) fn check_with_red_team(
    artifacts_json: &str,
    context_json: &str,
) -> Result<(String, bool), String> {
    let report = norn_checker_wasm::check_artifact_set_json(artifacts_json, context_json)
        .map_err(|error| error.to_string())?;
    let mut value: Value = serde_json::from_str(&report).map_err(|error| error.to_string())?;
    let admitted = value.get("admitted").and_then(Value::as_bool) == Some(true);
    if !admitted {
        return Ok((report, false));
    }
    let red_team = match norn_checker_wasm::analyze_red_team_json(artifacts_json)
        .map_err(|error| error.to_string())
        .and_then(|json| serde_json::from_str::<Value>(&json).map_err(|error| error.to_string()))
    {
        Ok(report) => report,
        Err(error) => serde_json::json!({
            "status": "unavailable",
            "error": error,
        }),
    };
    if let Some(object) = value.as_object_mut() {
        object.insert("redTeam".to_owned(), red_team);
    }
    let enriched = serde_json::to_string(&value).map_err(|error| error.to_string())?;
    Ok((enriched, true))
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use serde_json::{Value, json};

    use super::check_with_red_team;
    use crate::authoring::{CheckSourceRequest, execute_check_source};
    use crate::run_check;

    type TestResult = Result<(), Box<dyn Error>>;

    static SEQ: AtomicU64 = AtomicU64::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Result<Self, Box<dyn Error>> {
            let seq = SEQ.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "norn-cli-red-team-{label}-{}-{seq}",
                std::process::id()
            ));
            if root.exists() {
                std::fs::remove_dir_all(&root)?;
            }
            std::fs::create_dir_all(&root)?;
            let canonical = std::fs::canonicalize(&root)?;
            Ok(Self(canonical))
        }

        fn write(&self, rel: &str, text: &str) -> Result<PathBuf, Box<dyn Error>> {
            let path = self.0.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, text)?;
            Ok(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            if let Err(_cleanup) = std::fs::remove_dir_all(&self.0) {}
        }
    }

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn tools_norn_ts_root() -> PathBuf {
        repo_root().join("tools/norn-ts")
    }

    #[test]
    fn check_artifacts_auto_runs_red_team_and_reports_findings() -> TestResult {
        let scratch = Scratch::new("artifacts")?;
        let crm_ir = tools_norn_ts_root().join("fixtures/definitions/crm.ir.json");
        let context_path = scratch.write("context.json", "{}")?;

        let (report_json, admitted) = run_check(&crm_ir, &context_path)?;
        assert!(admitted, "expected crm.ir.json to admit: {report_json}");

        let report: Value = serde_json::from_str(&report_json)?;
        assert_eq!(
            report.get("format").and_then(Value::as_str),
            Some(norn_checker_wasm::CHECK_FORMAT)
        );
        assert_eq!(report.get("admitted").and_then(Value::as_bool), Some(true));
        assert!(report.get("receipt").is_some_and(Value::is_object));
        assert!(
            report
                .get("receiptSha256")
                .and_then(Value::as_str)
                .is_some()
        );

        let red_team = report
            .get("redTeam")
            .and_then(Value::as_object)
            .ok_or("admitted norn check --artifacts output must include redTeam")?;
        assert!(red_team.get("evaluationErrors").is_none());
        assert!(red_team.get("decodeErrors").is_none());
        assert!(
            red_team
                .get("scopeConfinement")
                .is_some_and(Value::is_array)
        );
        assert!(
            red_team
                .get("unauthenticatedSettlements")
                .is_some_and(Value::is_array)
        );
        assert!(
            red_team
                .get("selfRatifiableTransitions")
                .is_some_and(Value::is_array)
        );
        let unconstrained = red_team
            .get("unconstrainedWrites")
            .and_then(Value::as_array)
            .ok_or("missing redTeam.unconstrainedWrites")?;
        assert!(
            unconstrained.contains(&json!({
                "actionId": "crm.contact.upsert",
                "objectType": "crm.Contact",
                "property": "email",
            })),
            "expected crm.Contact.email unconstrained write finding, got: {red_team:?}"
        );
        Ok(())
    }

    #[test]
    fn check_source_ts_auto_runs_red_team_without_opt_in_flag() -> TestResult {
        let ts_root = tools_norn_ts_root();
        let crm_source = ts_root.join("fixtures/definitions/crm.source.ts");

        let checked = execute_check_source(&CheckSourceRequest {
            source: &crm_source,
            context: None,
            org: Some("org_demo"),
            env: Some("dev"),
            forbid_unsafe_zen: false,
            tool_root: Some(&ts_root),
        });
        assert_eq!(
            checked.exit_code, 0,
            "advisory red-team findings must not fail exit code: stdout={} stderr={}",
            checked.stdout, checked.stderr
        );

        let report: Value = serde_json::from_str(&checked.stdout)?;
        assert_eq!(report.get("admitted").and_then(Value::as_bool), Some(true));
        let unconstrained = report
            .pointer("/redTeam/unconstrainedWrites")
            .and_then(Value::as_array)
            .ok_or(
                "admitted norn check --source output must include redTeam.unconstrainedWrites",
            )?;
        assert!(
            unconstrained.contains(&json!({
                "actionId": "crm.contact.upsert",
                "objectType": "crm.Contact",
                "property": "email",
            })),
            "expected crm.Contact.email unconstrained write finding, got: {report}"
        );
        Ok(())
    }

    #[test]
    fn check_source_clean_python_reports_empty_red_team() -> TestResult {
        let scratch = Scratch::new("clean-py")?;
        let module = scratch.write(
            "clean.py",
            r#"from __future__ import annotations

from norn_py import Concept, Field, KeySpec


class Item(
    Concept,
    name="demo.Item",
    title="Item",
    version="0.1.0",
    identity=(KeySpec("primary", ("id",)),),
):
    id: Field[str]
    label: Field[str]
"#,
        )?;
        let checked = execute_check_source(&CheckSourceRequest {
            source: &module,
            context: None,
            org: Some("org_demo"),
            env: Some("dev"),
            forbid_unsafe_zen: false,
            tool_root: None,
        });
        assert_eq!(
            checked.exit_code, 0,
            "clean python check failed: stdout={} stderr={}",
            checked.stdout, checked.stderr
        );
        let report: Value = serde_json::from_str(&checked.stdout)?;
        assert_eq!(report.get("admitted").and_then(Value::as_bool), Some(true));
        let red_team = report
            .get("redTeam")
            .and_then(Value::as_object)
            .ok_or("expected redTeam report on clean admitted source")?;
        for key in [
            "scopeConfinement",
            "unauthenticatedSettlements",
            "selfRatifiableTransitions",
            "unconstrainedWrites",
        ] {
            let findings = red_team
                .get(key)
                .and_then(Value::as_array)
                .ok_or("expected redTeam finding array")?;
            assert!(
                findings.is_empty(),
                "expected empty {key} on clean concept, got: {findings:?}"
            );
        }
        assert!(red_team.get("evaluationErrors").is_none());
        assert!(red_team.get("decodeErrors").is_none());
        Ok(())
    }

    #[test]
    fn check_refused_artifact_set_omits_red_team() -> TestResult {
        let (report_json, admitted) = check_with_red_team(r#"[{"invalid":true}]"#, "{}")?;
        assert!(!admitted);
        let report: Value = serde_json::from_str(&report_json)?;
        assert_eq!(report.get("admitted").and_then(Value::as_bool), Some(false));
        assert!(
            report.get("redTeam").is_none(),
            "refused check must not attach redTeam: {report}"
        );
        Ok(())
    }
}
