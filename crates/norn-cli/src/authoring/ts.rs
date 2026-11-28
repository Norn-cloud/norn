//! Native Rust `TypeScript` authoring compilation, profiling, questions, and dashboards
//! backed by `norn-authoring` (`tsrs`, zero Node/Bun/`tsc`).
//!
//! All source and relative-import reads run through FD-pinned [`TrustedDir`] handles
//! under an invocation-wide [`InvocationBudget`] without reopening pathnames.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use norn_authoring::{
    AuthoringProgram, BoundedVfs, CompilerLimits, DashboardsCompileOutcome,
    DefinitionsCompileOptions, DefinitionsCompileOutcome, EnvelopeOptions, FlowCompileOptions,
    FlowCompileOutcome, PINNED_EFFECT_DIST_PREFIX, Profile, ProfileChecker, ProfileDiagnostic,
    QuestionsCompileOutcome, compile_dashboards_vfs, compile_definitions_program,
    compile_flow_program, compile_questions_vfs,
};
use serde_json::{Value, json};

use super::AuthoringFailure;
use super::atomic_io::write_file_atomic;
use super::fs_io::{
    BoundedJsonArray, BoundedJsonWriter, InvocationBudget, SourceLang, classify_source_file,
    discover_tool_root, display_posix_path, is_ts_or_json_support,
};
use super::trusted_dir::{TrustedDir, open_regular_file_pinned};

const MAX_LOCAL_IMPORTS: usize = 128;
const MAX_LOCAL_IMPORT_DEPTH: usize = 16;

/// Shared compilation options for `TypeScript` authoring modules.
pub(crate) struct TsCompileOptions<'a> {
    pub(crate) org: &'a str,
    pub(crate) env: &'a str,
    pub(crate) forbid_unsafe_zen: bool,
    pub(crate) tool_root: Option<&'a Path>,
}

/// FD-pinned target passed to [`compile_ts_targets`].
pub(crate) enum TsCompileTarget<'a> {
    /// Single `TypeScript` file already read from its verified parent directory fd.
    SingleFile {
        parent_dir: &'a TrustedDir,
        file_name: &'a str,
        display_path: &'a Path,
        content: String,
    },
    /// Directory already opened as a trusted fd with top-level relative `.ts`/`.tsx` paths.
    Directory {
        root_dir: &'a TrustedDir,
        root_rel_paths: Vec<PathBuf>,
    },
}

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

fn normalize_lexical_relative(path: &Path) -> PathBuf {
    let mut up_count: usize = 0;
    let mut normals: Vec<&std::ffi::OsStr> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if normals.pop().is_none() {
                    up_count = up_count.saturating_add(1);
                }
            }
            Component::Normal(part) => normals.push(part),
            Component::RootDir | Component::Prefix(_) => {}
        }
    }
    let mut normalized = PathBuf::new();
    for _step in 0..up_count {
        normalized.push("..");
    }
    for part in normals {
        normalized.push(part);
    }
    normalized
}

fn normalize_display_join(base: &Path, rel: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in base.join(rel).components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            Component::Normal(part) => out.push(part),
        }
    }
    out
}

fn relative_candidates(importer_rel: &Path, specifier: &str) -> Vec<PathBuf> {
    let importer_dir = importer_rel.parent().unwrap_or_else(|| Path::new(""));
    let base = normalize_lexical_relative(&importer_dir.join(specifier));
    let mut probes = Vec::with_capacity(5);
    probes.push(base.clone());
    for suffix in [".ts", ".tsx", ".json"] {
        let mut with_ext = base.as_os_str().to_os_string();
        with_ext.push(suffix);
        probes.push(PathBuf::from(with_ext));
    }
    probes.push(base.join("index.ts"));
    probes
}

fn count_leading_parent_dirs(rel: &Path) -> usize {
    let mut count = 0_usize;
    for comp in rel.components() {
        if comp == Component::ParentDir {
            count = count.saturating_add(1);
        } else {
            break;
        }
    }
    count
}

fn virtual_path_from_rel(rel: &Path, up_levels: usize) -> Result<String, AuthoringFailure> {
    let mut parts: Vec<String> = (0..up_levels).map(|idx| format!("d{idx}")).collect();
    for comp in rel.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if parts.pop().is_none() {
                    return Err(AuthoringFailure::Io(format!(
                        "{}: relative path escaped virtual workspace",
                        rel.display()
                    )));
                }
            }
            Component::Normal(seg) => {
                let text = seg.to_str().ok_or_else(|| {
                    AuthoringFailure::Io(format!("{}: non-UTF-8 path component", rel.display()))
                })?;
                parts.push(text.to_owned());
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(AuthoringFailure::Io(format!(
                    "{}: expected relative path inside virtual workspace",
                    rel.display()
                )));
            }
        }
    }
    if parts.is_empty() {
        return Err(AuthoringFailure::Io(format!(
            "{}: cannot map path into virtual filesystem",
            rel.display()
        )));
    }
    Ok(format!("/workspace/{}", parts.join("/")))
}

fn mount_single_file_closure(
    parent_dir: &TrustedDir,
    file_name: &str,
    display_path: &Path,
    initial_content: String,
    explicit_tool_root: Option<&Path>,
    budget: &mut InvocationBudget,
) -> Result<MountedTsContext, AuthoringFailure> {
    let profile = Profile::load().map_err(|err| AuthoringFailure::Io(err.to_string()))?;
    let mut vfs = BoundedVfs::default();
    let effect_dist_prefix =
        mount_pinned_packages(&mut vfs, &profile, display_path, explicit_tool_root)?;

    let initial_rel = PathBuf::from(file_name);
    let mut visited: BTreeSet<PathBuf> = BTreeSet::new();
    visited.insert(initial_rel.clone());
    let mut queue: Vec<(PathBuf, usize, String)> = vec![(initial_rel, 0, initial_content)];
    let mut loaded: Vec<(PathBuf, String)> = Vec::new();

    while let Some((current_rel, depth, text)) = queue.pop() {
        if loaded.len() >= MAX_LOCAL_IMPORTS {
            return Err(AuthoringFailure::Io(format!(
                "{}: local import closure exceeds {MAX_LOCAL_IMPORTS} files",
                display_path.display()
            )));
        }
        if depth < MAX_LOCAL_IMPORT_DEPTH {
            for spec in extract_relative_specifiers(&text) {
                for candidate in relative_candidates(&current_rel, &spec) {
                    if visited.contains(&candidate) {
                        break;
                    }
                    if let Some(imported_text) = parent_dir
                        .probe_import_file_rel(&candidate, budget)
                        .map_err(AuthoringFailure::Io)?
                    {
                        visited.insert(candidate.clone());
                        queue.push((candidate, depth.saturating_add(1), imported_text));
                        break;
                    }
                }
            }
        }
        loaded.push((current_rel, text));
    }

    let up_levels = loaded
        .iter()
        .map(|(rel, _)| count_leading_parent_dirs(rel))
        .max()
        .unwrap_or(0);
    let mut source_paths = BTreeMap::new();
    let mut primary_vpath = String::new();
    for (idx, (rel_path, text)) in loaded.into_iter().enumerate() {
        let vpath = virtual_path_from_rel(&rel_path, up_levels)?;
        let normalized = vfs
            .add_file(&vpath, text)
            .map_err(|err| AuthoringFailure::Io(err.to_string()))?;
        if idx == 0 {
            primary_vpath.clone_from(&normalized);
        }
        let disk_display = normalize_display_join(parent_dir.display_path(), &rel_path);
        source_paths.insert(normalized, display_posix_path(&disk_display));
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
    root_dir: &TrustedDir,
    root_rel_paths: &[PathBuf],
    explicit_tool_root: Option<&Path>,
    budget: &mut InvocationBudget,
) -> Result<MountedTsContext, AuthoringFailure> {
    let profile = Profile::load().map_err(|err| AuthoringFailure::Io(err.to_string()))?;
    let mut vfs = BoundedVfs::default();
    let effect_dist_prefix = mount_pinned_packages(
        &mut vfs,
        &profile,
        root_dir.display_path(),
        explicit_tool_root,
    )?;

    let support_files = root_dir
        .walk_files(is_ts_or_json_support, budget)
        .map_err(AuthoringFailure::Io)?;
    let mut source_paths = BTreeMap::new();
    let mut rel_to_vpath = BTreeMap::new();
    for loaded in support_files {
        let vpath = virtual_path_from_rel(&loaded.rel_path, 0)?;
        let normalized = vfs
            .add_file(&vpath, loaded.content)
            .map_err(|err| AuthoringFailure::Io(err.to_string()))?;
        source_paths.insert(normalized.clone(), display_posix_path(&loaded.display_path));
        rel_to_vpath.insert(loaded.rel_path, normalized);
    }

    let mut root_vpaths = Vec::with_capacity(root_rel_paths.len());
    for root_rel in root_rel_paths {
        if let Some(vpath) = rel_to_vpath.get(root_rel) {
            root_vpaths.push(vpath.clone());
        } else {
            let text = root_dir
                .read_regular_file_rel(root_rel, budget)
                .map_err(AuthoringFailure::Io)?;
            let vpath = virtual_path_from_rel(root_rel, 0)?;
            let normalized = vfs
                .add_file(&vpath, text)
                .map_err(|err| AuthoringFailure::Io(err.to_string()))?;
            let shown = display_posix_path(&root_dir.display_path().join(root_rel));
            source_paths.insert(normalized.clone(), shown);
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

fn open_single_ts_file_pinned(
    path: &Path,
    budget: &mut InvocationBudget,
) -> Result<(TrustedDir, String, String), AuthoringFailure> {
    if classify_source_file(path) != Some(SourceLang::TypeScript) {
        return Err(AuthoringFailure::Io(format!(
            "{}: expected a TypeScript (.ts) authoring file",
            path.display()
        )));
    }
    open_regular_file_pinned(path, budget).map_err(AuthoringFailure::Io)
}

struct ArtifactSink<'a> {
    seen_ids: &'a mut BTreeSet<String>,
    out: &'a mut BoundedJsonArray,
    budget: &'a mut InvocationBudget,
}

fn append_unique_artifact(
    artifact: &Value,
    source_path: &str,
    sink: &mut ArtifactSink<'_>,
) -> Result<(), AuthoringFailure> {
    if let Some(id) = artifact.get("artifactId").and_then(Value::as_str)
        && !sink.seen_ids.insert(id.to_owned())
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
    sink.out
        .push_value("typescript compile output", artifact, sink.budget)
        .map_err(AuthoringFailure::Io)
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
    sink: &mut ArtifactSink<'_>,
) -> Result<Vec<Value>, AuthoringFailure> {
    let mut failures = Vec::new();
    match outcome {
        DefinitionsCompileOutcome::Err { diagnostics } => {
            failures.push(json!({ "file": source_path, "diagnostics": diagnostics }));
        }
        DefinitionsCompileOutcome::Ok(out) => {
            let before_count = sink.out.len();
            for artifact in out.artifacts {
                append_unique_artifact(&artifact, source_path, sink)?;
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
                                append_unique_artifact(&artifact, source_path, sink)?;
                            }
                        }
                        FlowCompileOutcome::Err { diagnostics } => {
                            failures
                                .push(json!({ "file": source_path, "diagnostics": diagnostics }));
                        }
                    }
                }
            } else if sink.out.len() == before_count && out.retained.is_empty() && cfg.single_module
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

fn run_ts_program_compilation_into(
    mounted: MountedTsContext,
    opts: &TsCompileOptions<'_>,
    single_module: bool,
    sink: &mut ArtifactSink<'_>,
) -> Result<(), AuthoringFailure> {
    let root_slices: Vec<&str> = mounted.root_vpaths.iter().map(String::as_str).collect();
    let checker = ProfileChecker::new(mounted.profile.clone());
    let mut program = AuthoringProgram::new(mounted.vfs, &root_slices, mounted.profile)
        .map_err(|err| AuthoringFailure::Io(err.to_string()))?;
    if let Some(prefix) = mounted.effect_dist_prefix {
        program = program.with_effect_dist_prefix(prefix);
    }

    let mut scope = BTreeMap::new();
    scope.insert("environmentId".to_owned(), opts.env.to_owned());
    scope.insert("orgId".to_owned(), opts.org.to_owned());

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
            forbid_unsafe_zen: Some(opts.forbid_unsafe_zen),
            tool_root: "/".to_owned(),
            limits: CompilerLimits::default(),
        }
    })
    .map_err(|err| AuthoringFailure::Io(err.to_string()))?;

    let cfg = FlowCollectConfig {
        checker: &checker,
        program: &program,
        scope: &scope,
        forbid_unsafe_zen: opts.forbid_unsafe_zen,
        single_module,
    };
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
            sink,
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
    Ok(())
}

/// Compiles `TypeScript` targets and streams each emitted artifact directly into `out` under `budget`.
pub(crate) fn compile_ts_targets_into(
    target: TsCompileTarget<'_>,
    opts: &TsCompileOptions<'_>,
    seen_ids: &mut BTreeSet<String>,
    out: &mut BoundedJsonArray,
    budget: &mut InvocationBudget,
) -> Result<(), AuthoringFailure> {
    let single_module = match &target {
        TsCompileTarget::SingleFile { .. } => true,
        TsCompileTarget::Directory { root_rel_paths, .. } => root_rel_paths.len() == 1,
    };
    let mounted = match target {
        TsCompileTarget::SingleFile {
            parent_dir,
            file_name,
            display_path,
            content,
        } => mount_single_file_closure(
            parent_dir,
            file_name,
            display_path,
            content,
            opts.tool_root,
            budget,
        )?,
        TsCompileTarget::Directory {
            root_dir,
            root_rel_paths,
        } => mount_ts_files_and_support(root_dir, &root_rel_paths, opts.tool_root, budget)?,
    };
    let mut sink = ArtifactSink {
        seen_ids,
        out,
        budget,
    };
    run_ts_program_compilation_into(mounted, opts, single_module, &mut sink)
}

/// Compiles one `TypeScript` target into a bounded JSON artifact array.
pub(crate) fn compile_ts_targets(
    target: TsCompileTarget<'_>,
    opts: &TsCompileOptions<'_>,
    budget: &mut InvocationBudget,
) -> Result<String, AuthoringFailure> {
    let mut seen_ids = BTreeSet::new();
    let mut out = BoundedJsonArray::new();
    compile_ts_targets_into(target, opts, &mut seen_ids, &mut out, budget)?;
    out.finish("typescript compile output", budget)
        .map_err(AuthoringFailure::Io)
}

/// Runs `ProfileChecker` on a single `TypeScript` file and returns `(reports_json, all_clean)`.
pub(crate) fn run_profile(
    path: &Path,
    tool_root: Option<&Path>,
) -> Result<(String, bool), AuthoringFailure> {
    let mut budget = InvocationBudget::new();
    let (parent_dir, file_name, content) = open_single_ts_file_pinned(path, &mut budget)?;
    let mounted = mount_single_file_closure(
        &parent_dir,
        &file_name,
        path,
        content,
        tool_root,
        &mut budget,
    )?;
    let root_slices: Vec<&str> = mounted.root_vpaths.iter().map(String::as_str).collect();
    let checker = ProfileChecker::new(mounted.profile);
    let reports = checker
        .check_vfs(mounted.vfs, &root_slices, mounted.effect_dist_prefix)
        .map_err(|err| AuthoringFailure::Io(err.to_string()))?;
    let clean = reports.iter().all(|report| report.diagnostics.is_empty());
    let mut buf = Vec::new();
    {
        let mut writer = BoundedJsonWriter::new("profile output", &mut buf, &mut budget);
        serde_json::to_writer(&mut writer, &reports)
            .map_err(|err| AuthoringFailure::Io(err.to_string()))?;
    }
    let rendered = String::from_utf8(buf)
        .map_err(|err| AuthoringFailure::Io(format!("profile output: invalid UTF-8: {err}")))?;
    Ok((rendered, clean))
}

/// Compiles standalone `Norn.question` declarations from a `TypeScript` file.
pub(crate) fn run_question(
    path: &Path,
    out: Option<&Path>,
    tool_root: Option<&Path>,
) -> Result<String, AuthoringFailure> {
    let mut budget = InvocationBudget::new();
    let (parent_dir, file_name, content) = open_single_ts_file_pinned(path, &mut budget)?;
    let mounted = mount_single_file_closure(
        &parent_dir,
        &file_name,
        path,
        content,
        tool_root,
        &mut budget,
    )?;
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
            let mut array = BoundedJsonArray::new();
            for question in &questions {
                array
                    .push_value("question output", question, &mut budget)
                    .map_err(AuthoringFailure::Io)?;
            }
            let rendered = array
                .finish("question output", &mut budget)
                .map_err(AuthoringFailure::Io)?;
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
    let mut budget = InvocationBudget::new();
    let (parent_dir, file_name, content) = open_single_ts_file_pinned(path, &mut budget)?;
    let mounted = mount_single_file_closure(
        &parent_dir,
        &file_name,
        path,
        content,
        tool_root,
        &mut budget,
    )?;
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
            let mut array = BoundedJsonArray::new();
            for dashboard in &dashboards {
                array
                    .push_value("dashboard output", dashboard, &mut budget)
                    .map_err(AuthoringFailure::Io)?;
            }
            let rendered = array
                .finish("dashboard output", &mut budget)
                .map_err(AuthoringFailure::Io)?;
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
