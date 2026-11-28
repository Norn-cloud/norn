//! Structured `--pack-json` compilation output for native `norn compile`.
//!
//! Emits the full pack compilation result (`artifacts`, `sources`, `sourceMap`,
//! `warnings`, `examples`, `retained`, `flows`, `flowSourceMap`, and per-module
//! outcomes) from a single in-process `norn-authoring` [`AuthoringProgram`].

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use norn_authoring::{
    AuthoringProgram, CompiledExample, CompilerLimits, DefinitionSource, DefinitionsCompileOptions,
    DefinitionsCompileOutcome, EnvelopeOptions, FLOW_SOURCE_MAP_DRAFT_FORMAT, FlowCompileOptions,
    FlowCompileOutcome, FlowSourceMapFlow, ProfileChecker, ProfileDiagnostic,
    RetainedDefinitionDoc, compile_definitions_program, compile_flow_program,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::atomic_io::write_file_atomic;
use super::fs_io::{
    InvocationBudget, LoadedFile, SourceLang, classify_source_file, display_posix_path,
    is_declaration_file, is_json_file, read_opened_file_budgeted,
};
use super::trusted_dir::{
    PinnedSourceTarget, TrustedDir, open_regular_file_pinned, open_source_target_pinned,
    read_files_in_pinned_subdirs,
};
use super::ts::{
    MountedTsContext, TsClosureInputs, is_ts_support_file, mount_single_file_closure,
    mount_ts_closure,
};
use super::{AuthoringFailure, CommandOutput, CompileArgs};

const MAX_MODULES_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModuleSpec {
    path: PathBuf,
    #[serde(default)]
    scope: Option<BTreeMap<String, String>>,
    #[serde(default)]
    source_path: Option<String>,
    #[serde(default)]
    registry_package_path: Option<String>,
}

struct ModuleEnvelope {
    file: String,
    scope: BTreeMap<String, String>,
    source_path: String,
    registry_package_path: Option<String>,
}

struct PackCollector {
    artifacts: Vec<Value>,
    sources: BTreeMap<String, String>,
    source_map: Vec<DefinitionSource>,
    warnings: Vec<Value>,
    examples: Vec<CompiledExample>,
    retained: Vec<RetainedDefinitionDoc>,
    retained_from: BTreeMap<String, String>,
    flows: Vec<Value>,
    flow_sm_flows: Vec<FlowSourceMapFlow>,
    failures: Vec<Value>,
    modules: Vec<Value>,
}

impl PackCollector {
    const fn new() -> Self {
        Self {
            artifacts: Vec::new(),
            sources: BTreeMap::new(),
            source_map: Vec::new(),
            warnings: Vec::new(),
            examples: Vec::new(),
            retained: Vec::new(),
            retained_from: BTreeMap::new(),
            flows: Vec::new(),
            flow_sm_flows: Vec::new(),
            failures: Vec::new(),
            modules: Vec::new(),
        }
    }

    fn push_artifact(&mut self, artifact: Value, file: &str) {
        if let Some(id) = artifact.get("artifactId").and_then(Value::as_str)
            && let Some(prev) = self.sources.get(id)
        {
            self.failures.push(synthetic_failure(
                file,
                "NORN-FLOW-LITERAL",
                id,
                &format!("artifact {id} is also declared by {prev}"),
            ));
            return;
        }
        if let Some(id) = artifact.get("artifactId").and_then(Value::as_str) {
            self.sources.insert(id.to_owned(), file.to_owned());
        }
        self.artifacts.push(artifact);
    }

    fn push_retained(&mut self, doc: RetainedDefinitionDoc, file: &str) {
        if let Some(prev) = self.retained_from.get(&doc.id) {
            self.failures.push(synthetic_failure(
                file,
                "NORN-FLOW-LITERAL",
                &doc.id,
                &format!("definition {} is also declared by {prev}", doc.id),
            ));
            return;
        }
        self.retained_from.insert(doc.id.clone(), file.to_owned());
        self.retained.push(doc);
    }
}

fn synthetic_failure(file: &str, code: &str, construct: &str, message: &str) -> Value {
    json!({
        "file": file,
        "diagnostics": [{
            "code": code,
            "start": { "line": 1, "column": 1 },
            "end": { "line": 1, "column": 1 },
            "construct": construct,
            "message": message,
        }],
    })
}

fn normalize_abs(path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else if let Ok(cwd) = std::env::current_dir() {
        cwd.join(path)
    } else {
        path.to_path_buf()
    };
    let mut out = PathBuf::new();
    for comp in joined.components() {
        match comp {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = out.pop();
            }
            Component::Normal(part) => out.push(part),
        }
    }
    out
}

fn relative_pathbuf(base: &Path, target: &Path) -> PathBuf {
    let abs_base = normalize_abs(base);
    let abs_target = normalize_abs(target);
    let base_parts: Vec<Component<'_>> = abs_base.components().collect();
    let target_parts: Vec<Component<'_>> = abs_target.components().collect();
    let common = base_parts
        .iter()
        .zip(target_parts.iter())
        .take_while(|(left, right)| left == right)
        .count();
    let mut out = PathBuf::new();
    for _step in common..base_parts.len() {
        out.push("..");
    }
    if let Some(rest) = target_parts.get(common..) {
        for comp in rest {
            out.push(comp.as_os_str());
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

fn display_relative(root: Option<&Path>, target: &Path) -> String {
    root.map_or_else(
        || display_posix_path(target),
        |base| display_posix_path(&relative_pathbuf(base, target)),
    )
}

fn default_scope(args: &CompileArgs) -> Result<BTreeMap<String, String>, AuthoringFailure> {
    if args.registry.is_some() {
        return Ok(BTreeMap::new());
    }
    match (&args.org, &args.env) {
        (Some(org), Some(env)) => Ok(BTreeMap::from([
            ("environmentId".to_owned(), env.clone()),
            ("orgId".to_owned(), org.clone()),
        ])),
        (None, None) => Ok(BTreeMap::new()),
        (Some(_), None) | (None, Some(_)) => Err(AuthoringFailure::Io(
            "--org and --env must be supplied together".to_owned(),
        )),
    }
}

fn build_limits(args: &CompileArgs) -> CompilerLimits {
    let mut limits = CompilerLimits::default();
    if let Some(work) = args.max_expansion_work {
        limits.max_expansion_work = work;
        limits = limits.clamped();
    }
    limits
}

fn mount_explicit_modules(
    args: &CompileArgs,
    modules_path: &Path,
    base_scope: &BTreeMap<String, String>,
    budget: &mut InvocationBudget,
) -> Result<(MountedTsContext, Vec<ModuleEnvelope>), AuthoringFailure> {
    let raw = crate::cell::read_regular_file_bounded(
        modules_path,
        MAX_MODULES_MANIFEST_BYTES,
        "4 MiB modules manifest limit",
    )
    .map_err(AuthoringFailure::Io)?;
    let specs: Vec<ModuleSpec> = serde_json::from_str(&raw)
        .map_err(|err| AuthoringFailure::Io(format!("invalid --modules manifest: {err}")))?;
    let base_dir = TrustedDir::open(&args.path).map_err(AuthoringFailure::Io)?;
    let mut root_files = Vec::with_capacity(specs.len());
    let mut envelopes = Vec::with_capacity(specs.len());
    for spec in specs {
        let (_, _, content) =
            open_regular_file_pinned(&spec.path, budget).map_err(AuthoringFailure::Io)?;
        let rel_path = relative_pathbuf(base_dir.display_path(), &spec.path);
        let file = display_relative(args.root.as_deref(), &spec.path);
        let registry_package_path = spec.registry_package_path.or_else(|| args.registry.clone());
        let scope = if registry_package_path.is_some() {
            BTreeMap::new()
        } else {
            spec.scope.unwrap_or_else(|| base_scope.clone())
        };
        let source_path = spec.source_path.unwrap_or_else(|| file.clone());
        root_files.push(LoadedFile {
            display_path: spec.path,
            rel_path,
            content,
        });
        envelopes.push(ModuleEnvelope {
            file,
            scope,
            source_path,
            registry_package_path,
        });
    }
    let inputs = TsClosureInputs {
        root_files,
        support_files: &[],
        subdir_files: Vec::new(),
        package_hint_path: base_dir.display_path(),
    };
    let mounted = mount_ts_closure(&base_dir, inputs, args.tool_root.as_deref(), budget)?;
    Ok((mounted, envelopes))
}

fn mount_directory_target(
    root_dir: &TrustedDir,
    args: &CompileArgs,
    base_scope: &BTreeMap<String, String>,
    budget: &mut InvocationBudget,
) -> Result<(MountedTsContext, Vec<ModuleEnvelope>), AuthoringFailure> {
    let top_scan = root_dir
        .scan_top_level(budget)
        .map_err(AuthoringFailure::Io)?;
    let mut ts_files = Vec::new();
    let mut json_files = Vec::new();
    for loaded in top_scan.files {
        let name = loaded
            .rel_path
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("");
        if is_declaration_file(name) {
            continue;
        }
        if is_json_file(&loaded.display_path) {
            json_files.push(loaded);
        } else if classify_source_file(&loaded.display_path) == Some(SourceLang::TypeScript) {
            ts_files.push(loaded);
        }
    }
    let envelopes: Vec<ModuleEnvelope> = ts_files
        .iter()
        .map(|loaded| {
            let file = display_relative(args.root.as_deref(), &loaded.display_path);
            ModuleEnvelope {
                file: file.clone(),
                scope: base_scope.clone(),
                source_path: file,
                registry_package_path: args.registry.clone(),
            }
        })
        .collect();
    let subdir_files = read_files_in_pinned_subdirs(&top_scan.subdirs, is_ts_support_file, budget)
        .map_err(AuthoringFailure::Io)?;
    let inputs = TsClosureInputs {
        root_files: ts_files,
        support_files: &json_files,
        subdir_files,
        package_hint_path: root_dir.display_path(),
    };
    let mounted = mount_ts_closure(root_dir, inputs, args.tool_root.as_deref(), budget)?;
    Ok((mounted, envelopes))
}

fn mount_target_context(
    args: &CompileArgs,
    base_scope: &BTreeMap<String, String>,
    budget: &mut InvocationBudget,
) -> Result<(MountedTsContext, Vec<ModuleEnvelope>), AuthoringFailure> {
    if let Some(modules_path) = &args.modules {
        return mount_explicit_modules(args, modules_path, base_scope, budget);
    }
    match open_source_target_pinned(&args.path).map_err(AuthoringFailure::Io)? {
        PinnedSourceTarget::File {
            parent_dir,
            file_name,
            display_path,
            opened,
        } => {
            if classify_source_file(&display_path) != Some(SourceLang::TypeScript) {
                return Err(AuthoringFailure::Io(format!(
                    "{}: --pack-json expects TypeScript (.ts) sources",
                    display_path.display()
                )));
            }
            budget
                .charge_accepted_file(&display_path)
                .map_err(AuthoringFailure::Io)?;
            let content = read_opened_file_budgeted(opened, &display_path, budget)
                .map_err(AuthoringFailure::Io)?;
            let file = display_relative(args.root.as_deref(), &display_path);
            let source_path = args.source_path.clone().unwrap_or_else(|| file.clone());
            let mounted = mount_single_file_closure(
                &parent_dir,
                &file_name,
                &display_path,
                content,
                args.tool_root.as_deref(),
                budget,
            )?;
            let envelope = ModuleEnvelope {
                file,
                scope: base_scope.clone(),
                source_path,
                registry_package_path: args.registry.clone(),
            };
            Ok((mounted, vec![envelope]))
        }
        PinnedSourceTarget::Directory(root_dir) => {
            mount_directory_target(&root_dir, args, base_scope, budget)
        }
    }
}

fn rewrite_location_file(file: &str, vpath_to_rel: &BTreeMap<String, String>) -> String {
    let trimmed = file.trim_start_matches('/');
    vpath_to_rel
        .get(trimmed)
        .cloned()
        .unwrap_or_else(|| file.to_owned())
}

fn rewrite_builder_source_map(mut sm: Value, vpath_to_rel: &BTreeMap<String, String>) -> Value {
    if let Some(steps) = sm.get_mut("steps").and_then(Value::as_object_mut) {
        for loc in steps.values_mut() {
            if let Some(file_val) = loc.get_mut("file")
                && let Some(raw) = file_val.as_str()
            {
                *file_val = Value::String(rewrite_location_file(raw, vpath_to_rel));
            }
        }
    }
    sm
}

fn warning_with_file(warning: &ProfileDiagnostic, file: &str) -> Result<Value, AuthoringFailure> {
    let mut val =
        serde_json::to_value(warning).map_err(|err| AuthoringFailure::Io(err.to_string()))?;
    if let Some(obj) = val.as_object_mut() {
        obj.insert("file".to_owned(), Value::String(file.to_owned()));
    }
    Ok(val)
}

struct FlowPassContext<'a> {
    checker: &'a ProfileChecker,
    program: &'a AuthoringProgram,
    vpath_to_rel: &'a BTreeMap<String, String>,
    forbid_unsafe_zen: bool,
    limits: CompilerLimits,
}

fn compile_module_flow(
    ctx: &FlowPassContext<'_>,
    vpath: &str,
    env: &ModuleEnvelope,
    collector: &mut PackCollector,
) -> Result<(Vec<Value>, Option<FlowSourceMapFlow>, Option<Value>, bool), AuthoringFailure> {
    let flow_opts = FlowCompileOptions {
        envelope: EnvelopeOptions {
            scope: env.scope.clone(),
            source_path: env.source_path.clone(),
            registry_package_path: env.registry_package_path.clone(),
        },
        include_flow_source_map: true,
        source_root: "/".to_owned(),
        tool_root: "/".to_owned(),
        forbid_unsafe_zen: ctx.forbid_unsafe_zen || env.registry_package_path.is_some(),
        limits: ctx.limits,
    };
    let outcomes = compile_flow_program(ctx.checker, ctx.program, &[vpath], &flow_opts)
        .map_err(|err| AuthoringFailure::Io(err.to_string()))?;
    let Some(outcome) = outcomes.into_iter().next() else {
        return Ok((Vec::new(), None, None, true));
    };
    match outcome {
        FlowCompileOutcome::Err { diagnostics } => {
            collector.failures.push(json!({
                "file": env.file,
                "diagnostics": diagnostics,
            }));
            collector.modules.push(json!({
                "file": env.file,
                "ok": false,
                "diagnostics": diagnostics,
            }));
            Ok((Vec::new(), None, None, false))
        }
        FlowCompileOutcome::Ok(flow_out) => {
            for warning in &flow_out.warnings {
                collector
                    .warnings
                    .push(warning_with_file(warning, &env.file)?);
            }
            let flow_artifacts = flow_out.artifacts;
            for artifact in &flow_artifacts {
                collector.push_artifact(artifact.clone(), &env.file);
            }
            let builder_sm = rewrite_builder_source_map(flow_out.source_map, ctx.vpath_to_rel);
            let mut flow_sm = flow_out
                .flow_source_map
                .and_then(|draft| draft.flows.into_iter().next());
            if let Some(entry) = &mut flow_sm {
                entry.source_path.clone_from(&env.source_path);
                for step in &mut entry.steps {
                    if let Some(at) = &mut step.at {
                        at.file = rewrite_location_file(&at.file, ctx.vpath_to_rel);
                    }
                }
                collector.flows.push(json!({
                    "file": env.file,
                    "artifacts": flow_artifacts,
                    "flowSourceMap": entry,
                    "builderSourceMap": builder_sm,
                }));
                collector.flow_sm_flows.push(entry.clone());
            }
            Ok((flow_artifacts, flow_sm, Some(builder_sm), true))
        }
    }
}

fn process_module_outcome(
    ctx: &FlowPassContext<'_>,
    vpath: &str,
    env: &ModuleEnvelope,
    outcome: DefinitionsCompileOutcome,
    allow_empty: bool,
    collector: &mut PackCollector,
) -> Result<(), AuthoringFailure> {
    match outcome {
        DefinitionsCompileOutcome::Err { diagnostics } => {
            collector.failures.push(json!({
                "file": env.file,
                "diagnostics": diagnostics,
            }));
            collector.modules.push(json!({
                "file": env.file,
                "ok": false,
                "diagnostics": diagnostics,
            }));
        }
        DefinitionsCompileOutcome::Ok(mut out) => {
            for entry in &mut out.source_map {
                entry.at.file = rewrite_location_file(&entry.at.file, ctx.vpath_to_rel);
            }
            for example in &mut out.examples {
                example.at.file = rewrite_location_file(&example.at.file, ctx.vpath_to_rel);
            }
            for warning in &out.warnings {
                collector
                    .warnings
                    .push(warning_with_file(warning, &env.file)?);
            }
            let mut mod_artifacts = out.artifacts.clone();
            for artifact in out.artifacts {
                collector.push_artifact(artifact, &env.file);
            }
            let mod_retained = out.retained.clone();
            for doc in out.retained {
                collector.push_retained(doc, &env.file);
            }
            collector.source_map.extend(out.source_map.clone());
            collector.examples.extend(out.examples.clone());

            let mut mod_flow_sm: Option<FlowSourceMapFlow> = None;
            let mut mod_builder_sm: Option<Value> = None;
            if out.flows > 0 {
                let (flow_arts, flow_sm, builder_sm, ok) =
                    compile_module_flow(ctx, vpath, env, collector)?;
                if !ok {
                    return Ok(());
                }
                mod_artifacts.extend(flow_arts);
                mod_flow_sm = flow_sm;
                mod_builder_sm = builder_sm;
            } else if mod_artifacts.is_empty()
                && mod_retained.is_empty()
                && out.warnings.is_empty()
                && !allow_empty
            {
                let failure = synthetic_failure(
                    &env.file,
                    "NORN-FLOW-RESULT",
                    &env.file,
                    "a pack module declares no definition and no Norn.flow; move shared code under a subdirectory",
                );
                collector.failures.push(failure);
                collector.modules.push(json!({
                    "file": env.file,
                    "ok": false,
                    "diagnostics": [{
                        "code": "NORN-FLOW-RESULT",
                        "start": { "line": 1, "column": 1 },
                        "end": { "line": 1, "column": 1 },
                        "construct": env.file,
                        "message": "a pack module declares no definition and no Norn.flow; move shared code under a subdirectory",
                    }],
                }));
                return Ok(());
            }

            collector.modules.push(json!({
                "file": env.file,
                "ok": true,
                "artifacts": mod_artifacts,
                "flows": out.flows,
                "sourceMap": out.source_map,
                "warnings": out.warnings,
                "examples": out.examples,
                "retained": mod_retained,
                "workUsed": out.work_used,
                "flowSourceMap": mod_flow_sm,
                "builderSourceMap": mod_builder_sm,
            }));
        }
    }
    Ok(())
}

fn render_pack_collector(profile_name: &str, collector: PackCollector) -> (String, bool) {
    if !collector.failures.is_empty() {
        let doc = json!({
            "ok": false,
            "profile": profile_name,
            "failures": collector.failures,
            "modules": collector.modules,
        })
        .to_string();
        return (format!("{doc}\n"), false);
    }

    let mut payload = json!({
        "ok": true,
        "profile": profile_name,
        "artifacts": collector.artifacts,
        "sources": collector.sources,
        "sourceMap": collector.source_map,
        "warnings": collector.warnings,
        "examples": collector.examples,
        "retained": collector.retained,
        "flows": collector.flows,
        "modules": collector.modules,
    });
    if !collector.flow_sm_flows.is_empty()
        && let Some(obj) = payload.as_object_mut()
    {
        obj.insert(
            "flowSourceMap".to_owned(),
            json!({
                "format": FLOW_SOURCE_MAP_DRAFT_FORMAT,
                "flows": collector.flow_sm_flows,
            }),
        );
    }
    (format!("{payload}\n"), true)
}

fn compile_pack_json_inner(args: &CompileArgs) -> Result<(String, bool), AuthoringFailure> {
    let mut budget = InvocationBudget::new();
    let base_scope = default_scope(args)?;
    let limits = build_limits(args);
    let (mounted, envelopes) = mount_target_context(args, &base_scope, &mut budget)?;
    let profile_name = mounted.profile.profile.clone();

    if mounted.root_vpaths.is_empty() {
        return Ok(render_pack_collector(&profile_name, PackCollector::new()));
    }

    let vpath_to_rel: BTreeMap<String, String> = mounted
        .source_paths
        .iter()
        .map(|(vpath, display)| {
            let rel = display_relative(args.root.as_deref(), Path::new(display));
            (vpath.trim_start_matches('/').to_owned(), rel)
        })
        .collect();
    let root_slices: Vec<&str> = mounted.root_vpaths.iter().map(String::as_str).collect();
    let checker = ProfileChecker::new(mounted.profile.clone());
    let mut program = AuthoringProgram::new(mounted.vfs, &root_slices, mounted.profile)
        .map_err(|err| AuthoringFailure::Io(err.to_string()))?;
    if let Some(prefix) = mounted.effect_dist_prefix {
        program = program.with_effect_dist_prefix(prefix);
    }

    let env_by_vpath: BTreeMap<&str, &ModuleEnvelope> = mounted
        .root_vpaths
        .iter()
        .map(String::as_str)
        .zip(envelopes.iter())
        .collect();
    let outcomes = compile_definitions_program(&checker, &program, &root_slices, |vpath| {
        let env_opt = env_by_vpath.get(vpath).copied();
        let (scope, source_path, registry_package_path) = env_opt.map_or_else(
            || {
                (
                    base_scope.clone(),
                    vpath.trim_start_matches('/').to_owned(),
                    args.registry.clone(),
                )
            },
            |env| {
                (
                    env.scope.clone(),
                    env.source_path.clone(),
                    env.registry_package_path.clone(),
                )
            },
        );
        let forbid = args.forbid_unsafe_zen || registry_package_path.is_some();
        DefinitionsCompileOptions {
            envelope: EnvelopeOptions {
                scope,
                source_path,
                registry_package_path,
            },
            allow_empty: true,
            forbid_unsafe_zen: Some(forbid),
            tool_root: "/".to_owned(),
            limits,
        }
    })
    .map_err(|err| AuthoringFailure::Io(err.to_string()))?;

    let ctx = FlowPassContext {
        checker: &checker,
        program: &program,
        vpath_to_rel: &vpath_to_rel,
        forbid_unsafe_zen: args.forbid_unsafe_zen,
        limits,
    };
    let mut collector = PackCollector::new();
    for ((vpath, env), outcome) in mounted
        .root_vpaths
        .iter()
        .zip(envelopes.iter())
        .zip(outcomes)
    {
        process_module_outcome(&ctx, vpath, env, outcome, args.allow_empty, &mut collector)?;
    }

    Ok(render_pack_collector(&profile_name, collector))
}

/// Executes `norn compile --pack-json`.
pub(crate) fn execute_compile_pack_json(args: &CompileArgs) -> CommandOutput {
    match compile_pack_json_inner(args) {
        Ok((rendered, true)) => {
            if let Some(out_path) = &args.out {
                match write_file_atomic(out_path, &rendered) {
                    Ok(()) => CommandOutput::ok(String::new()),
                    Err(err) => CommandOutput::usage_or_io_error(&err),
                }
            } else {
                CommandOutput::ok(rendered)
            }
        }
        Ok((rendered, false)) => CommandOutput::refused(rendered),
        Err(failure) => failure.into_output(),
    }
}
