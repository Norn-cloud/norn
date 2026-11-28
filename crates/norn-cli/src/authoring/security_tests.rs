//! Security, resource-budget, zero-CI ABI, and manifest source regression tests
//! for native Rust authoring commands in `norn-cli`.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

use super::atomic_io::write_file_atomic;
use super::fs_io::{BoundedJsonArray, BoundedJsonWriter, InvocationBudget, is_formattable_file};
use super::trusted_dir::TrustedDir;
use super::zero_ci::{
    ZERO_CI_DIAGNOSTIC_CODE, build_zero_ci_report, find_norn_app_root_from_dir_with_limit,
};
use super::{
    AuthoringFailure, CheckSourceRequest, CompileArgs, compile_path_to_json, execute_check_source,
    execute_compile,
};

type TestResult = Result<(), Box<dyn Error>>;

static SEC_SEQ: AtomicU64 = AtomicU64::new(0);

struct SecWorkspace(PathBuf);

impl SecWorkspace {
    fn new(label: &str) -> Result<Self, Box<dyn Error>> {
        let seq = SEC_SEQ.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("norn-cli-sec-{label}-{}-{seq}", std::process::id()));
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

impl Drop for SecWorkspace {
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

#[cfg(unix)]
#[test]
fn write_file_atomic_refuses_symlink_parent_and_symlink_final_component() -> TestResult {
    use std::os::unix::fs::symlink;

    let scratch = SecWorkspace::new("atomic-symlink")?;
    let outside = SecWorkspace::new("atomic-outside")?;
    let external_secret = outside.write("secret.txt", "original")?;

    // Symlinked parent directory must be refused before any temp file or rename happens.
    let link_dir = scratch.path().join("linked-dir");
    symlink(outside.path(), &link_dir)?;
    let via_parent_link = link_dir.join("secret.txt");
    let parent_err = write_file_atomic(&via_parent_link, "pwned");
    assert!(
        parent_err
            .as_ref()
            .is_err_and(|msg| msg.contains("symlink")),
        "expected symlink refusal for parent dir, got: {parent_err:?}"
    );
    assert_eq!(std::fs::read_to_string(&external_secret)?, "original");

    // Symlinked final component must be refused without overwriting target.
    let link_file = scratch.path().join("linked-file.txt");
    symlink(&external_secret, &link_file)?;
    let file_err = write_file_atomic(&link_file, "pwned");
    assert!(
        file_err.as_ref().is_err_and(|msg| msg.contains("symlink")),
        "expected symlink refusal for final component, got: {file_err:?}"
    );
    assert_eq!(std::fs::read_to_string(&external_secret)?, "original");

    // Normal atomic create + overwrite inside real directory succeeds cleanly.
    let real_file = scratch.path().join("sub/real.txt");
    std::fs::create_dir_all(scratch.path().join("sub"))?;
    write_file_atomic(&real_file, "v1")?;
    assert_eq!(std::fs::read_to_string(&real_file)?, "v1");
    write_file_atomic(&real_file, "v2")?;
    assert_eq!(std::fs::read_to_string(&real_file)?, "v2");
    Ok(())
}

#[test]
fn invocation_budget_enforces_scanned_entry_cap_and_shared_byte_cap() -> TestResult {
    let scratch = SecWorkspace::new("budget")?;
    for idx in 0..6_usize {
        scratch.write(&format!("noise_{idx}.txt"), "ignored-by-filter")?;
    }
    let dir = Arc::new(TrustedDir::open(scratch.path())?);
    let mut scan_budget = InvocationBudget::with_limits(4, 100, 1024, 4096, 4096);
    let scan_err = dir
        .walk_files_pinned(is_formattable_file, &mut scan_budget)
        .err();
    assert!(
        scan_err
            .as_deref()
            .is_some_and(|msg| msg.contains("entry invocation limit")),
        "expected scanned-entry cap error, got: {scan_err:?}"
    );
    assert_eq!(scan_budget.scanned_entries(), 4);

    // Shared byte budget accumulates across multiple files and separate source directories.
    let multi = SecWorkspace::new("budget-bytes")?;
    multi.write("first_batch/a.ts", "export const a = '1234567890';\n")?;
    multi.write("second_batch/b.ts", "export const b = '1234567890';\n")?;
    let root_dir = TrustedDir::open(multi.path())?;
    let first_batch = root_dir.open_subdir_rel(Path::new("first_batch"))?;
    let second_batch = root_dir.open_subdir_rel(Path::new("second_batch"))?;

    let mut byte_budget = InvocationBudget::with_limits(100, 100, 64, 45, 4096);
    let loaded_first = first_batch.scan_top_level(&mut byte_budget)?;
    assert_eq!(loaded_first.files.len(), 1);
    assert_eq!(byte_budget.bytes_read(), 31);

    let byte_err = second_batch.scan_top_level(&mut byte_budget);
    assert!(
        byte_err
            .as_ref()
            .is_err_and(|msg| msg.contains("byte read limit")),
        "expected shared invocation byte read limit error, got: {byte_err:?}"
    );
    Ok(())
}

#[test]
fn zero_ci_report_matches_exact_checker_abi_check_v1_schema() -> TestResult {
    let report_text = build_zero_ci_report(".github/workflows");
    let report: Value = serde_json::from_str(&report_text)?;
    let version: Value = serde_json::from_str(&norn_checker_wasm::version_json())?;

    assert_eq!(
        report.get("format").and_then(Value::as_str),
        Some(norn_checker_wasm::CHECK_FORMAT)
    );
    assert_eq!(report.get("admitted").and_then(Value::as_bool), Some(false));
    assert!(report.get("receipt").is_some_and(Value::is_null));
    assert!(report.get("receiptSha256").is_some_and(Value::is_null));

    let diag_doc = report
        .get("diagnostics")
        .and_then(Value::as_object)
        .ok_or("diagnostics must be an object matching norn-checker/diagnostics@1")?;
    assert_eq!(
        diag_doc.get("format").and_then(Value::as_str),
        version.get("diagnosticsFormat").and_then(Value::as_str)
    );
    assert_eq!(
        diag_doc.get("checkerVersion").and_then(Value::as_str),
        version.get("checkerVersion").and_then(Value::as_str)
    );
    let items = diag_doc
        .get("diagnostics")
        .and_then(Value::as_array)
        .ok_or("diagnostics.diagnostics must be an array")?;
    assert_eq!(items.len(), 1);
    let first = items.first().ok_or("missing diagnostic entry")?;
    assert_eq!(
        first.get("code").and_then(Value::as_str),
        Some(ZERO_CI_DIAGNOSTIC_CODE)
    );
    assert!(first.get("tier").is_some_and(Value::is_null));
    assert_eq!(first.get("severity").and_then(Value::as_str), Some("error"));
    assert!(first.get("message").and_then(Value::as_str).is_some());
    assert_eq!(
        first.pointer("/span/pointer").and_then(Value::as_str),
        Some("/.github/workflows")
    );
    assert!(first.pointer("/span/source").is_some_and(Value::is_null));
    assert!(first.get("artifactId").is_some_and(Value::is_null));
    assert!(first.get("remediation").is_some_and(Value::is_null));
    Ok(())
}

#[cfg(unix)]
#[test]
fn zero_ci_fails_closed_on_github_symlinks_and_non_dir_workflows() -> TestResult {
    use std::os::unix::fs::symlink;

    let scratch = SecWorkspace::new("zero-ci-edge")?;
    scratch.write(
        "norn.json",
        &json!({
            "format": "norn-project/1",
            "name": "edge-app",
            "sources": ["src"],
            "scope": { "orgId": "local", "environmentId": "dev" },
            "dev": { "port": 4174, "actor": "local:developer" },
            "cell": ".norn/cell.sqlite",
        })
        .to_string(),
    )?;
    scratch.write("src/app.py", "x = 1\n")?;

    // Case 1: `.github` is a symlink -> must fail closed (exit 2), never treated as absent.
    let decoy = SecWorkspace::new("zero-ci-decoy")?;
    let github_path = scratch.path().join(".github");
    symlink(decoy.path(), &github_path)?;
    let sym_github = execute_check_source(&CheckSourceRequest {
        source: scratch.path(),
        context: None,
        org: None,
        env: None,
        forbid_unsafe_zen: false,
        tool_root: None,
    });
    assert_eq!(sym_github.exit_code, 2);
    assert_eq!(sym_github.stdout, "");
    assert!(sym_github.stderr.contains("symlink"));
    std::fs::remove_file(&github_path)?;

    // Case 2: `.github/workflows` is a regular file -> fail-closed inspection error (exit 2), NOT policy refusal.
    scratch.write(".github/workflows", "not a directory\n")?;
    let file_workflows = execute_check_source(&CheckSourceRequest {
        source: scratch.path(),
        context: None,
        org: None,
        env: None,
        forbid_unsafe_zen: false,
        tool_root: None,
    });
    assert_eq!(file_workflows.exit_code, 2);
    assert_eq!(file_workflows.stdout, "");
    assert!(
        file_workflows.stderr.contains("not a directory"),
        "expected .github/workflows not a directory error, got: {}",
        file_workflows.stderr
    );
    std::fs::remove_file(scratch.path().join(".github/workflows"))?;

    // Case 3: `.github/workflows` is a symlink -> fail-closed inspection error (exit 2), NOT policy refusal.
    symlink(decoy.path(), scratch.path().join(".github/workflows"))?;
    let sym_workflows = execute_check_source(&CheckSourceRequest {
        source: scratch.path(),
        context: None,
        org: None,
        env: None,
        forbid_unsafe_zen: false,
        tool_root: None,
    });
    assert_eq!(sym_workflows.exit_code, 2);
    assert_eq!(sym_workflows.stdout, "");
    assert!(
        sym_workflows.stderr.contains("symlink"),
        "expected .github/workflows symlink refusal, got: {}",
        sym_workflows.stderr
    );
    std::fs::remove_dir_all(scratch.path().join(".github"))?;

    // Case 4: `.github` is a regular file -> must fail closed (exit 2), never treated as clean.
    scratch.write(".github", "regular file masquerading as .github\n")?;
    let regular_github = execute_check_source(&CheckSourceRequest {
        source: scratch.path(),
        context: None,
        org: None,
        env: None,
        forbid_unsafe_zen: false,
        tool_root: None,
    });
    assert_eq!(regular_github.exit_code, 2);
    assert_eq!(regular_github.stdout, "");
    assert!(
        regular_github.stderr.contains("not a directory"),
        "expected .github not a directory error, got: {}",
        regular_github.stderr
    );
    Ok(())
}

#[test]
fn zero_ci_ancestor_search_fails_closed_on_depth_limit_and_non_regular_manifest() -> TestResult {
    let scratch = SecWorkspace::new("zero-ci-depth")?;
    let nested = scratch.path().join("a/b/c");
    std::fs::create_dir_all(&nested)?;
    let nested_dir = TrustedDir::open(&nested)?;

    // Exhausting ancestor depth without reaching filesystem root must return Err, never Ok(None).
    let depth_err = find_norn_app_root_from_dir_with_limit(&nested_dir, 1);
    assert!(
        depth_err
            .as_ref()
            .is_err_and(|msg| msg.contains("exceeded 1 levels")),
        "expected depth-limit refusal, got: {depth_err:?}"
    );

    // A non-regular `norn.json` (directory) must fail closed, never be ignored as absent.
    let dir_manifest = scratch.path().join("a/b/norn.json");
    std::fs::create_dir_all(&dir_manifest)?;
    let non_regular_err = find_norn_app_root_from_dir_with_limit(&nested_dir, 4);
    assert!(
        non_regular_err
            .as_ref()
            .is_err_and(|msg| msg.contains("not a regular file")),
        "expected non-regular norn.json refusal, got: {non_regular_err:?}"
    );
    Ok(())
}

#[test]
fn bounded_json_writer_aborts_mid_serialization_before_exceeding_limit() -> TestResult {
    let mut budget = InvocationBudget::with_limits(100, 100, 1024, 4096, 32);
    let mut buf = Vec::new();
    let oversized = json!({
        "artifactId": "demo.Large.entity",
        "payload": "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ",
    });

    let write_res = {
        let mut writer = BoundedJsonWriter::new("test output", &mut buf, &mut budget);
        super::fs_io::write_canonical_json_value(&mut writer, &oversized)
    };
    assert!(
        write_res
            .as_ref()
            .is_err_and(|err| err.contains("output limit")),
        "expected bounded writer overflow error, got: {write_res:?}"
    );
    assert!(
        buf.len() <= 32,
        "buffer must never exceed output budget cap, got {} bytes",
        buf.len()
    );

    // Streaming JSON array helper must also abort before exceeding output cap.
    let mut array_budget = InvocationBudget::with_limits(100, 100, 1024, 4096, 24);
    let mut array = BoundedJsonArray::new();
    let array_err = array.push_value("test array", &oversized, &mut array_budget);
    assert!(
        array_err
            .as_ref()
            .is_err_and(|msg| msg.contains("output limit")),
        "expected BoundedJsonArray overflow error, got: {array_err:?}"
    );
    Ok(())
}

#[test]
fn check_source_combines_ts_and_precompiled_json_pack_dirs() -> TestResult {
    let ts_root = tools_norn_ts_root();
    let scratch = SecWorkspace::new("mixed-pack")?;
    let core_units =
        std::fs::read_to_string(repo_root().join("registry/core/artifacts/core.units.json"))?;
    scratch.write("definitions/core/core.units.json", &core_units)?;
    scratch.write(
        "definitions/questions/demo.weekly.json",
        &json!({
            "kind": "question",
            "id": "demo.weekly",
            "payload": {
                "irVersion": "0.1",
                "name": "demo.weekly",
                "objectSet": "shop.Product",
                "aggregate": [{ "metric": "demo.count" }],
                "window": {
                    "start": "2026-07-06T00:00:00Z",
                    "end": "2026-10-26T00:00:00Z"
                },
                "calendar": "iso_utc",
                "freshness": {
                    "kind": "live",
                    "maxAgeMs": 5000
                }
            }
        })
        .to_string(),
    )?;
    scratch.write(
        "src/product.ts",
        r#"import { Norn } from "@norn/authoring"
import { Schema } from "effect"

export const Product = Norn.type({
  name: "shop.Product",
  title: "Product",
  version: "0.1.0",
  properties: Schema.Struct({ sku: Schema.String, name: Schema.String, price: Norn.decimal(2) }),
  identity: [{ name: "primary", properties: ["sku"] }],
})
"#,
    )?;
    scratch.write(
        "norn.json",
        &json!({
            "format": "norn-project/1",
            "name": "mixed-pack",
            "sources": ["src", "definitions/core", "definitions/questions"],
            "scope": { "orgId": "local", "environmentId": "dev" },
            "dev": { "port": 4174, "actor": "local:developer" },
            "cell": ".norn/cell.sqlite",
        })
        .to_string(),
    )?;

    let checked = execute_check_source(&CheckSourceRequest {
        source: scratch.path(),
        context: None,
        org: None,
        env: None,
        forbid_unsafe_zen: false,
        tool_root: Some(&ts_root),
    });
    assert_eq!(
        checked.exit_code, 0,
        "mixed TS + precompiled JSON pack check failed: stdout={} stderr={}",
        checked.stdout, checked.stderr
    );
    let report: Value = serde_json::from_str(&checked.stdout)?;
    assert_eq!(report.get("admitted").and_then(Value::as_bool), Some(true));
    Ok(())
}

#[test]
fn compile_rejects_file_source_and_unsupported_json_in_manifest() -> TestResult {
    let ts_root = tools_norn_ts_root();

    // A regular file listed in `sources` (instead of a directory) must be explicitly rejected.
    let bad_file_source = SecWorkspace::new("bad-file-source")?;
    bad_file_source.write("src/product.ts", "export const x = 1\n")?;
    bad_file_source.write(
        "norn.json",
        &json!({
            "format": "norn-project/1",
            "name": "bad-file-source",
            "sources": ["src/product.ts"],
            "scope": { "orgId": "local", "environmentId": "dev" },
        })
        .to_string(),
    )?;
    let file_source_out = execute_compile(&CompileArgs {
        path: bad_file_source.path().to_path_buf(),
        org: "local".to_owned(),
        env: "dev".to_owned(),
        forbid_unsafe_zen: false,
        out: None,
        tool_root: Some(ts_root.clone()),
    });
    assert_eq!(file_source_out.exit_code, 2);
    assert!(
        file_source_out.stderr.contains("not a directory"),
        "expected not a directory error, got: {}",
        file_source_out.stderr
    );

    // An unsupported JSON file inside a source directory must be explicitly rejected.
    let bad_json = SecWorkspace::new("bad-json-source")?;
    bad_json.write("definitions/core/junk.json", r#"{"hello":"world"}"#)?;
    bad_json.write(
        "norn.json",
        &json!({
            "format": "norn-project/1",
            "name": "bad-json-source",
            "sources": ["definitions/core"],
            "scope": { "orgId": "local", "environmentId": "dev" },
        })
        .to_string(),
    )?;
    let bad_json_out = execute_compile(&CompileArgs {
        path: bad_json.path().to_path_buf(),
        org: "local".to_owned(),
        env: "dev".to_owned(),
        forbid_unsafe_zen: false,
        out: None,
        tool_root: Some(ts_root),
    });
    assert_eq!(bad_json_out.exit_code, 2);
    assert!(
        bad_json_out.stderr.contains("unsupported JSON source"),
        "expected unsupported JSON error, got: {}",
        bad_json_out.stderr
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn norn_json_sources_refuse_symlink_and_parent_escape() -> TestResult {
    use std::os::unix::fs::symlink;

    let scratch = SecWorkspace::new("manifest-symlink")?;
    let outside = SecWorkspace::new("manifest-outside")?;
    outside.write("evil.py", "x = 1\n")?;
    symlink(outside.path(), scratch.path().join("src"))?;
    scratch.write(
        "norn.json",
        &json!({
            "format": "norn-project/1",
            "name": "escape-app",
            "sources": ["src"],
            "scope": { "orgId": "local", "environmentId": "dev" },
        })
        .to_string(),
    )?;

    let refused = execute_compile(&CompileArgs {
        path: scratch.path().to_path_buf(),
        org: "local".to_owned(),
        env: "dev".to_owned(),
        forbid_unsafe_zen: false,
        out: None,
        tool_root: None,
    });
    assert_eq!(refused.exit_code, 2);
    assert!(
        refused.stderr.contains("symlink"),
        "expected symlink refusal, got: {}",
        refused.stderr
    );
    Ok(())
}

#[test]
fn retained_definitions_refuse_empty_payload_for_all_kinds() -> TestResult {
    for kind in ["question", "dashboard", "approval"] {
        let scratch = SecWorkspace::new(&format!("empty-retained-{kind}"))?;
        scratch.write(
            &format!("definitions/{kind}.json"),
            &json!({
                "kind": kind,
                "id": format!("demo.empty_{kind}"),
                "payload": {},
            })
            .to_string(),
        )?;
        scratch.write(
            "norn.json",
            &json!({
                "format": "norn-project/1",
                "name": "empty-retained",
                "sources": ["definitions"],
                "scope": { "orgId": "local", "environmentId": "dev" },
            })
            .to_string(),
        )?;

        let refused = execute_compile(&CompileArgs {
            path: scratch.path().to_path_buf(),
            org: "local".to_owned(),
            env: "dev".to_owned(),
            forbid_unsafe_zen: false,
            out: None,
            tool_root: None,
        });
        assert_eq!(
            refused.exit_code, 2,
            "expected empty {kind} payload refusal, got stdout={} stderr={}",
            refused.stdout, refused.stderr
        );
        assert!(
            refused
                .stderr
                .contains(&format!("invalid retained {kind} definition")),
            "expected structural validation error for {kind}, got: {}",
            refused.stderr
        );
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn manifest_source_dir_refuses_hidden_symlink_and_hidden_unsupported_file() -> TestResult {
    use std::os::unix::fs::symlink;

    // Hidden symlink inside a manifest source directory must fail closed, never be skipped.
    let sym_ws = SecWorkspace::new("hidden-symlink")?;
    let outside = SecWorkspace::new("hidden-outside")?;
    let target = outside.write("evil.py", "x = 1\n")?;
    sym_ws.write("src/ok.py", "x = 1\n")?;
    symlink(&target, sym_ws.path().join("src/.hidden.py"))?;
    sym_ws.write(
        "norn.json",
        &json!({
            "format": "norn-project/1",
            "name": "hidden-sym-app",
            "sources": ["src"],
            "scope": { "orgId": "local", "environmentId": "dev" },
        })
        .to_string(),
    )?;
    let sym_out = execute_compile(&CompileArgs {
        path: sym_ws.path().to_path_buf(),
        org: "local".to_owned(),
        env: "dev".to_owned(),
        forbid_unsafe_zen: false,
        out: None,
        tool_root: None,
    });
    assert_eq!(sym_out.exit_code, 2);
    assert!(
        sym_out.stderr.contains("symlink"),
        "expected hidden symlink refusal, got: {}",
        sym_out.stderr
    );

    // Unsupported hidden regular file inside a manifest source directory must be refused.
    let junk_ws = SecWorkspace::new("hidden-junk")?;
    junk_ws.write("src/.DS_Store", "junk\n")?;
    junk_ws.write(
        "norn.json",
        &json!({
            "format": "norn-project/1",
            "name": "hidden-junk-app",
            "sources": ["src"],
            "scope": { "orgId": "local", "environmentId": "dev" },
        })
        .to_string(),
    )?;
    let junk_out = execute_compile(&CompileArgs {
        path: junk_ws.path().to_path_buf(),
        org: "local".to_owned(),
        env: "dev".to_owned(),
        forbid_unsafe_zen: false,
        out: None,
        tool_root: None,
    });
    assert_eq!(junk_out.exit_code, 2);
    assert!(
        junk_out.stderr.contains("unsupported file"),
        "expected unsupported hidden file refusal, got: {}",
        junk_out.stderr
    );
    Ok(())
}

#[test]
fn ts_directory_compile_charges_single_read_budget_and_resolves_imports() -> TestResult {
    let ts_root = tools_norn_ts_root();
    let scratch = SecWorkspace::new("ts-single-read")?;
    let billing_src = r#"import { Norn } from "@norn/authoring"

export const loadInvoice = Norn.fn<{
  readonly invoiceId: string
}, {
  readonly id: string
}>({ id: "billing.load_invoice", version: "1.0.0" })
"#;
    let charge_src = r#"import { Norn } from "@norn/authoring"

export const chargeCard = Norn.fn<{
  readonly invoiceId: string
}, { readonly receiptId: string }>({
  id: "billing.charge_card",
  version: "1.0.0",
})
"#;
    let sequence_src = r#"import { Effect } from "effect"
import { Norn } from "@norn/authoring"
import { loadInvoice } from "./billing"
import { chargeCard } from "./support/charge"

export default Norn.flow(
  { id: "billing.sequence", version: "1.0.0" },
  Effect.gen(function* () {
    const invoice = yield* Norn.perform("load", loadInvoice, { invoiceId: "inv-1" })
    const receipt = yield* Norn.perform("charge", chargeCard, { invoiceId: invoice.id })
    return { invoice: invoice.id, receipt: receipt.receiptId }
  }),
)
"#;
    scratch.write("billing.ts", billing_src)?;
    scratch.write("support/charge.ts", charge_src)?;
    scratch.write("sequence.ts", sequence_src)?;

    let total_len = billing_src
        .len()
        .saturating_add(charge_src.len())
        .saturating_add(sequence_src.len());
    let expected_bytes = u64::try_from(total_len)?;
    // Exact entry cap: 3 root entries (`billing.ts`, `sequence.ts`, `support`) + 1 subdir entry (`charge.ts`).
    let mut budget = InvocationBudget::with_limits(4, 3, 65_536, expected_bytes, 65_536);

    let compiled = match compile_path_to_json(
        scratch.path(),
        "local",
        "dev",
        false,
        Some(&ts_root),
        &mut budget,
    ) {
        Ok(text) => text,
        Err(AuthoringFailure::Diagnostics(doc)) => {
            return Err(format!("unexpected TS diagnostics: {doc}").into());
        }
        Err(AuthoringFailure::Io(msg)) => return Err(msg.into()),
    };
    assert_eq!(budget.scanned_entries(), 4);
    assert_eq!(budget.bytes_read(), expected_bytes);
    let parsed: Value = serde_json::from_str(&compiled)?;
    let items = parsed.as_array().ok_or("expected JSON artifact array")?;
    assert_eq!(items.len(), 1);
    Ok(())
}

#[test]
fn ts_directory_compile_reuses_preloaded_top_level_json_without_double_read() -> TestResult {
    let ts_root = tools_norn_ts_root();
    let scratch = SecWorkspace::new("ts-top-json-single-read")?;
    let config_json = "{\"version\":\"1.0.0\"}\n";
    let importer_src = r#"import { Norn } from "@norn/authoring"
import config from "./config.json"

export const ping = Norn.fn<{ readonly v: string }, { readonly ok: boolean }>({
  id: "demo.ping",
  version: config.version,
})
"#;
    scratch.write("config.json", config_json)?;
    scratch.write("importer.ts", importer_src)?;

    let expected_bytes = u64::try_from(config_json.len().saturating_add(importer_src.len()))?;
    let mut budget = InvocationBudget::with_limits(2, 2, 65_536, expected_bytes, 65_536);
    let outcome = compile_path_to_json(
        scratch.path(),
        "local",
        "dev",
        false,
        Some(&ts_root),
        &mut budget,
    );
    assert!(
        matches!(outcome, Err(AuthoringFailure::Diagnostics(_))),
        "expected profile diagnostics without IO/budget double-read for top-level .json import"
    );
    assert_eq!(budget.scanned_entries(), 2);
    assert_eq!(budget.bytes_read(), expected_bytes);
    Ok(())
}

#[test]
fn bounded_json_array_emits_canonical_json_with_sorted_keys_and_zero_norm() -> TestResult {
    let mut budget = InvocationBudget::new();
    let mut array = BoundedJsonArray::new();
    let sample = json!({
        "b": { "z": -0.0, "a": 1.0 },
        "a": 1e21,
        "10": 1.0,
        "2": 1,
        "01": -0.0,
        "0": -0.0,
    });
    array.push_value("canonical test", &sample, &mut budget)?;
    let rendered = array.finish("canonical test", &mut budget)?;
    assert_eq!(
        rendered,
        "[{\"0\":0,\"2\":1,\"10\":1,\"01\":0,\"a\":1e+21,\"b\":{\"a\":1,\"z\":0}}]\n"
    );

    let mut proto_budget = InvocationBudget::new();
    let mut proto_array = BoundedJsonArray::new();
    let proto_err =
        proto_array.push_value("proto test", &json!({ "__proto__": 1 }), &mut proto_budget);
    assert!(
        proto_err
            .as_ref()
            .is_err_and(|msg| msg.contains("__proto__")),
        "expected __proto__ refusal, got: {proto_err:?}"
    );

    let mut out_of_range_budget = InvocationBudget::new();
    let mut out_of_range_array = BoundedJsonArray::new();
    let out_of_range_err = out_of_range_array.push_value(
        "out-of-range int test",
        &json!({ "big": 9_007_199_254_740_993_u64 }),
        &mut out_of_range_budget,
    );
    assert!(
        out_of_range_err
            .as_ref()
            .is_err_and(|msg| msg.contains("safe integer")),
        "expected safe integer refusal, got: {out_of_range_err:?}"
    );
    Ok(())
}
