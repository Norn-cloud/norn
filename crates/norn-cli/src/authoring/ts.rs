//! Native Rust `TypeScript` authoring compilation, profiling, questions, and dashboards
//! backed by `norn-authoring` (`tsrs`, zero Node/Bun/`tsc`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use norn_authoring::{
    AuthoringProgram, BoundedVfs, CompilerLimits, DashboardsCompileOutcome,
    DefinitionsCompileOptions, DefinitionsCompileOutcome, EnvelopeOptions, FlowCompileOptions,
    FlowCompileOutcome, PINNED_EFFECT_DIST_PREFIX, Profile, ProfileChecker, ProfileDiagnostic,
    QuestionsCompileOutcome, canonical_json, compile_dashboards_vfs, compile_definitions_program,
    compile_flow_program, compile_questions_vfs,
};
use serde_json::{Value, json};

use super::AuthoringFailure;
use super::fs_io::{
    EntryKind, SourceLang, classify_source_file, collect_walk_files, discover_tool_root,
    display_posix_path, inspect_path_no_follow, is_ts_or_json_support, read_source_file_bounded,
    write_file_atomic,
};

const MAX_LOCAL_IMPORTS: usize = 128;
const MAX_LOCAL_IMPORT_DEPTH: usize = 16;

struct MountedTsContext {
    profile: Profile,
    vfs: BoundedVfs,
    effect_dist_prefix: Option<&'static str>,
    root_vpaths: Vec<String>,
    source_paths: BTreeMap<String, String>,
}

fn diagnostics_json(file: &str, diagnostics: &[ProfileDiagnostic]) -> String {
    json!({
        "ok": false,
        "file": file,
        "diagnostics": diagnostics,
    })
    .to_string()
}

fn mount_pinned_packages(
    vfs: &mut BoundedVfs,
    profile: &Profile,
    start: &Path,
    explicit_tool_root: Option<&Path>,
) -> Result<Option<&'static str>, AuthoringFailure> {
    let Some(tool_root) =
        discover_tool_root(start, explicit_tool_root).map_err(AuthoringFailure::Io)?
    else {
        return Ok(None);
    };
    let effect_dir = tool_root.join("node_modules/effect");
    let _effect_files = vfs
        .mount_pinned_effect_dir(&effect_dir, profile)
        .map_err(|err| AuthoringFailure::Io(err.to_string()))?;
    let ts_dir = tool_root.join("node_modules/typescript");
    if ts_dir.join("package.json").is_file() {
        let _ts_files = vfs
            .mount_pinned_package_dir("/node_modules/typescript", &ts_dir)
            .map_err(|err| AuthoringFailure::Io(err.to_string()))?;
    }
    Ok(Some(PINNED_EFFECT_DIST_PREFIX))
}

fn extract_relative_specifiers(source: &str) -> Vec<String> {
    let mut specs = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if !(trimmed.starts_with("import ") || trimmed.starts_with("export ")) {
            continue;
        }
        if let Some((_, after_from)) = trimmed.split_once(" from ")
            && let Some(spec) = first_quoted_literal(after_from.trim())
            && (spec.starts_with("./") || spec.starts_with("../"))
        {
            specs.push(spec);
        }
    }
    specs
}

fn first_quoted_literal(text: &str) -> Option<String> {
    let quote = text.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let rest = text.get(1..)?;
    let (inner, _) = rest.split_once(quote)?;
    Some(inner.to_owned())
}

fn normalize_lexical_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            std::path::Component::RootDir => normalized.push(Path::new("/")),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    normalized.push("..");
                }
            }
            std::path::Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

fn resolve_local_specifier(importer: &Path, specifier: &str) -> Option<PathBuf> {
    let parent = importer.parent().unwrap_or_else(|| Path::new("."));
    let candidate = normalize_lexical_path(&parent.join(specifier));
    if candidate.is_file() {
        return Some(candidate);
    }
    for suffix in [".ts", ".tsx", ".json"] {
        let mut with_ext = candidate.as_os_str().to_os_string();
        with_ext.push(suffix);
        let probe = PathBuf::from(with_ext);
        if probe.is_file() {
            return Some(probe);
        }
    }
    let index_ts = candidate.join("index.ts");
    if index_ts.is_file() {
        return Some(index_ts);
    }
    None
}

fn common_base_dir(paths: &[PathBuf]) -> PathBuf {
    let Some(first) = paths.first() else {
        return PathBuf::from(".");
    };
    let mut base = first
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    for path in paths.iter().skip(1) {
        let dir = path
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        while !dir.starts_with(&base) {
            if !base.pop() {
                return PathBuf::from(".");
            }
        }
    }
    base
}

fn virtual_path_for(base: &Path, file: &Path) -> Result<String, AuthoringFailure> {
    let rel = file.strip_prefix(base).unwrap_or(file);
    let mut parts = Vec::new();
    for comp in rel.components() {
        if let std::path::Component::Normal(seg) = comp {
            let text = seg.to_str().ok_or_else(|| {
                AuthoringFailure::Io(format!("{}: non-UTF-8 path component", file.display()))
            })?;
            parts.push(text);
        }
    }
    if parts.is_empty() {
        return Err(AuthoringFailure::Io(format!(
            "{}: cannot map path into virtual filesystem",
            file.display()
        )));
    }
    Ok(format!("/workspace/{}", parts.join("/")))
}

fn mount_single_file_closure(
    file: &Path,
    explicit_tool_root: Option<&Path>,
) -> Result<MountedTsContext, AuthoringFailure> {
    let profile = Profile::load().map_err(|err| AuthoringFailure::Io(err.to_string()))?;
    let mut vfs = BoundedVfs::default();
    let effect_dist_prefix = mount_pinned_packages(&mut vfs, &profile, file, explicit_tool_root)?;

    let mut queue: Vec<(PathBuf, usize)> = vec![(file.to_path_buf(), 0)];
    let mut visited: BTreeSet<PathBuf> = BTreeSet::new();
    let mut loaded: Vec<(PathBuf, String)> = Vec::new();

    while let Some((current, depth)) = queue.pop() {
        if !visited.insert(current.clone()) {
            continue;
        }
        if loaded.len() >= MAX_LOCAL_IMPORTS {
            return Err(AuthoringFailure::Io(format!(
                "{}: local import closure exceeds {MAX_LOCAL_IMPORTS} files",
                file.display()
            )));
        }
        let text = read_source_file_bounded(&current).map_err(AuthoringFailure::Io)?;
        if depth < MAX_LOCAL_IMPORT_DEPTH {
            for spec in extract_relative_specifiers(&text) {
                if let Some(target) = resolve_local_specifier(&current, &spec) {
                    queue.push((target, depth.saturating_add(1)));
                }
            }
        }
        loaded.push((current, text));
    }

    let disk_paths: Vec<PathBuf> = loaded.iter().map(|(path, _)| path.clone()).collect();
    let base = common_base_dir(&disk_paths);
    let mut source_paths = BTreeMap::new();
    let mut primary_vpath = String::new();
    for (idx, (disk_path, text)) in loaded.into_iter().enumerate() {
        let vpath = virtual_path_for(&base, &disk_path)?;
        let normalized = vfs
            .add_file(&vpath, text)
            .map_err(|err| AuthoringFailure::Io(err.to_string()))?;
        if idx == 0 {
            primary_vpath.clone_from(&normalized);
        }
        source_paths.insert(normalized, display_posix_path(&disk_path));
    }

    Ok(MountedTsContext {
        profile,
        vfs,
        effect_dist_prefix,
        root_vpaths: vec![primary_vpath],
        source_paths,
    })
}

fn mount_ts_files_and_support(
    root_dir: &Path,
    root_files: &[PathBuf],
    explicit_tool_root: Option<&Path>,
) -> Result<MountedTsContext, AuthoringFailure> {
    let profile = Profile::load().map_err(|err| AuthoringFailure::Io(err.to_string()))?;
    let mut vfs = BoundedVfs::default();
    let effect_dist_prefix =
        mount_pinned_packages(&mut vfs, &profile, root_dir, explicit_tool_root)?;

    let support_files =
        collect_walk_files(root_dir, is_ts_or_json_support).map_err(AuthoringFailure::Io)?;
    let mut source_paths = BTreeMap::new();
    let mut disk_to_vpath = BTreeMap::new();
    for file in support_files {
        let text = read_source_file_bounded(&file).map_err(AuthoringFailure::Io)?;
        let vpath = virtual_path_for(root_dir, &file)?;
        let normalized = vfs
            .add_file(&vpath, text)
            .map_err(|err| AuthoringFailure::Io(err.to_string()))?;
        source_paths.insert(normalized.clone(), display_posix_path(&file));
        disk_to_vpath.insert(file, normalized);
    }

    let mut root_vpaths = Vec::with_capacity(root_files.len());
    for root_file in root_files {
        if let Some(vpath) = disk_to_vpath.get(root_file) {
            root_vpaths.push(vpath.clone());
        } else {
            let text = read_source_file_bounded(root_file).map_err(AuthoringFailure::Io)?;
            let vpath = virtual_path_for(root_dir, root_file)?;
            let normalized = vfs
                .add_file(&vpath, text)
                .map_err(|err| AuthoringFailure::Io(err.to_string()))?;
            source_paths.insert(normalized.clone(), display_posix_path(root_file));
            root_vpaths.push(normalized);
        }
    }

    Ok(MountedTsContext {
        profile,
        vfs,
        effect_dist_prefix,
        root_vpaths,
        source_paths,
    })
}

fn validate_single_ts_file(path: &Path) -> Result<(), AuthoringFailure> {
    let kind = inspect_path_no_follow(path).map_err(AuthoringFailure::Io)?;
    if kind != EntryKind::File {
        return Err(AuthoringFailure::Io(format!(
            "{}: expected a single TypeScript (.ts) file",
            path.display()
        )));
    }
    if classify_source_file(path) != Some(SourceLang::TypeScript) {
        return Err(AuthoringFailure::Io(format!(
            "{}: expected a TypeScript (.ts) authoring file",
            path.display()
        )));
    }
    Ok(())
}

fn append_unique_artifact(
    artifact: Value,
    source_path: &str,
    seen_ids: &mut BTreeSet<String>,
    out: &mut Vec<Value>,
) -> Result<(), AuthoringFailure> {
    if let Some(id) = artifact.get("artifactId").and_then(Value::as_str)
        && !seen_ids.insert(id.to_owned())
    {
        let refused = json!({
            "ok": false,
            "diagnostics": [{
                "code": "NORN-FLOW-LITERAL",
                "message": format!("duplicate artifactId `{id}` declared in `{source_path}`"),
                "file": source_path,
            }],
        })
        .to_string();
        return Err(AuthoringFailure::Diagnostics(refused));
    }
    out.push(artifact);
    Ok(())
}

struct FlowCollectConfig<'a> {
    checker: &'a ProfileChecker,
    program: &'a AuthoringProgram,
    scope: &'a BTreeMap<String, String>,
    forbid_unsafe_zen: bool,
    single_module: bool,
}

fn collect_module_artifacts(
    cfg: &FlowCollectConfig<'_>,
    vpath: &str,
    source_path: &str,
    outcome: DefinitionsCompileOutcome,
    seen_ids: &mut BTreeSet<String>,
    artifacts: &mut Vec<Value>,
) -> Result<Vec<Value>, AuthoringFailure> {
    let mut failures = Vec::new();
    match outcome {
        DefinitionsCompileOutcome::Err { diagnostics } => {
            failures.push(json!({ "file": source_path, "diagnostics": diagnostics }));
        }
        DefinitionsCompileOutcome::Ok(out) => {
            let before_count = artifacts.len();
            for artifact in out.artifacts {
                append_unique_artifact(artifact, source_path, seen_ids, artifacts)?;
            }
            if out.flows > 0 {
                let flow_opts = FlowCompileOptions {
                    envelope: EnvelopeOptions {
                        scope: cfg.scope.clone(),
                        source_path: source_path.to_owned(),
                        registry_package_path: None,
                    },
                    include_flow_source_map: false,
                    source_root: "/".to_owned(),
                    tool_root: "/".to_owned(),
                    forbid_unsafe_zen: cfg.forbid_unsafe_zen,
                    limits: CompilerLimits::default(),
                };
                let flow_outcomes =
                    compile_flow_program(cfg.checker, cfg.program, &[vpath], &flow_opts)
                        .map_err(|err| AuthoringFailure::Io(err.to_string()))?;
                for flow_outcome in flow_outcomes {
                    match flow_outcome {
                        FlowCompileOutcome::Ok(flow_out) => {
                            for artifact in flow_out.artifacts {
                                append_unique_artifact(artifact, source_path, seen_ids, artifacts)?;
                            }
                        }
                        FlowCompileOutcome::Err { diagnostics } => {
                            failures
                                .push(json!({ "file": source_path, "diagnostics": diagnostics }));
                        }
                    }
                }
            } else if artifacts.len() == before_count
                && out.retained.is_empty()
                && cfg.single_module
            {
                failures.push(json!({
                    "file": source_path,
                    "diagnostics": [{
                        "code": "NORN-FLOW-RESULT",
                        "message": "module declares no Norn definitions or flows",
                    }],
                }));
            }
        }
    }
    Ok(failures)
}

/// Compiles one `TypeScript` file or a directory's top-level `TypeScript` modules into canonical JSON IR.
pub(crate) fn compile_ts_targets(
    mount_root: Option<&Path>,
    ts_files: &[PathBuf],
    org: &str,
    env: &str,
    forbid_unsafe_zen: bool,
    tool_root: Option<&Path>,
) -> Result<String, AuthoringFailure> {
    let mounted = match mount_root {
        Some(dir) => mount_ts_files_and_support(dir, ts_files, tool_root)?,
        None => {
            let first = ts_files.first().ok_or_else(|| {
                AuthoringFailure::Io("no TypeScript source files provided".into())
            })?;
            mount_single_file_closure(first, tool_root)?
        }
    };
    let root_slices: Vec<&str> = mounted.root_vpaths.iter().map(String::as_str).collect();
    let checker = ProfileChecker::new(mounted.profile.clone());
    let mut program = AuthoringProgram::new(mounted.vfs, &root_slices, mounted.profile)
        .map_err(|err| AuthoringFailure::Io(err.to_string()))?;
    if let Some(prefix) = mounted.effect_dist_prefix {
        program = program.with_effect_dist_prefix(prefix);
    }

    let mut scope = BTreeMap::new();
    scope.insert("environmentId".to_owned(), env.to_owned());
    scope.insert("orgId".to_owned(), org.to_owned());

    let outcomes = compile_definitions_program(&checker, &program, &root_slices, |vpath| {
        let source_path = mounted
            .source_paths
            .get(vpath)
            .cloned()
            .unwrap_or_else(|| vpath.trim_start_matches('/').to_owned());
        DefinitionsCompileOptions {
            envelope: EnvelopeOptions {
                scope: scope.clone(),
                source_path,
                registry_package_path: None,
            },
            allow_empty: true,
            forbid_unsafe_zen: Some(forbid_unsafe_zen),
            tool_root: "/".to_owned(),
            limits: CompilerLimits::default(),
        }
    })
    .map_err(|err| AuthoringFailure::Io(err.to_string()))?;

    let cfg = FlowCollectConfig {
        checker: &checker,
        program: &program,
        scope: &scope,
        forbid_unsafe_zen,
        single_module: ts_files.len() == 1,
    };
    let mut artifacts = Vec::new();
    let mut seen_ids = BTreeSet::new();
    let mut failures = Vec::new();
    for (vpath, outcome) in mounted.root_vpaths.iter().zip(outcomes) {
        let source_path = mounted
            .source_paths
            .get(vpath)
            .map_or(vpath.as_str(), String::as_str);
        failures.extend(collect_module_artifacts(
            &cfg,
            vpath,
            source_path,
            outcome,
            &mut seen_ids,
            &mut artifacts,
        )?);
    }

    if !failures.is_empty() {
        let doc = json!({
            "ok": false,
            "failures": failures,
        })
        .to_string();
        return Err(AuthoringFailure::Diagnostics(doc));
    }

    canonical_json(&Value::Array(artifacts)).map_err(|err| AuthoringFailure::Io(err.to_string()))
}

/// Runs `ProfileChecker` on a single `TypeScript` file and returns `(reports_json, all_clean)`.
pub(crate) fn run_profile(
    path: &Path,
    tool_root: Option<&Path>,
) -> Result<(String, bool), AuthoringFailure> {
    validate_single_ts_file(path)?;
    let mounted = mount_single_file_closure(path, tool_root)?;
    let root_slices: Vec<&str> = mounted.root_vpaths.iter().map(String::as_str).collect();
    let checker = ProfileChecker::new(mounted.profile);
    let reports = checker
        .check_vfs(mounted.vfs, &root_slices, mounted.effect_dist_prefix)
        .map_err(|err| AuthoringFailure::Io(err.to_string()))?;
    let clean = reports.iter().all(|report| report.diagnostics.is_empty());
    let rendered =
        serde_json::to_string(&reports).map_err(|err| AuthoringFailure::Io(err.to_string()))?;
    Ok((rendered, clean))
}

/// Compiles standalone `Norn.question` declarations from a `TypeScript` file.
pub(crate) fn run_question(
    path: &Path,
    out: Option<&Path>,
    tool_root: Option<&Path>,
) -> Result<String, AuthoringFailure> {
    validate_single_ts_file(path)?;
    let mounted = mount_single_file_closure(path, tool_root)?;
    let vpath = mounted
        .root_vpaths
        .first()
        .ok_or_else(|| AuthoringFailure::Io("missing virtual question path".into()))?;
    let checker = ProfileChecker::new(mounted.profile);
    let outcome = compile_questions_vfs(
        &checker,
        mounted.vfs,
        vpath,
        mounted.effect_dist_prefix,
        CompilerLimits::default(),
    )
    .map_err(|err| AuthoringFailure::Io(err.to_string()))?;
    match outcome {
        QuestionsCompileOutcome::Ok { questions } => {
            let rendered = canonical_json(&Value::Array(questions))
                .map_err(|err| AuthoringFailure::Io(err.to_string()))?;
            if let Some(out_path) = out {
                write_file_atomic(out_path, &rendered).map_err(AuthoringFailure::Io)?;
                Ok(String::new())
            } else {
                Ok(rendered)
            }
        }
        QuestionsCompileOutcome::Err { diagnostics } => Err(AuthoringFailure::Diagnostics(
            diagnostics_json(&display_posix_path(path), &diagnostics),
        )),
    }
}

/// Compiles standalone `Norn.dashboard` declarations from a `TypeScript` file.
pub(crate) fn run_dashboard(
    path: &Path,
    out: Option<&Path>,
    tool_root: Option<&Path>,
) -> Result<String, AuthoringFailure> {
    validate_single_ts_file(path)?;
    let mounted = mount_single_file_closure(path, tool_root)?;
    let vpath = mounted
        .root_vpaths
        .first()
        .ok_or_else(|| AuthoringFailure::Io("missing virtual dashboard path".into()))?;
    let checker = ProfileChecker::new(mounted.profile);
    let outcome = compile_dashboards_vfs(
        &checker,
        mounted.vfs,
        vpath,
        mounted.effect_dist_prefix,
        CompilerLimits::default(),
    )
    .map_err(|err| AuthoringFailure::Io(err.to_string()))?;
    match outcome {
        DashboardsCompileOutcome::Ok { dashboards } => {
            let rendered = canonical_json(&Value::Array(dashboards))
                .map_err(|err| AuthoringFailure::Io(err.to_string()))?;
            if let Some(out_path) = out {
                write_file_atomic(out_path, &rendered).map_err(AuthoringFailure::Io)?;
                Ok(String::new())
            } else {
                Ok(rendered)
            }
        }
        DashboardsCompileOutcome::Err { diagnostics } => Err(AuthoringFailure::Diagnostics(
            diagnostics_json(&display_posix_path(path), &diagnostics),
        )),
    }
}
