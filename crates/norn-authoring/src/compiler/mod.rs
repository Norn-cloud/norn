//! Native Rust TypeScript → Canonical IR lowering compiler (`Slice 3`).
//!
//! Runs [`ProfileChecker`] first on every authoring program and lowers
//! validated `Norn.flow`, `Norn.type`, `Norn.interfaceType`, `Norn.action`,
//! `Norn.access`, `Norn.decision`, `Norn.law`, `Norn.obligation`/`prohibition`/
//! `permission`, `Norn.connector`, `Norn.mapping`, `Norn.setting`,
//! `Norn.transform`, `Norn.sourceSchema`, `Norn.ontologyPack`, `Norn.packLink`,
//! `Norn.markings`, `Norn.labels`, `Norn.model`, `Norn.approval`,
//! `Norn.question`, `Norn.dashboard`, and exported Effect Schema value schemas
//! directly from `tsrs` AST and type information into canonical JSON IR.

pub(crate) mod canonical;
mod def_access;
mod def_action_effects;
mod def_actions;
mod def_catalog;
mod def_decision_cb;
mod def_decisions;
mod def_entities;
mod def_expr_helpers;
mod def_helpers;
mod def_laws;
mod def_lifecycles;
mod def_literals;
mod def_mapping;
mod def_models;
mod def_module;
mod def_norms;
mod def_queries;
mod def_settings;
mod def_slices;
pub(crate) mod expr;
mod expr_render;
mod expr_type;
pub(crate) mod flow_limits;
pub(crate) mod flow_source_map;
mod flow_syntax;
mod flow_syntax_calls;
mod flow_syntax_effects;
mod flow_syntax_expr;
mod flow_syntax_meta;
mod flow_syntax_steps;
pub(crate) mod law_statement;
mod lower_flow;
mod lower_flow_artifacts;
mod lower_flow_collect;
mod lower_flow_plan;
mod lower_flow_resume;
mod schema;
mod schema_project;
pub(crate) mod types;
pub(crate) mod value_schema;
mod value_schema_ops;
pub(crate) mod value_types;
mod zen_parser;
pub(crate) mod zen_reads;

use std::collections::BTreeMap;
use std::time::Instant;

use serde_json::Value;
use tsrs_ast::{Node, SourceFile};
use tsrs_core::P;
use tsrs_core::context::Context;

use crate::{
    AuthoringError, AuthoringProgram, BoundedVfs, Position, ProfileChecker, ProfileDiagnostic,
};

use self::def_helpers::{DefCompilerConfig, DefinitionCompiler};
use self::def_literals::bind_compiled_source_schemas;
use self::flow_syntax::{SyntaxCompiler, SyntaxConfig};
use self::lower_flow::{BuildDiagnostic, LowerFlowOptions, LowerFlowOutcome, lower_flow};
use self::types::{check_closure, diagnostic_for_node, relative_virtual_path};
use self::value_schema::{BudgetExceeded, ExpansionBudget, ExpansionLimits};

pub use self::canonical::{
    CanonicalError, canonical_json, compact_canonical_json, norn_canonical_hash, norn1_sha256,
    to_canonical_json,
};
pub use self::def_helpers::{
    CompiledExample, DefinitionSource, ExampleExpectation, PENDING_SOURCE_SHA256,
    RetainedDefinitionDoc,
};
pub use self::flow_limits::{
    FlowLimits, LimitCheck, LimitsEnvelope, LimitsProfile, StepLimitKind, StepLimits, flow_limits,
    render_flow_limits,
};
pub use self::flow_source_map::{
    FLOW_MAX_RECURS, FLOW_MAX_ROWS, FLOW_MAX_STEPS, FLOW_MAX_TIMER_MILLIS,
    FLOW_SOURCE_MAP_DRAFT_FORMAT, FLOW_SOURCE_MAP_FORMAT, FlowDef, FlowMeta,
    FlowSourceMapAnnotation, FlowSourceMapArtifactRef, FlowSourceMapDraft, FlowSourceMapEdge,
    FlowSourceMapEdgeKind, FlowSourceMapFlow, FlowSourceMapGraph, FlowSourceMapStep,
    FlowSourceMapStepKind, FnRef, PerformStep, RetryDelay, RetryPolicy, RouteBranch,
    SourceLocation, Step, TaggedErrorDecl,
};
pub use self::law_statement::{
    LawMetadata, LawStatement, StatementError, from_statement, parse_sentence, parse_statement,
    render_sentence, serialize_statement, to_statement,
};
pub use self::lower_flow::{FlowDisplay, FlowStepLabel};
pub use self::types::{CompilerLimits, DependencyPin, EnvelopeOptions};
pub use self::value_schema::{InputGroup, ObjectSchema, SchemaLabels, ValueSchema};
pub use self::value_types::{
    CurrencySource, DecimalType, EnumValue, PropertyAdditivity, PropertyType, Rounding, TextFormat,
    TimezonePolicy, UnitRef,
};
pub use self::zen_reads::{lexical_bounds, may_read_actor};

/// Options controlling `Norn.flow` compilation.
#[derive(Clone, Debug)]
pub struct FlowCompileOptions {
    /// Envelope provenance and scope options.
    pub envelope: EnvelopeOptions,
    /// Whether to emit the draft flow source map (`norn-flow-source-map-draft/1`).
    pub include_flow_source_map: bool,
    /// Virtual root directory for draft flow source-map paths (for example `"/"`).
    pub source_root: String,
    /// Virtual tool root directory for builder `SourceLocation` paths (for example `"/"`).
    pub tool_root: String,
    /// Promote `NORN-AUTHORING-UNSAFE-ZEN` warnings to hard errors.
    pub forbid_unsafe_zen: bool,
    /// Compiler resource ceilings.
    pub limits: CompilerLimits,
}

impl Default for FlowCompileOptions {
    fn default() -> Self {
        Self {
            envelope: EnvelopeOptions::default(),
            include_flow_source_map: false,
            source_root: "/".to_owned(),
            tool_root: "/".to_owned(),
            forbid_unsafe_zen: false,
            limits: CompilerLimits::default(),
        }
    }
}

/// Successful compilation payload for one `Norn.flow` module.
#[derive(Clone, Debug)]
pub struct FlowCompileOutput {
    /// Closure-free flow definition AST.
    pub definition: FlowDef,
    /// Lowered canonical JSON IR artifacts (Run entity + decide rule).
    pub artifacts: Vec<Value>,
    /// Builder source map (`{ artifactId, version, steps, rows }`).
    pub source_map: Value,
    /// Non-fatal compiler warnings (such as `NORN-AUTHORING-UNSAFE-ZEN`).
    pub warnings: Vec<ProfileDiagnostic>,
    /// Optional draft flow source map (`norn-flow-source-map-draft/1`).
    pub flow_source_map: Option<FlowSourceMapDraft>,
    /// Flow display metadata.
    pub flow: FlowDisplay,
}

/// Outcome of compiling one `Norn.flow` module.
#[derive(Clone, Debug)]
pub enum FlowCompileOutcome {
    /// Lowering succeeded and produced canonical IR artifacts.
    Ok(Box<FlowCompileOutput>),
    /// Profile checking or lowering refused the module.
    Err {
        /// Ordered compiler diagnostics.
        diagnostics: Vec<ProfileDiagnostic>,
    },
}

/// Options controlling ontology/action/access/decision/law definition compilation.
#[derive(Clone, Debug)]
pub struct DefinitionsCompileOptions {
    /// Envelope provenance and scope options.
    pub envelope: EnvelopeOptions,
    /// Allow modules with no ontology definition (for example flow-only modules).
    pub allow_empty: bool,
    /// Override `NORN-AUTHORING-UNSAFE-ZEN` hard-error policy; defaults to
    /// `envelope.registry_package_path.is_some()`.
    pub forbid_unsafe_zen: Option<bool>,
    /// Virtual tool root directory for source-map paths (for example `"/"`).
    pub tool_root: String,
    /// Compiler resource ceilings.
    pub limits: CompilerLimits,
}

impl Default for DefinitionsCompileOptions {
    fn default() -> Self {
        Self {
            envelope: EnvelopeOptions::default(),
            allow_empty: false,
            forbid_unsafe_zen: None,
            tool_root: "/".to_owned(),
            limits: CompilerLimits::default(),
        }
    }
}

/// Successful compilation payload for all definitions in one authoring module.
#[derive(Clone, Debug)]
pub struct DefinitionsCompileOutput {
    /// Lowered canonical JSON IR artifacts (including any `.labels` artifacts).
    pub artifacts: Vec<Value>,
    /// Number of top-level `Norn.flow` declarations encountered in this module.
    pub flows: usize,
    /// Source-map attribution entries for lowered artifacts and expressions.
    pub source_map: Vec<DefinitionSource>,
    /// Non-fatal compiler warnings.
    pub warnings: Vec<ProfileDiagnostic>,
    /// Compiled `Norn.example` cases (`LAW-EXAMPLE-1`).
    pub examples: Vec<CompiledExample>,
    /// Retained cell definitions (`Norn.approval`).
    pub retained: Vec<RetainedDefinitionDoc>,
    /// Expansion budget work units charged for this module.
    pub work_used: usize,
}

/// Outcome of compiling all definitions in one authoring module.
#[derive(Clone, Debug)]
pub enum DefinitionsCompileOutcome {
    /// Lowering succeeded and produced canonical IR artifacts.
    Ok(Box<DefinitionsCompileOutput>),
    /// Profile checking or lowering refused the module.
    Err {
        /// Ordered compiler diagnostics.
        diagnostics: Vec<ProfileDiagnostic>,
    },
}

/// Outcome of compiling standalone `Norn.question` declarations.
#[derive(Clone, Debug)]
pub enum QuestionsCompileOutcome {
    /// Lowered canonical `QuestionIR` documents.
    Ok {
        /// Compiled questions in declaration order.
        questions: Vec<Value>,
    },
    /// Profile checking or lowering refused the module.
    Err {
        /// Ordered compiler diagnostics.
        diagnostics: Vec<ProfileDiagnostic>,
    },
}

/// Outcome of compiling standalone `Norn.dashboard` declarations.
#[derive(Clone, Debug)]
pub enum DashboardsCompileOutcome {
    /// Lowered canonical dashboard definitions.
    Ok {
        /// Compiled dashboards in declaration order.
        dashboards: Vec<Value>,
    },
    /// Profile checking or lowering refused the module.
    Err {
        /// Ordered compiler diagnostics.
        diagnostics: Vec<ProfileDiagnostic>,
    },
}

/// Outcome of compiling exported Effect Schema value schemas.
#[derive(Clone, Debug)]
pub enum ValueSchemasCompileOutcome {
    /// Map of exported binding name → closed [`ValueSchema`].
    Ok {
        /// Compiled closed value schemas.
        schemas: BTreeMap<String, ValueSchema>,
    },
    /// Profile checking or lowering refused the module.
    Err {
        /// Ordered compiler diagnostics.
        diagnostics: Vec<ProfileDiagnostic>,
    },
}

fn new_budget(limits: CompilerLimits, started: Instant) -> ExpansionBudget {
    ExpansionBudget::with_started(
        ExpansionLimits {
            max_work: limits.max_expansion_work,
            max_output_bytes: limits.max_output_bytes,
            max_millis: Some(limits.max_millis),
        },
        started,
    )
}

/// Compiles `files` as `Norn.flow` modules inside an existing [`AuthoringProgram`].
///
/// Runs [`ProfileChecker::check_program`] first; rejected files never reach
/// lowering.
///
/// # Errors
///
/// Returns [`AuthoringError`] if any requested file is missing or invalid in `program`.
pub fn compile_flow_program(
    profile_checker: &ProfileChecker,
    program: &AuthoringProgram,
    files: &[&str],
    options: &FlowCompileOptions,
) -> Result<Vec<FlowCompileOutcome>, AuthoringError> {
    let started = Instant::now();
    let limits = options.limits.clamped();
    let reports = profile_checker.check_program(program, files)?;
    let mut expansion = new_budget(limits, started);
    let mut outcomes = Vec::with_capacity(files.len());
    for (&file, report) in files.iter().zip(reports) {
        if !report.diagnostics.is_empty() {
            outcomes.push(FlowCompileOutcome::Err {
                diagnostics: report.diagnostics,
            });
            continue;
        }
        let sf = program.source_file(file)?;
        outcomes.push(compile_one_flow_file(
            profile_checker,
            program,
            sf,
            options,
            limits,
            started,
            &mut expansion,
        ));
    }
    Ok(outcomes)
}

fn compile_one_flow_file(
    profile_checker: &ProfileChecker,
    program: &AuthoringProgram,
    sf: P<SourceFile>,
    options: &FlowCompileOptions,
    limits: CompilerLimits,
    started: Instant,
    expansion: &mut ExpansionBudget,
) -> FlowCompileOutcome {
    let raw_program = program.raw_program();
    let ctx = Context::default();
    let mut checker = raw_program.get_type_checker_for_file_exclusive(&ctx, sf);
    let mut lowering = SyntaxCompiler::new(SyntaxConfig {
        intrinsics_module: profile_checker.profile().intrinsics_module.as_str(),
        effect_dist_prefix: program.effect_dist_prefix(),
        vfs: program.vfs(),
        program: raw_program,
        source: sf,
        started,
        limits,
        source_root: &options.source_root,
        tool_root: &options.tool_root,
        forbid_unsafe_zen: options.forbid_unsafe_zen,
    });
    let definition = match lowering.compile(&mut checker, expansion) {
        Ok(def) => def,
        Err(err) => {
            return FlowCompileOutcome::Err {
                diagnostics: vec![err.into_diagnostic()],
            };
        }
    };
    let lower_opts = LowerFlowOptions {
        envelope: options.envelope.clone(),
        include_flow_source_map: options.include_flow_source_map,
    };
    match lower_flow(&definition, &lower_opts, expansion) {
        LowerFlowOutcome::Err(diags) => FlowCompileOutcome::Err {
            diagnostics: map_build_diagnostics(diags, raw_program, sf, &options.tool_root),
        },
        LowerFlowOutcome::Ok(out) => {
            if let Err(err) = lowering.check_budget(sf.as_node()) {
                return FlowCompileOutcome::Err {
                    diagnostics: vec![err.into_diagnostic()],
                };
            }
            for art in &out.artifacts {
                if let Err(BudgetExceeded(msg)) = expansion.add_output(art) {
                    return FlowCompileOutcome::Err {
                        diagnostics: vec![diagnostic_for_node(
                            sf.as_node(),
                            "NORN-FLOW-UNBOUNDED",
                            &msg,
                        )],
                    };
                }
            }
            FlowCompileOutcome::Ok(Box::new(FlowCompileOutput {
                definition,
                artifacts: out.artifacts,
                source_map: out.source_map,
                warnings: lowering.warnings,
                flow_source_map: out.flow_source_map,
                flow: out.flow,
            }))
        }
    }
}

/// Builds a fresh [`AuthoringProgram`] over `vfs` and compiles `files` as `Norn.flow` modules.
///
/// # Errors
///
/// Returns [`AuthoringError`] if program construction fails or any root path is invalid.
pub fn compile_flow_vfs(
    profile_checker: &ProfileChecker,
    vfs: BoundedVfs,
    files: &[&str],
    effect_dist_prefix: Option<&str>,
    options: &FlowCompileOptions,
) -> Result<Vec<FlowCompileOutcome>, AuthoringError> {
    let mut program = AuthoringProgram::new(vfs, files, profile_checker.profile().clone())?;
    if let Some(prefix) = effect_dist_prefix {
        program = program.with_effect_dist_prefix(prefix);
    }
    compile_flow_program(profile_checker, &program, files, options)
}

/// Compiles ontology, action, access, decision, law, connector, mapping, setting,
/// transform, source-schema, pack, markings, labels, model, and approval definitions
/// across `files` inside an existing [`AuthoringProgram`].
///
/// Runs [`ProfileChecker::check_program`] first, lowers each accepted module, and
/// binds same-project `Norn.sourceSchema` Norn1 hashes onto pending `Norn.mapping`
/// artifacts (`MAPPING-SOURCE-REF-1`).
///
/// # Errors
///
/// Returns [`AuthoringError`] if any requested file is missing or invalid in `program`.
pub fn compile_definitions_program(
    profile_checker: &ProfileChecker,
    program: &AuthoringProgram,
    files: &[&str],
    options_for: impl Fn(&str) -> DefinitionsCompileOptions,
) -> Result<Vec<DefinitionsCompileOutcome>, AuthoringError> {
    let started = Instant::now();
    let reports = profile_checker.check_program(program, files)?;
    let first_limits = files
        .first()
        .map(|f| options_for(f).limits.clamped())
        .unwrap_or_default();
    let mut expansion = new_budget(first_limits, started);

    let mut outcomes = Vec::with_capacity(files.len());
    for (&file, report) in files.iter().zip(reports) {
        if !report.diagnostics.is_empty() {
            outcomes.push(DefinitionsCompileOutcome::Err {
                diagnostics: report.diagnostics,
            });
            continue;
        }
        let sf = program.source_file(file)?;
        let opts = options_for(file);
        outcomes.push(compile_one_definitions_file(
            profile_checker,
            program,
            sf,
            &opts,
            started,
            &mut expansion,
        ));
    }

    // MAPPING-SOURCE-REF-1: bind same-project source schema Norn1 hashes onto pending mappings.
    let mut ok_indices: Vec<usize> = Vec::new();
    let mut batches: Vec<Vec<Value>> = Vec::new();
    for (idx, outcome) in outcomes.iter_mut().enumerate() {
        if let DefinitionsCompileOutcome::Ok(out) = outcome {
            ok_indices.push(idx);
            batches.push(std::mem::take(&mut out.artifacts));
        }
    }
    bind_compiled_source_schemas(&mut batches);
    for (idx, batch) in ok_indices.into_iter().zip(batches) {
        if let Some(DefinitionsCompileOutcome::Ok(out)) = outcomes.get_mut(idx) {
            out.artifacts = batch;
        }
    }
    Ok(outcomes)
}

fn compile_one_definitions_file(
    profile_checker: &ProfileChecker,
    program: &AuthoringProgram,
    sf: P<SourceFile>,
    opts: &DefinitionsCompileOptions,
    started: Instant,
    expansion: &mut ExpansionBudget,
) -> DefinitionsCompileOutcome {
    let raw_program = program.raw_program();
    let limits = opts.limits.clamped();
    if let Err(err) = check_closure(raw_program, sf, limits) {
        return DefinitionsCompileOutcome::Err {
            diagnostics: vec![err.into_diagnostic()],
        };
    }
    expansion.begin_module();
    let forbid_unsafe = opts
        .forbid_unsafe_zen
        .unwrap_or_else(|| opts.envelope.registry_package_path.is_some());
    let ctx = Context::default();
    let mut checker = raw_program.get_type_checker_for_file_exclusive(&ctx, sf);
    let mut compiler = DefinitionCompiler::new(DefCompilerConfig {
        intrinsics_module: profile_checker.profile().intrinsics_module.as_str(),
        effect_dist_prefix: program.effect_dist_prefix(),
        vfs: program.vfs(),
        program: raw_program,
        started,
        limits,
        tool_root: &opts.tool_root,
        forbid_unsafe_zen: forbid_unsafe,
    });
    match compiler.compile_module(
        &mut checker,
        expansion,
        sf,
        &opts.envelope,
        opts.allow_empty,
    ) {
        Ok(compiled) => {
            for art in &compiled.artifacts {
                if let Err(BudgetExceeded(msg)) = expansion.add_output(art) {
                    return DefinitionsCompileOutcome::Err {
                        diagnostics: vec![diagnostic_for_node(
                            sf.as_node(),
                            "NORN-FLOW-UNBOUNDED",
                            &msg,
                        )],
                    };
                }
            }
            DefinitionsCompileOutcome::Ok(Box::new(DefinitionsCompileOutput {
                artifacts: compiled.artifacts,
                flows: compiled.flows,
                source_map: compiler.source_map,
                warnings: compiler.warnings,
                examples: compiled.examples,
                retained: compiled.retained,
                work_used: expansion.work_used(),
            }))
        }
        Err(err) => DefinitionsCompileOutcome::Err {
            diagnostics: vec![err.into_diagnostic()],
        },
    }
}

/// Builds a fresh [`AuthoringProgram`] over `vfs` and compiles definitions in `files`.
///
/// # Errors
///
/// Returns [`AuthoringError`] if program construction fails or any root path is invalid.
pub fn compile_definitions_vfs(
    profile_checker: &ProfileChecker,
    vfs: BoundedVfs,
    files: &[&str],
    effect_dist_prefix: Option<&str>,
    options_for: impl Fn(&str) -> DefinitionsCompileOptions,
) -> Result<Vec<DefinitionsCompileOutcome>, AuthoringError> {
    let mut program = AuthoringProgram::new(vfs, files, profile_checker.profile().clone())?;
    if let Some(prefix) = effect_dist_prefix {
        program = program.with_effect_dist_prefix(prefix);
    }
    compile_definitions_program(profile_checker, &program, files, options_for)
}

/// Compiles standalone `Norn.question` definitions in `file`.
///
/// # Errors
///
/// Returns [`AuthoringError`] if `file` is missing or invalid in `program`.
pub fn compile_questions_program(
    profile_checker: &ProfileChecker,
    program: &AuthoringProgram,
    file: &str,
    limits: CompilerLimits,
) -> Result<QuestionsCompileOutcome, AuthoringError> {
    let started = Instant::now();
    let limits = limits.clamped();
    let reports = profile_checker.check_program(program, &[file])?;
    if let Some(report) = reports.into_iter().next()
        && !report.diagnostics.is_empty()
    {
        return Ok(QuestionsCompileOutcome::Err {
            diagnostics: report.diagnostics,
        });
    }
    let sf = program.source_file(file)?;
    let raw_program = program.raw_program();
    if let Err(err) = check_closure(raw_program, sf, limits) {
        return Ok(QuestionsCompileOutcome::Err {
            diagnostics: vec![err.into_diagnostic()],
        });
    }
    let mut expansion = new_budget(limits, started);
    let ctx = Context::default();
    let mut checker = raw_program.get_type_checker_for_file_exclusive(&ctx, sf);
    let mut compiler = DefinitionCompiler::new(DefCompilerConfig {
        intrinsics_module: &profile_checker.profile().intrinsics_module,
        effect_dist_prefix: program.effect_dist_prefix(),
        vfs: program.vfs(),
        program: raw_program,
        started,
        limits,
        tool_root: "/",
        forbid_unsafe_zen: false,
    });
    match compiler.compile_questions(&mut checker, &mut expansion, sf) {
        Ok(questions) => Ok(QuestionsCompileOutcome::Ok { questions }),
        Err(err) => Ok(QuestionsCompileOutcome::Err {
            diagnostics: vec![err.into_diagnostic()],
        }),
    }
}

/// Compiles standalone `Norn.dashboard` definitions in `file`.
///
/// # Errors
///
/// Returns [`AuthoringError`] if `file` is missing or invalid in `program`.
pub fn compile_dashboards_program(
    profile_checker: &ProfileChecker,
    program: &AuthoringProgram,
    file: &str,
    limits: CompilerLimits,
) -> Result<DashboardsCompileOutcome, AuthoringError> {
    let started = Instant::now();
    let limits = limits.clamped();
    let reports = profile_checker.check_program(program, &[file])?;
    if let Some(report) = reports.into_iter().next()
        && !report.diagnostics.is_empty()
    {
        return Ok(DashboardsCompileOutcome::Err {
            diagnostics: report.diagnostics,
        });
    }
    let sf = program.source_file(file)?;
    let raw_program = program.raw_program();
    if let Err(err) = check_closure(raw_program, sf, limits) {
        return Ok(DashboardsCompileOutcome::Err {
            diagnostics: vec![err.into_diagnostic()],
        });
    }
    let mut expansion = new_budget(limits, started);
    let ctx = Context::default();
    let mut checker = raw_program.get_type_checker_for_file_exclusive(&ctx, sf);
    let mut compiler = DefinitionCompiler::new(DefCompilerConfig {
        intrinsics_module: &profile_checker.profile().intrinsics_module,
        effect_dist_prefix: program.effect_dist_prefix(),
        vfs: program.vfs(),
        program: raw_program,
        started,
        limits,
        tool_root: "/",
        forbid_unsafe_zen: false,
    });
    match compiler.compile_dashboards(&mut checker, &mut expansion, sf) {
        Ok(dashboards) => Ok(DashboardsCompileOutcome::Ok { dashboards }),
        Err(err) => Ok(DashboardsCompileOutcome::Err {
            diagnostics: vec![err.into_diagnostic()],
        }),
    }
}

/// Compiles every exported top-level Effect Schema const in `file` to its closed [`ValueSchema`].
///
/// # Errors
///
/// Returns [`AuthoringError`] if `file` is missing or invalid in `program`.
pub fn compile_value_schemas_program(
    profile_checker: &ProfileChecker,
    program: &AuthoringProgram,
    file: &str,
    limits: CompilerLimits,
) -> Result<ValueSchemasCompileOutcome, AuthoringError> {
    let started = Instant::now();
    let limits = limits.clamped();
    let reports = profile_checker.check_program(program, &[file])?;
    if let Some(report) = reports.into_iter().next()
        && !report.diagnostics.is_empty()
    {
        return Ok(ValueSchemasCompileOutcome::Err {
            diagnostics: report.diagnostics,
        });
    }
    let sf = program.source_file(file)?;
    let raw_program = program.raw_program();
    if let Err(err) = check_closure(raw_program, sf, limits) {
        return Ok(ValueSchemasCompileOutcome::Err {
            diagnostics: vec![err.into_diagnostic()],
        });
    }
    let mut expansion = new_budget(limits, started);
    let ctx = Context::default();
    let mut checker = raw_program.get_type_checker_for_file_exclusive(&ctx, sf);
    let mut compiler = DefinitionCompiler::new(DefCompilerConfig {
        intrinsics_module: &profile_checker.profile().intrinsics_module,
        effect_dist_prefix: program.effect_dist_prefix(),
        vfs: program.vfs(),
        program: raw_program,
        started,
        limits,
        tool_root: "/",
        forbid_unsafe_zen: false,
    });
    match compiler.compile_value_schemas(&mut checker, &mut expansion, sf) {
        Ok(schemas) => Ok(ValueSchemasCompileOutcome::Ok { schemas }),
        Err(err) => Ok(ValueSchemasCompileOutcome::Err {
            diagnostics: vec![err.into_diagnostic()],
        }),
    }
}

fn map_build_diagnostics(
    diags: Vec<BuildDiagnostic>,
    program: &'static tsrs_compiler::Program,
    source: P<SourceFile>,
    tool_root: &str,
) -> Vec<ProfileDiagnostic> {
    diags
        .into_iter()
        .map(|d| {
            let start = match &d.at {
                Some(at) => Position {
                    line: at.line,
                    column: at.column.unwrap_or(1),
                },
                None => Position { line: 1, column: 1 },
            };
            let target_sf = d.at.as_ref().and_then(|at| {
                program
                    .get_source_files()
                    .iter()
                    .copied()
                    .find(|s| relative_virtual_path(tool_root, s.file_name()) == at.file)
            });
            let target: P<Node> = target_sf.map_or_else(|| source.as_node(), |sf| sf.as_node());
            let mut diag = diagnostic_for_node(target, &d.code, &d.message);
            diag.start = start;
            diag
        })
        .collect()
}
