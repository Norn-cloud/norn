//! Direct `--source` compilation and `norn.json` target profile resolution for
//! `norn cell plan` and `norn cell apply`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;
use serde_json::value::RawValue;

use crate::authoring::{
    AuthoringFailure, InvocationBudget, PinnedSourceTarget, TrustedDir, compile_source_to_json,
    find_norn_app_root_from_dir, open_source_target_pinned,
};
use crate::cell::{Artifact, Target};

/// Fully resolved remote cell connection target after combining CLI flags
/// with optional `norn.json` target profiles (CLI flags always win).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResolvedTarget {
    pub(crate) url: String,
    pub(crate) org: String,
    pub(crate) env: String,
    pub(crate) token_env: Option<String>,
    pub(crate) email: Option<String>,
    pub(crate) password_env: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct ManifestTargetProfile {
    url: Option<String>,
    org: Option<String>,
    env: Option<String>,
    token_env: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct ManifestTargets {
    default_target: Option<String>,
    scope_org: Option<String>,
    scope_env: Option<String>,
    profiles: BTreeMap<String, ManifestTargetProfile>,
}

/// Resolves `--url`, `--org`, `--env`, and optional `--token-env` from CLI
/// flags plus an optional `norn.json` target profile (`--target <NAME>` or
/// `defaultTarget`). Explicit CLI flags always override manifest values;
/// missing required fields fail closed.
pub(crate) fn resolve_target(target: &Target) -> Result<ResolvedTarget, String> {
    let manifest = load_enclosing_manifest_targets(target)?;

    let profile = select_target_profile(target.profile.as_deref(), manifest.as_ref())?;

    let url = pick_non_empty(
        target.url.as_deref(),
        profile.and_then(|p| p.url.as_deref()),
    )
    .ok_or_else(|| {
        "missing required --url (pass --url or configure targets.<name>.url in norn.json)"
            .to_owned()
    })?;
    let org = pick_non_empty(
        target.org.as_deref(),
        profile
            .and_then(|p| p.org.as_deref())
            .or_else(|| manifest.as_ref().and_then(|m| m.scope_org.as_deref())),
    )
    .ok_or_else(|| {
        "missing required --org (pass --org or configure targets.<name>.org / scope.orgId in norn.json)"
            .to_owned()
    })?;
    let env = pick_non_empty(
        target.env.as_deref(),
        profile
            .and_then(|p| p.env.as_deref())
            .or_else(|| manifest.as_ref().and_then(|m| m.scope_env.as_deref())),
    )
    .ok_or_else(|| {
        "missing required --env (pass --env or configure targets.<name>.env / scope.environmentId in norn.json)"
            .to_owned()
    })?;

    let token_env = if target.email.is_some() {
        None
    } else {
        pick_non_empty(
            target.token_env.as_deref(),
            profile.and_then(|p| p.token_env.as_deref()),
        )
    };

    Ok(ResolvedTarget {
        url,
        org,
        env,
        token_env,
        email: target.email.clone(),
        password_env: target.password_env.clone(),
    })
}

/// Compiles `--source <PATH>` in-process with the resolved `(org, env)` scope
/// and returns exact `RawValue` artifact JSON entries ready for cell plan/apply.
pub(crate) fn compile_source_artifacts(
    source_path: &Path,
    resolved: &ResolvedTarget,
    forbid_unsafe_zen: bool,
    tool_root: Option<&Path>,
) -> Result<Vec<Artifact>, String> {
    let compiled = compile_source_to_json(
        source_path,
        &resolved.org,
        &resolved.env,
        forbid_unsafe_zen,
        tool_root,
    )
    .map_err(|failure| match failure {
        AuthoringFailure::Diagnostics(doc) => doc,
        AuthoringFailure::Io(message) => message,
    })?;
    let raw_artifacts: Vec<Box<RawValue>> = serde_json::from_str(&compiled).map_err(|err| {
        format!(
            "{}: compiled output is not a JSON array: {err}",
            source_path.display()
        )
    })?;
    if raw_artifacts.is_empty() {
        return Err(format!(
            "{}: compiled zero artifacts",
            source_path.display()
        ));
    }
    Ok(raw_artifacts)
}

fn pick_non_empty(primary: Option<&str>, fallback: Option<&str>) -> Option<String> {
    primary
        .or(fallback)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn select_target_profile<'a>(
    requested: Option<&str>,
    manifest: Option<&'a ManifestTargets>,
) -> Result<Option<&'a ManifestTargetProfile>, String> {
    if let Some(name) = requested {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err("--target must be a non-empty target profile name".to_owned());
        }
        let targets = manifest.ok_or_else(|| {
            format!("--target `{trimmed}` requested, but no enclosing norn.json was found")
        })?;
        let profile = targets.profiles.get(trimmed).ok_or_else(|| {
            format!("target profile `{trimmed}` was not found in norn.json `targets`")
        })?;
        return Ok(Some(profile));
    }
    let Some(targets) = manifest else {
        return Ok(None);
    };
    let Some(default_name) = targets.default_target.as_deref() else {
        return Ok(None);
    };
    let profile = targets.profiles.get(default_name).ok_or_else(|| {
        format!("norn.json `defaultTarget` `{default_name}` was not found in `targets`")
    })?;
    Ok(Some(profile))
}

fn load_enclosing_manifest_targets(target: &Target) -> Result<Option<ManifestTargets>, String> {
    let seed = target
        .source
        .as_deref()
        .or_else(|| target.paths.first().map(PathBuf::as_path));
    if let Some(seed_path) = seed
        && let Some(found) = find_manifest_from_path(seed_path)?
    {
        return Ok(Some(found));
    }
    let cwd = std::env::current_dir().map_err(|err| format!("current directory: {err}"))?;
    find_manifest_from_path(&cwd)
}

fn find_manifest_from_path(path: &Path) -> Result<Option<ManifestTargets>, String> {
    let start_dir = match open_source_target_pinned(path) {
        Ok(PinnedSourceTarget::Directory(dir)) => dir,
        Ok(PinnedSourceTarget::File { parent_dir, .. }) => parent_dir,
        Err(err) => {
            // If the path itself is a symlink or invalid, fail closed.
            if err.contains("symlink") {
                return Err(err);
            }
            return Ok(None);
        }
    };
    let Some(app_root) = find_norn_app_root_from_dir(&start_dir, None)? else {
        return Ok(None);
    };
    read_manifest_targets(app_root.as_dir(&start_dir))
}

fn read_manifest_targets(app_root: &TrustedDir) -> Result<Option<ManifestTargets>, String> {
    let mut budget = InvocationBudget::new();
    let Some(raw) = app_root.try_read_regular_file_rel(Path::new("norn.json"), &mut budget)? else {
        return Ok(None);
    };
    let manifest_display = app_root.display_path().join("norn.json");
    let parsed: Value = serde_json::from_str(&raw)
        .map_err(|err| format!("{}: invalid JSON: {err}", manifest_display.display()))?;
    parse_manifest_targets(&parsed, &manifest_display).map(Some)
}

fn parse_manifest_targets(
    parsed: &Value,
    manifest_display: &Path,
) -> Result<ManifestTargets, String> {
    let default_target = if let Some(raw) = parsed.get("defaultTarget") {
        let Some(name) = raw.as_str().map(str::trim).filter(|s| !s.is_empty()) else {
            return Err(format!(
                "{}: `defaultTarget` must be a non-empty string",
                manifest_display.display()
            ));
        };
        Some(name.to_owned())
    } else {
        None
    };
    let scope_org = parsed
        .pointer("/scope/orgId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let scope_env = parsed
        .pointer("/scope/environmentId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);

    let mut profiles = BTreeMap::new();
    if let Some(raw_targets) = parsed.get("targets") {
        let map = raw_targets.as_object().ok_or_else(|| {
            format!(
                "{}: `targets` must be an object mapping target names to profiles",
                manifest_display.display()
            )
        })?;
        for (name, value) in map {
            if name.trim().is_empty() {
                return Err(format!(
                    "{}: target profile name must not be empty",
                    manifest_display.display()
                ));
            }
            let profile = parse_single_profile(name, value, manifest_display)?;
            profiles.insert(name.clone(), profile);
        }
    }

    if let Some(default_name) = default_target.as_deref()
        && !profiles.contains_key(default_name)
    {
        return Err(format!(
            "{}: `defaultTarget` `{default_name}` was not found in `targets`",
            manifest_display.display()
        ));
    }

    Ok(ManifestTargets {
        default_target,
        scope_org,
        scope_env,
        profiles,
    })
}

fn parse_single_profile(
    name: &str,
    value: &Value,
    manifest_display: &Path,
) -> Result<ManifestTargetProfile, String> {
    let obj = value.as_object().ok_or_else(|| {
        format!(
            "{}: target `{name}` must be a JSON object",
            manifest_display.display()
        )
    })?;
    let mut profile = ManifestTargetProfile::default();
    for (key, field_val) in obj {
        match key.as_str() {
            "url" => {
                profile.url = Some(non_empty_string_field(
                    name,
                    key,
                    field_val,
                    manifest_display,
                )?);
            }
            "org" => {
                profile.org = Some(non_empty_string_field(
                    name,
                    key,
                    field_val,
                    manifest_display,
                )?);
            }
            "env" => {
                profile.env = Some(non_empty_string_field(
                    name,
                    key,
                    field_val,
                    manifest_display,
                )?);
            }
            "tokenEnv" => {
                profile.token_env = Some(non_empty_string_field(
                    name,
                    key,
                    field_val,
                    manifest_display,
                )?);
            }
            "token" | "password" => {
                return Err(format!(
                    "{}: target `{name}` must not store raw secret `{key}` (use `tokenEnv` instead)",
                    manifest_display.display()
                ));
            }
            other => {
                return Err(format!(
                    "{}: target `{name}` has unsupported field `{other}`",
                    manifest_display.display()
                ));
            }
        }
    }
    Ok(profile)
}

fn non_empty_string_field(
    target_name: &str,
    field: &str,
    value: &Value,
    manifest_display: &Path,
) -> Result<String, String> {
    let Some(text) = value.as_str().map(str::trim).filter(|s| !s.is_empty()) else {
        return Err(format!(
            "{}: target `{target_name}.{field}` must be a non-empty string",
            manifest_display.display()
        ));
    };
    Ok(text.to_owned())
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use serde_json::{Value, json};

    use super::{compile_source_artifacts, resolve_target};
    use crate::cell::Target;

    type TestResult = Result<(), Box<dyn Error>>;

    static SEQ: AtomicU64 = AtomicU64::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Result<Self, Box<dyn Error>> {
            let seq = SEQ.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "norn-cli-cell-source-{label}-{}-{seq}",
                std::process::id()
            ));
            if root.exists() {
                std::fs::remove_dir_all(&root)?;
            }
            std::fs::create_dir_all(&root)?;
            Ok(Self(std::fs::canonicalize(&root)?))
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

    fn base_target() -> Target {
        Target {
            url: None,
            org: None,
            env: None,
            profile: None,
            source: None,
            forbid_unsafe_zen: false,
            tool_root: None,
            token_env: None,
            email: None,
            password_env: None,
            json: false,
            paths: Vec::new(),
        }
    }

    #[test]
    fn resolves_default_named_and_cli_overridden_targets_from_manifest() -> TestResult {
        let scratch = Scratch::new("targets")?;
        scratch.write(
            "norn.json",
            &json!({
                "format": "norn-project/1",
                "name": "demo-app",
                "sources": ["src"],
                "scope": { "orgId": "org_fallback", "environmentId": "dev_fallback" },
                "defaultTarget": "dev",
                "targets": {
                    "dev": {
                        "url": "http://127.0.0.1:8787",
                        "org": "org_dev",
                        "env": "dev",
                        "tokenEnv": "NORN_DEV_TOKEN"
                    },
                    "staging": {
                        "url": "https://staging.norn.cloud",
                        "org": "org_stage",
                        "env": "staging"
                    }
                }
            })
            .to_string(),
        )?;
        let src_dir = scratch.0.join("src");
        std::fs::create_dir_all(&src_dir)?;

        // Default target selection.
        let mut req = base_target();
        req.source = Some(scratch.0.clone());
        let dev = resolve_target(&req)?;
        assert_eq!(dev.url, "http://127.0.0.1:8787");
        assert_eq!(dev.org, "org_dev");
        assert_eq!(dev.env, "dev");
        assert_eq!(dev.token_env.as_deref(), Some("NORN_DEV_TOKEN"));

        // Explicit --target selects staging, and explicit --env overrides profile env.
        req.profile = Some("staging".to_owned());
        req.env = Some("preview".to_owned());
        let staging = resolve_target(&req)?;
        assert_eq!(staging.url, "https://staging.norn.cloud");
        assert_eq!(staging.org, "org_stage");
        assert_eq!(staging.env, "preview");
        assert_eq!(staging.token_env, None);

        // Missing named target fails closed.
        req.profile = Some("prod".to_owned());
        let err = resolve_target(&req)
            .err()
            .ok_or("missing target profile must fail closed")?;
        assert!(err.contains("prod"), "{err}");
        Ok(())
    }

    #[test]
    fn refuses_missing_fields_and_raw_secrets_in_manifest_targets() -> TestResult {
        let scratch = Scratch::new("bad-targets")?;
        scratch.write(
            "norn.json",
            &json!({
                "format": "norn-project/1",
                "name": "bad-secret-app",
                "sources": ["src"],
                "defaultTarget": "dev",
                "targets": {
                    "dev": {
                        "url": "https://alpha.norn.cloud",
                        "token": "raw-secret-forbidden"
                    }
                }
            })
            .to_string(),
        )?;
        let mut req = base_target();
        req.source = Some(scratch.0.clone());
        let err = resolve_target(&req)
            .err()
            .ok_or("raw secret in norn.json target must refuse")?;
        assert!(err.contains("tokenEnv"), "{err}");

        // Missing --url with no manifest fails closed.
        let lonely = Scratch::new("no-manifest")?;
        let mut bare = base_target();
        bare.source = Some(lonely.0.clone());
        let missing = resolve_target(&bare)
            .err()
            .ok_or("missing --url must fail closed")?;
        assert!(missing.contains("--url"), "{missing}");
        Ok(())
    }

    #[test]
    fn compiles_python_and_ts_source_into_raw_artifacts_for_cell_push() -> TestResult {
        let scratch = Scratch::new("compile-source")?;
        let py_file = scratch.write(
            "item.py",
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

        let mut req = base_target();
        req.url = Some("http://127.0.0.1:8787".to_owned());
        req.org = Some("org_push".to_owned());
        req.env = Some("staging".to_owned());
        req.source = Some(py_file.clone());
        let resolved = resolve_target(&req)?;
        let py_artifacts = compile_source_artifacts(&py_file, &resolved, false, None)?;
        assert_eq!(py_artifacts.len(), 1);
        let first_py: Value =
            serde_json::from_str(py_artifacts.first().ok_or("expected artifact")?.get())?;
        assert_eq!(
            first_py.get("artifactId").and_then(Value::as_str),
            Some("demo.Item.entity")
        );
        assert_eq!(
            first_py.pointer("/scope/orgId").and_then(Value::as_str),
            Some("org_push")
        );
        assert_eq!(
            first_py
                .pointer("/scope/environmentId")
                .and_then(Value::as_str),
            Some("staging")
        );

        let ts_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/norn-ts");
        let crm_ts = ts_root.join("fixtures/definitions/crm.source.ts");
        let ts_artifacts = compile_source_artifacts(&crm_ts, &resolved, false, Some(&ts_root))?;
        assert!(ts_artifacts.len() >= 2);
        Ok(())
    }

    #[test]
    fn cell_cli_accepts_source_and_target_and_rejects_mixed_source_and_paths() {
        use clap::Parser as _;

        assert!(crate::Cli::try_parse_from(["norn", "cell", "plan", "--source", "app.ts"]).is_ok());
        assert!(
            crate::Cli::try_parse_from([
                "norn", "cell", "apply", "--target", "staging", "--source", "app.ts",
            ])
            .is_ok()
        );
        assert!(
            crate::Cli::try_parse_from(["norn", "cell", "plan", "--source", "app.ts", "pack/"])
                .is_err()
        );
        assert!(crate::Cli::try_parse_from(["norn", "cell", "plan"]).is_err());
    }
}
