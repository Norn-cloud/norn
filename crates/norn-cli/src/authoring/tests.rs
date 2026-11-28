//! Integration tests for native Rust authoring subcommands in `norn-cli`.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

use crate::{CheckArgs, run_check_args};

use super::zero_ci::ZERO_CI_DIAGNOSTIC_CODE;
use super::{
    CheckSourceRequest, CompileArgs, DashboardArgs, FmtArgs, ProfileArgs, QuestionArgs,
    execute_check_source, execute_compile, execute_dashboard, execute_fmt, execute_profile,
    execute_question,
};

type TestResult = Result<(), Box<dyn Error>>;

static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

struct TempWorkspace(PathBuf);

impl TempWorkspace {
    fn new(label: &str) -> Result<Self, Box<dyn Error>> {
        let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "norn-cli-authoring-{label}-{}-{seq}",
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
        let dest = self.0.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, text)?;
        Ok(dest)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempWorkspace {
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
fn py_compile_and_check_source_admits_hr_pilot_and_standalone_module() -> TestResult {
    let hr_pilot = repo_root().join("tools/norn-py/examples/hr_pilot.py");
    let compiled = execute_compile(&CompileArgs {
        path: hr_pilot.clone(),
        org: Some("org_demo".to_owned()),
        env: Some("dev".to_owned()),
        forbid_unsafe_zen: false,
        out: None,
        tool_root: None,
        pack_json: false,
        registry: None,
        root: None,
        source_path: None,
        modules: None,
        allow_empty: false,
        max_expansion_work: None,
    });
    assert_eq!(
        compiled.exit_code, 0,
        "hr_pilot compile failed: stdout={} stderr={}",
        compiled.stdout, compiled.stderr
    );
    let artifacts: Value = serde_json::from_str(&compiled.stdout)?;
    let list = artifacts.as_array().ok_or("expected JSON artifact array")?;
    assert_eq!(list.len(), 6);
    assert!(
        list.iter().any(
            |item| item.get("artifactId").and_then(Value::as_str) == Some("hr.Employee.entity")
        )
    );
    assert!(
        list.iter()
            .any(|item| item.get("artifactId").and_then(Value::as_str)
                == Some("hr.shift.valid_window_and_hours.law"))
    );

    // Check hr_pilot.py in-process against core.units base layer.
    let scratch = TempWorkspace::new("py-hr-pilot")?;
    let core_units_text =
        std::fs::read_to_string(repo_root().join("registry/core/artifacts/core.units.json"))?;
    let core_units: Value = serde_json::from_str(&core_units_text)?;
    let ctx_path = scratch.write("context.json", &json!({ "base": [core_units] }).to_string())?;
    let checked = execute_check_source(&CheckSourceRequest {
        source: &hr_pilot,
        context: Some(&ctx_path),
        base: &[],
        org: Some("org_demo"),
        env: Some("dev"),
        forbid_unsafe_zen: false,
        tool_root: None,
    });
    assert_eq!(
        checked.exit_code, 0,
        "hr_pilot check --source failed: stdout={} stderr={}",
        checked.stdout, checked.stderr
    );
    let report: Value = serde_json::from_str(&checked.stdout)?;
    assert_eq!(report.get("admitted").and_then(Value::as_bool), Some(true));

    // Also verify a standalone Python module admits with zero context file.
    let standalone = scratch.write(
        "standalone.py",
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
    let standalone_check = execute_check_source(&CheckSourceRequest {
        source: &standalone,
        context: None,
        base: &[],
        org: Some("org_demo"),
        env: Some("dev"),
        forbid_unsafe_zen: false,
        tool_root: None,
    });
    assert_eq!(
        standalone_check.exit_code, 0,
        "standalone py check failed: stdout={} stderr={}",
        standalone_check.stdout, standalone_check.stderr
    );
    let standalone_report: Value = serde_json::from_str(&standalone_check.stdout)?;
    assert_eq!(
        standalone_report.get("admitted").and_then(Value::as_bool),
        Some(true)
    );
    Ok(())
}

#[test]
fn ts_compile_check_profile_on_crm_fixture() -> TestResult {
    let ts_root = tools_norn_ts_root();
    assert!(
        ts_root.join("node_modules/effect/package.json").is_file(),
        "pinned effect package must be installed at {}",
        ts_root.display()
    );
    let crm = ts_root.join("fixtures/definitions/crm.source.ts");

    let compiled = execute_compile(&CompileArgs {
        path: crm.clone(),
        org: Some("org_demo".to_owned()),
        env: Some("dev".to_owned()),
        forbid_unsafe_zen: false,
        out: None,
        tool_root: Some(ts_root.clone()),
        pack_json: false,
        registry: None,
        root: None,
        source_path: None,
        modules: None,
        allow_empty: false,
        max_expansion_work: None,
    });
    assert_eq!(
        compiled.exit_code, 0,
        "crm compile failed: stdout={} stderr={}",
        compiled.stdout, compiled.stderr
    );
    let parsed: Value = serde_json::from_str(&compiled.stdout)?;
    let artifacts = parsed.as_array().ok_or("expected JSON array")?;
    assert!(
        artifacts.iter().any(
            |item| item.get("artifactId").and_then(Value::as_str) == Some("crm.Account.entity")
        )
    );
    assert!(
        artifacts.iter().any(
            |item| item.get("artifactId").and_then(Value::as_str) == Some("crm.Contact.entity")
        )
    );

    let checked = execute_check_source(&CheckSourceRequest {
        source: &crm,
        context: None,
        base: &[],
        org: Some("org_demo"),
        env: Some("dev"),
        forbid_unsafe_zen: false,
        tool_root: Some(&ts_root),
    });
    assert_eq!(
        checked.exit_code, 0,
        "crm check --source failed: stdout={} stderr={}",
        checked.stdout, checked.stderr
    );
    let report: Value = serde_json::from_str(&checked.stdout)?;
    assert_eq!(report.get("admitted").and_then(Value::as_bool), Some(true));

    // --forbid-unsafe-zen must refuse crm.source.ts because ArchiveContact uses Norn.unsafeZen.
    let strict = execute_compile(&CompileArgs {
        path: crm.clone(),
        org: Some("org_demo".to_owned()),
        env: Some("dev".to_owned()),
        forbid_unsafe_zen: true,
        out: None,
        tool_root: Some(ts_root.clone()),
        pack_json: false,
        registry: None,
        root: None,
        source_path: None,
        modules: None,
        allow_empty: false,
        max_expansion_work: None,
    });
    assert_eq!(strict.exit_code, 1);
    assert!(
        strict.stdout.contains("NORN-AUTHORING-UNSAFE-ZEN"),
        "expected NORN-AUTHORING-UNSAFE-ZEN, got: {}",
        strict.stdout
    );

    // Profile check on crm.source.ts must pass cleanly.
    let profiled = execute_profile(&ProfileArgs {
        path: crm,
        tool_root: Some(ts_root),
    });
    assert_eq!(
        profiled.exit_code, 0,
        "crm profile failed: stdout={} stderr={}",
        profiled.stdout, profiled.stderr
    );
    let reports: Value = serde_json::from_str(&profiled.stdout)?;
    let report_list = reports.as_array().ok_or("expected profile report array")?;
    assert_eq!(report_list.len(), 1);
    let diags = report_list
        .first()
        .and_then(|item| item.get("diagnostics"))
        .and_then(Value::as_array)
        .ok_or("missing diagnostics array")?;
    assert_eq!(diags, &Vec::<Value>::new());
    Ok(())
}

#[test]
fn ts_question_dashboard_and_profile_refusal() -> TestResult {
    let ts_root = tools_norn_ts_root();
    let scratch = TempWorkspace::new("ts-q-d")?;

    let question_file = ts_root.join("fixtures/questions/scrap-yield.ts");
    let question_out_path = scratch.path().join("question.json");
    let question_stdout = execute_question(&QuestionArgs {
        path: question_file.clone(),
        out: None,
        tool_root: None,
    });
    assert_eq!(
        question_stdout.exit_code, 0,
        "question stdout failed: stdout={} stderr={}",
        question_stdout.stdout, question_stdout.stderr
    );
    let question_parsed: Value = serde_json::from_str(&question_stdout.stdout)?;
    let question_list = question_parsed
        .as_array()
        .ok_or("expected question array")?;
    assert_eq!(question_list.len(), 1);
    assert_eq!(
        question_list
            .first()
            .and_then(|item| item.get("name"))
            .and_then(Value::as_str),
        Some("demo.yield_week")
    );

    let question_written = execute_question(&QuestionArgs {
        path: question_file,
        out: Some(question_out_path.clone()),
        tool_root: None,
    });
    assert_eq!(question_written.exit_code, 0);
    assert_eq!(question_written.stdout, "");
    assert_eq!(
        std::fs::read_to_string(&question_out_path)?,
        question_stdout.stdout
    );

    let dashboard_file = ts_root.join("fixtures/dashboards/refinery.ts");
    let dashboard_out_path = scratch.path().join("dashboard.json");
    let dashboard_stdout = execute_dashboard(&DashboardArgs {
        path: dashboard_file.clone(),
        out: None,
        tool_root: None,
    });
    assert_eq!(
        dashboard_stdout.exit_code, 0,
        "dashboard stdout failed: stdout={} stderr={}",
        dashboard_stdout.stdout, dashboard_stdout.stderr
    );
    let dashboard_parsed: Value = serde_json::from_str(&dashboard_stdout.stdout)?;
    let dashboard_list = dashboard_parsed
        .as_array()
        .ok_or("expected dashboard array")?;
    assert_eq!(dashboard_list.len(), 1);
    assert_eq!(
        dashboard_list
            .first()
            .and_then(|item| item.get("title"))
            .and_then(Value::as_str),
        Some("Refinery weekly")
    );

    let dashboard_written = execute_dashboard(&DashboardArgs {
        path: dashboard_file,
        out: Some(dashboard_out_path.clone()),
        tool_root: None,
    });
    assert_eq!(dashboard_written.exit_code, 0);
    assert_eq!(dashboard_written.stdout, "");
    assert_eq!(
        std::fs::read_to_string(&dashboard_out_path)?,
        dashboard_stdout.stdout
    );

    // Invalid TypeScript construct must fail profile check with exit code 1.
    let bad_ts = scratch.write("bad.ts", "while (true) {}\n")?;
    let bad_profile = execute_profile(&ProfileArgs {
        path: bad_ts,
        tool_root: None,
    });
    assert_eq!(bad_profile.exit_code, 1);
    let bad_reports: Value = serde_json::from_str(&bad_profile.stdout)?;
    let bad_diags = bad_reports
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|item| item.get("diagnostics"))
        .and_then(Value::as_array)
        .ok_or("expected diagnostics array on profile refusal")?;
    assert_ne!(bad_diags, &Vec::<Value>::new());
    Ok(())
}

#[test]
fn fmt_check_and_write_for_ts_and_py() -> TestResult {
    let scratch = TempWorkspace::new("fmt")?;
    let unformatted_typescript = "export const item={a:1,b:2};\n";
    let unformatted_python = "x=1+2\n";
    let typescript_file = scratch.write("src/model.ts", unformatted_typescript)?;
    let python_file = scratch.write("src/model.py", unformatted_python)?;

    // --check must exit 1, list both files, and leave files unmodified.
    let check_dirty = execute_fmt(&FmtArgs {
        check: true,
        path: scratch.path().to_path_buf(),
    });
    assert_eq!(check_dirty.exit_code, 1);
    assert!(check_dirty.stdout.contains("src/model.ts"));
    assert!(check_dirty.stdout.contains("src/model.py"));
    assert_eq!(
        std::fs::read_to_string(&typescript_file)?,
        unformatted_typescript
    );
    assert_eq!(std::fs::read_to_string(&python_file)?, unformatted_python);

    // Format in place.
    let formatted = execute_fmt(&FmtArgs {
        check: false,
        path: scratch.path().to_path_buf(),
    });
    assert_eq!(formatted.exit_code, 0, "fmt failed: {}", formatted.stderr);
    assert_eq!(
        std::fs::read_to_string(&typescript_file)?,
        "export const item = { a: 1, b: 2 }\n"
    );
    assert_eq!(std::fs::read_to_string(&python_file)?, "x = 1 + 2\n");

    // Re-running --check must now exit 0 with empty output.
    let check_clean = execute_fmt(&FmtArgs {
        check: true,
        path: scratch.path().to_path_buf(),
    });
    assert_eq!(check_clean.exit_code, 0);
    assert_eq!(check_clean.stdout, "");

    // Explicit single-file formatting of `.d.ts` succeeds while directory walks skip it.
    let dts_file = scratch.write(
        "src/types.d.ts",
        "export type Foo = { readonly a: string };\n",
    )?;
    let dir_check = execute_fmt(&FmtArgs {
        check: true,
        path: scratch.path().to_path_buf(),
    });
    assert_eq!(dir_check.exit_code, 0);
    let dts_fmt = execute_fmt(&FmtArgs {
        check: false,
        path: dts_file.clone(),
    });
    assert_eq!(dts_fmt.exit_code, 0, "dts fmt failed: {}", dts_fmt.stderr);
    assert_eq!(
        std::fs::read_to_string(&dts_file)?,
        "export type Foo = { readonly a: string }\n"
    );
    Ok(())
}

#[test]
fn check_source_refuses_norn_app_with_github_workflows() -> TestResult {
    let scratch = TempWorkspace::new("zero-ci")?;
    scratch.write(
        "norn.json",
        &json!({
            "format": "norn-project/1",
            "name": "zero-ci-app",
            "sources": ["src"],
            "scope": { "orgId": "local", "environmentId": "dev" },
            "dev": { "port": 4174, "actor": "local:developer" },
            "cell": ".norn/cell.sqlite",
        })
        .to_string(),
    )?;
    let app_py = scratch.write(
        "src/app.py",
        r#"from __future__ import annotations

from norn_py import Concept, Field, KeySpec


class Note(
    Concept,
    name="app.Note",
    title="Note",
    version="0.1.0",
    identity=(KeySpec("primary", ("id",)),),
):
    id: Field[str]
    body: Field[str]
"#,
    )?;
    scratch.write(".github/workflows/ci.yml", "name: ci\non: push\n")?;

    // Both the app directory and a source file inside it must be refused, even if a spoofed
    // `crates/norn-checker` directory exists inside the app.
    std::fs::create_dir_all(scratch.path().join("crates/norn-checker"))?;
    for target in [scratch.path(), app_py.as_path()] {
        let refused = execute_check_source(&CheckSourceRequest {
            source: target,
            context: None,
            base: &[],
            org: Some("local"),
            env: Some("dev"),
            forbid_unsafe_zen: false,
            tool_root: None,
        });
        assert_eq!(refused.exit_code, 1);
        let parsed: Value = serde_json::from_str(&refused.stdout)?;
        assert_eq!(
            parsed.get("format").and_then(Value::as_str),
            Some(norn_checker_wasm::CHECK_FORMAT)
        );
        assert_eq!(parsed.get("admitted").and_then(Value::as_bool), Some(false));
        assert!(parsed.get("receipt").is_some_and(Value::is_null));
        assert!(parsed.get("receiptSha256").is_some_and(Value::is_null));
        let diag_doc = parsed
            .get("diagnostics")
            .and_then(Value::as_object)
            .ok_or("expected versioned diagnostics object")?;
        assert_eq!(
            diag_doc.get("format").and_then(Value::as_str),
            Some("norn-checker/diagnostics@1")
        );
        let has_zero_ci = diag_doc
            .get("diagnostics")
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items.iter().any(|diag| {
                    diag.get("code").and_then(Value::as_str) == Some(ZERO_CI_DIAGNOSTIC_CODE)
                })
            });
        assert!(has_zero_ci, "expected {ZERO_CI_DIAGNOSTIC_CODE}: {parsed}");
    }

    // Removing .github/workflows allows the app to admit cleanly using norn.json scope.
    std::fs::remove_dir_all(scratch.path().join(".github"))?;
    let admitted = execute_check_source(&CheckSourceRequest {
        source: scratch.path(),
        context: None,
        base: &[],
        org: None,
        env: None,
        forbid_unsafe_zen: false,
        tool_root: None,
    });
    assert_eq!(
        admitted.exit_code, 0,
        "expected admission after removing .github/workflows: stdout={} stderr={}",
        admitted.stdout, admitted.stderr
    );
    let report: Value = serde_json::from_str(&admitted.stdout)?;
    assert_eq!(report.get("admitted").and_then(Value::as_bool), Some(true));
    Ok(())
}

#[test]
fn check_args_validation_preserves_backward_compat_and_usage_exit_codes() {
    use clap::{CommandFactory as _, Parser as _};

    crate::Cli::command().debug_assert();

    assert!(crate::Cli::try_parse_from(["norn", "check"]).is_err());
    assert!(crate::Cli::try_parse_from(["norn", "check", "--artifacts", "a.json"]).is_err());
    assert!(
        crate::Cli::try_parse_from([
            "norn",
            "check",
            "--artifacts",
            "a.json",
            "--context",
            "c.json",
            "--org",
            "org_demo"
        ])
        .is_err()
    );
    assert!(
        crate::Cli::try_parse_from(["norn", "check", "--source", "app", "--org", "org_demo"])
            .is_err()
    );
    assert!(
        crate::Cli::try_parse_from(["norn", "check", "--source", "app", "--env", "dev"]).is_err()
    );
    assert!(
        crate::Cli::try_parse_from([
            "norn",
            "check",
            "--artifacts",
            "a.json",
            "--context",
            "c.json"
        ])
        .is_ok()
    );
    assert!(crate::Cli::try_parse_from(["norn", "check", "--source", "app"]).is_ok());
    assert!(
        crate::Cli::try_parse_from([
            "norn", "check", "--source", "app", "--org", "org_demo", "--env", "dev"
        ])
        .is_ok()
    );

    let empty = run_check_args(&CheckArgs {
        artifacts: None,
        context: None,
        base: Vec::new(),
        source: None,
        org: None,
        env: None,
        forbid_unsafe_zen: false,
        tool_root: None,
    });
    assert_eq!(empty.exit_code, 2);

    let missing_context = run_check_args(&CheckArgs {
        artifacts: Some(PathBuf::from("artifacts.json")),
        context: None,
        base: Vec::new(),
        source: None,
        org: None,
        env: None,
        forbid_unsafe_zen: false,
        tool_root: None,
    });
    assert_eq!(missing_context.exit_code, 2);

    let half_scope = run_check_args(&CheckArgs {
        artifacts: None,
        context: None,
        base: Vec::new(),
        source: Some(PathBuf::from("app")),
        org: Some("org_demo".to_owned()),
        env: None,
        forbid_unsafe_zen: false,
        tool_root: None,
    });
    assert_eq!(half_scope.exit_code, 2);

    let extra_source_flag = run_check_args(&CheckArgs {
        artifacts: Some(PathBuf::from("artifacts.json")),
        context: Some(PathBuf::from("context.json")),
        base: Vec::new(),
        source: None,
        org: Some("org_demo".to_owned()),
        env: None,
        forbid_unsafe_zen: false,
        tool_root: None,
    });
    assert_eq!(extra_source_flag.exit_code, 2);
}

#[cfg(unix)]
#[test]
fn compile_and_fmt_refuse_symlink_inputs() -> TestResult {
    use std::os::unix::fs::symlink;

    let scratch = TempWorkspace::new("symlink")?;
    let target_python = scratch.write("real.py", "x = 1\n")?;
    let symlink_python = scratch.path().join("link.py");
    symlink(&target_python, &symlink_python)?;

    let compile_out = execute_compile(&CompileArgs {
        path: symlink_python.clone(),
        org: Some("org_demo".to_owned()),
        env: Some("dev".to_owned()),
        forbid_unsafe_zen: false,
        out: None,
        tool_root: None,
        pack_json: false,
        registry: None,
        root: None,
        source_path: None,
        modules: None,
        allow_empty: false,
        max_expansion_work: None,
    });
    assert_eq!(compile_out.exit_code, 2);
    assert!(compile_out.stderr.contains("symlink"));

    let fmt_out = execute_fmt(&FmtArgs {
        check: true,
        path: symlink_python,
    });
    assert_eq!(fmt_out.exit_code, 2);
    assert!(fmt_out.stderr.contains("symlink"));
    Ok(())
}

#[test]
fn ts_multiline_imports_compile_in_directory_and_single_file_modes() -> TestResult {
    let ts_root = tools_norn_ts_root();
    let scratch = TempWorkspace::new("ts-multiline-imports")?;
    scratch.write(
        "support/nested.ts",
        r#"import { Norn } from "@norn/authoring"

export const nestedHelper = Norn.fn<{
  readonly invoiceId: string
}, { readonly receiptId: string }>({
  id: "billing.multiline_nested",
  version: "1.0.0",
})
"#,
    )?;
    scratch.write(
        "support/helper.ts",
        r#"import { Norn } from "@norn/authoring"
export {
  nestedHelper,
}
  from "./\u{6e}ested"

export const helper = Norn.fn<{
  readonly invoiceId: string
}, { readonly id: string }>({
  id: "billing.multiline_helper",
  version: "1.0.0",
})
"#,
    )?;
    let flow_path = scratch.write(
        "flow.ts",
        r#"import { Effect } from "effect"
import { Norn } from "@norn/authoring"
// import { missingLine } from "./does-not-exist"
/*
  import { missingBlock }
    from "./also-missing"
*/
import {
  helper,
  nestedHelper,
}
  from "./support/\x68elper"

export default Norn.flow(
  { id: "billing.multiline_flow", version: "1.0.0" },
  Effect.gen(function* () {
    const first = yield* Norn.perform("first", helper, { invoiceId: "inv-1" })
    const second = yield* Norn.perform("second", nestedHelper, { invoiceId: first.id })
    return { invoice: first.id, receipt: second.receiptId }
  }),
)
"#,
    )?;

    for target in [scratch.path().to_path_buf(), flow_path] {
        let compiled = execute_compile(&CompileArgs {
            path: target,
            org: Some("local".to_owned()),
            env: Some("dev".to_owned()),
            forbid_unsafe_zen: false,
            out: None,
            tool_root: Some(ts_root.clone()),
            pack_json: false,
            registry: None,
            root: None,
            source_path: None,
            modules: None,
            allow_empty: false,
            max_expansion_work: None,
        });
        assert_eq!(
            compiled.exit_code, 0,
            "multiline TS import compile failed: stdout={} stderr={}",
            compiled.stdout, compiled.stderr
        );
        let parsed: Value = serde_json::from_str(&compiled.stdout)?;
        let items = parsed.as_array().ok_or("expected JSON artifact array")?;
        assert_eq!(items.len(), 2);
        let mut artifact_ids: Vec<&str> = items
            .iter()
            .filter_map(|item| item.get("artifactId").and_then(Value::as_str))
            .collect();
        artifact_ids.sort_unstable();
        assert_eq!(
            artifact_ids,
            vec![
                "billing.MultilineFlowRun.entity",
                "billing.multiline_flow.flow.rule",
            ]
        );
    }
    Ok(())
}
