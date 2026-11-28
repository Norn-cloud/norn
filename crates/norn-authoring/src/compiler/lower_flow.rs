//! Flow lowering (`FlowDef` → Run `entity` artifact, `decide` `rule` artifact,
//! source map, and draft flow source map).
//!
//! Faithful Rust port of `tools/norn-ts/src/builder/lower-flow.ts`.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::canonical::{CanonicalError, is_dotted_lower_id};
use super::expr::{Scope, zen_string};
use super::flow_source_map::{
    FLOW_MAX_ROWS, FlowDef, FlowSourceMapDraft, FlowSourceMapGraph, FnRef, PerformStep,
    RetryPolicy, SourceLocation, Step, TaggedErrorDecl, flow_source_map_graph,
};
use super::types::EnvelopeOptions;
use super::value_schema::{BudgetExceeded, ExpansionBudget, ValueSchema};

/// Build-time diagnostic produced by flow lowering (`BuildDiagnostic`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BuildDiagnostic {
    pub(crate) code: String,
    pub(crate) message: String,
    pub(crate) at: Option<SourceLocation>,
}

/// Display metadata extracted from a compiled flow (`FlowDisplay`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowDisplay {
    /// Flow id.
    pub id: String,
    /// Flow version.
    pub version: String,
    /// Optional human-readable title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Labelled steps in authoring order.
    pub steps: Vec<FlowStepLabel>,
}

/// One labelled perform or sleep step in [`FlowDisplay`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowStepLabel {
    /// Step id.
    pub id: String,
    /// Human-readable canvas title.
    pub label: String,
}

/// Successful output of [`lower_flow`].
#[derive(Clone, Debug)]
pub(crate) struct LowerFlowOutput {
    pub(crate) artifacts: Vec<Value>,
    pub(crate) source_map: Value,
    pub(crate) flow_source_map: Option<FlowSourceMapDraft>,
    pub(crate) flow: FlowDisplay,
}

/// Output of [`lower_flow`].
#[derive(Clone, Debug)]
pub(crate) enum LowerFlowOutcome {
    Ok(Box<LowerFlowOutput>),
    Err(Vec<BuildDiagnostic>),
}

/// Options controlling [`lower_flow`].
#[derive(Clone, Debug)]
pub(crate) struct LowerFlowOptions {
    pub(crate) envelope: EnvelopeOptions,
    pub(crate) include_flow_source_map: bool,
}

#[derive(Clone)]
pub(super) enum Frame {
    Rest {
        steps: Vec<Step>,
        index: usize,
        outer: BTreeSet<String>,
        source_map_context: Vec<String>,
    },
    Catch {
        tag: String,
        as_name: Option<String>,
        handler: Vec<Step>,
        node_source_map_id: Option<String>,
        source_map_context: Vec<String>,
    },
    Compose {
        name: String,
        schema: ValueSchema,
        outer: BTreeSet<String>,
        at: Option<SourceLocation>,
        source_map_context: Vec<String>,
    },
}

pub(super) struct Row {
    pub(super) id: String,
    pub(super) guard: String,
    pub(super) facts: Vec<String>,
    pub(super) commands: Vec<String>,
    pub(super) timers: Vec<String>,
    pub(super) description: String,
    pub(super) at: Option<SourceLocation>,
}

#[derive(Clone)]
pub(super) struct Path {
    pub(super) from: String,
    pub(super) event: String,
    pub(super) guards: Vec<String>,
    pub(super) scope: Scope,
    pub(super) facts: Vec<String>,
    pub(super) key: String,
    pub(super) source_map_context: Vec<String>,
    pub(super) source_map_owners: Vec<String>,
}

#[derive(Default)]
pub(super) struct SourceMapLinks {
    pub(super) states: BTreeSet<String>,
    pub(super) rows: BTreeSet<String>,
}

pub(super) struct AwaitTask {
    pub(super) node: Box<PerformStep>,
    pub(super) next: Vec<Frame>,
    pub(super) policy: Option<RetryPolicy>,
    pub(super) names: BTreeSet<String>,
    pub(super) attempt: u32,
    pub(super) source_map_context: Vec<String>,
    pub(super) owners: Vec<String>,
}

pub(super) struct RetryTask {
    pub(super) node: Box<PerformStep>,
    pub(super) next: Vec<Frame>,
    pub(super) policy: RetryPolicy,
    pub(super) names: BTreeSet<String>,
    pub(super) next_attempt: u32,
    pub(super) retrying: String,
    pub(super) source_map_context: Vec<String>,
    pub(super) owners: Vec<String>,
}

pub(super) enum ResumeTask {
    Sleep {
        step: String,
        state: String,
        names: BTreeSet<String>,
        source_map_context: Vec<String>,
        owners: Vec<String>,
        next: Vec<Frame>,
    },
    Awaiting(Box<AwaitTask>),
    Retrying(Box<RetryTask>),
}

pub(super) enum LowerBreak {
    Bound,
    Budget(BudgetExceeded),
    Canonical(CanonicalError),
}

impl From<BudgetExceeded> for LowerBreak {
    fn from(value: BudgetExceeded) -> Self {
        Self::Budget(value)
    }
}

impl From<CanonicalError> for LowerBreak {
    fn from(value: CanonicalError) -> Self {
        Self::Canonical(value)
    }
}

pub(super) fn snake_case_tag(tag: &str) -> String {
    let mut out = String::with_capacity(tag.len().saturating_add(4));
    let mut prev_alnum_lower = false;
    for ch in tag.chars() {
        if ch.is_ascii_uppercase() && prev_alnum_lower {
            out.push('_');
        }
        out.push(ch.to_ascii_lowercase());
        prev_alnum_lower = ch.is_ascii_lowercase() || ch.is_ascii_digit();
    }
    out
}

fn pascal_case_word(id: &str) -> String {
    let mut out = String::with_capacity(id.len());
    for seg in id.split('_') {
        let mut chars = seg.chars();
        if let Some(first) = chars.next() {
            out.push(first.to_ascii_uppercase());
            out.extend(chars);
        }
    }
    out
}

fn is_valid_flow_tail(tail: &str) -> bool {
    !tail.is_empty()
        && tail.split('_').all(|word| {
            let mut bytes = word.bytes();
            bytes.next().is_some_and(|b| b.is_ascii_lowercase())
                && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

pub(super) fn format_whole_f64(val: f64) -> String {
    if val.is_finite() && val.fract().abs().to_bits() == 0 {
        format!("{val:.0}")
    } else {
        val.to_string()
    }
}

fn step_labels(steps: &[Step], out: &mut Vec<FlowStepLabel>) {
    for step in steps {
        match step {
            Step::Perform(p) => {
                if let Some(label) = &p.label {
                    out.push(FlowStepLabel {
                        id: p.step.clone(),
                        label: label.clone(),
                    });
                }
            }
            Step::Retry { perform, .. } => {
                if let Some(label) = &perform.label {
                    out.push(FlowStepLabel {
                        id: perform.step.clone(),
                        label: label.clone(),
                    });
                }
            }
            Step::Sleep { step, label, .. } => {
                if let Some(label) = label {
                    out.push(FlowStepLabel {
                        id: step.clone(),
                        label: label.clone(),
                    });
                }
            }
            Step::Route {
                branches,
                otherwise,
                ..
            } => {
                for b in branches {
                    step_labels(&b.then, out);
                }
                step_labels(otherwise, out);
            }
            Step::CatchTag { body, handler, .. } => {
                step_labels(body, out);
                step_labels(handler, out);
            }
            Step::Compose { body, .. } => step_labels(body, out),
            Step::All { steps, .. } => step_labels(steps, out),
            Step::Fail { .. } | Step::Succeed { .. } | Step::Value { .. } => {}
        }
    }
}

/// Lowers a compiled [`FlowDef`] into its Run `entity` artifact, `decide` `rule`
/// artifact, source map, and optional draft flow source map.
pub(crate) fn lower_flow(
    def: &FlowDef,
    options: &LowerFlowOptions,
    budget: &mut ExpansionBudget,
) -> LowerFlowOutcome {
    let mut lowerer = FlowLowering::new(def, options, budget);
    match lowerer.run() {
        Ok(outcome) => outcome,
        Err(LowerBreak::Budget(BudgetExceeded(message))) => {
            LowerFlowOutcome::Err(vec![BuildDiagnostic {
                code: "NORN-FLOW-UNBOUNDED".to_owned(),
                message,
                at: def.at.clone(),
            }])
        }
        Err(LowerBreak::Canonical(err)) => LowerFlowOutcome::Err(vec![BuildDiagnostic {
            code: "NORN-FLOW-LITERAL".to_owned(),
            message: err.to_string(),
            at: def.at.clone(),
        }]),
        Err(LowerBreak::Bound) => LowerFlowOutcome::Err(lowerer.diagnostics),
    }
}

pub(super) struct FlowLowering<'a> {
    pub(super) def: &'a FlowDef,
    pub(super) options: &'a LowerFlowOptions,
    pub(super) budget: &'a mut ExpansionBudget,
    pub(super) diagnostics: Vec<BuildDiagnostic>,
    pub(super) namespace: String,
    pub(super) run_type: String,
    pub(super) rows: Vec<Row>,
    pub(super) row_counts: BTreeMap<String, usize>,
    pub(super) transitions_order: Vec<String>,
    pub(super) transitions: BTreeMap<String, Vec<String>>,
    pub(super) states_order: Vec<String>,
    pub(super) states: BTreeMap<String, Option<SourceLocation>>,
    pub(super) terminal: BTreeSet<String>,
    pub(super) steps_order: Vec<String>,
    pub(super) steps_at: BTreeMap<String, Option<SourceLocation>>,
    pub(super) performs_order: Vec<String>,
    pub(super) performs: BTreeMap<String, PerformStep>,
    pub(super) sleeps: BTreeSet<String>,
    pub(super) functions: BTreeMap<String, FnRef>,
    pub(super) errors: BTreeMap<String, TaggedErrorDecl>,
    pub(super) tag_states: BTreeMap<String, String>,
    pub(super) binding_order: Vec<String>,
    pub(super) binding_schemas: BTreeMap<String, ValueSchema>,
    pub(super) result_schema: Option<ValueSchema>,
    pub(super) uncaught: Option<ValueSchema>,
    pub(super) queue: VecDeque<ResumeTask>,
    pub(super) registered: BTreeSet<String>,
    pub(super) retried: BTreeSet<String>,
    pub(super) proceed_depth: usize,
    pub(super) source_map_graph: Option<FlowSourceMapGraph>,
    pub(super) source_map_links: BTreeMap<String, SourceMapLinks>,
}

impl<'a> FlowLowering<'a> {
    fn new(
        def: &'a FlowDef,
        options: &'a LowerFlowOptions,
        budget: &'a mut ExpansionBudget,
    ) -> Self {
        let mut diagnostics = Vec::new();
        let id = def.meta.id.as_str();
        if !is_dotted_lower_id(id) {
            diagnostics.push(BuildDiagnostic {
                code: "NORN-FLOW-LITERAL".to_owned(),
                message: format!(
                    "flow id {id:?} must be dotted lower-case segments (namespace.name)"
                ),
                at: def.at.clone(),
            });
        }
        let (namespace, tail) = id.rsplit_once('.').unwrap_or(("", id));
        if !is_valid_flow_tail(tail) {
            diagnostics.push(BuildDiagnostic {
                code: "NORN-FLOW-LITERAL".to_owned(),
                message: "the flow name must use nonempty, letter-led words separated by single underscores".to_owned(),
                at: def.at.clone(),
            });
        }
        let run_type = format!("{namespace}.{}Run", pascal_case_word(tail));
        let mut states = BTreeMap::new();
        states.insert("started".to_owned(), None);
        Self {
            def,
            options,
            budget,
            diagnostics,
            namespace: namespace.to_owned(),
            run_type,
            rows: Vec::new(),
            row_counts: BTreeMap::new(),
            transitions_order: Vec::new(),
            transitions: BTreeMap::new(),
            states_order: vec!["started".to_owned()],
            states,
            terminal: BTreeSet::new(),
            steps_order: Vec::new(),
            steps_at: BTreeMap::new(),
            performs_order: Vec::new(),
            performs: BTreeMap::new(),
            sleeps: BTreeSet::new(),
            functions: BTreeMap::new(),
            errors: BTreeMap::new(),
            tag_states: BTreeMap::new(),
            binding_order: Vec::new(),
            binding_schemas: BTreeMap::new(),
            result_schema: None,
            uncaught: None,
            queue: VecDeque::new(),
            registered: BTreeSet::new(),
            retried: BTreeSet::new(),
            proceed_depth: 0,
            source_map_graph: None,
            source_map_links: BTreeMap::new(),
        }
    }

    pub(super) fn report(
        &mut self,
        code: &str,
        message: impl Into<String>,
        at: Option<&SourceLocation>,
    ) {
        self.diagnostics.push(BuildDiagnostic {
            code: code.to_owned(),
            message: message.into(),
            at: at.cloned(),
        });
    }

    fn run(&mut self) -> Result<LowerFlowOutcome, LowerBreak> {
        self.collect(&self.def.body);
        if !self.diagnostics.is_empty() {
            return Ok(LowerFlowOutcome::Err(std::mem::take(&mut self.diagnostics)));
        }
        self.init_source_map();
        let Ok(run_type_zen) = zen_string(&self.run_type) else {
            self.report(
                "NORN-FLOW-EXPRESSION",
                format!("invalid run type name {}", self.run_type),
                self.def.at.as_ref(),
            );
            return Ok(LowerFlowOutcome::Err(std::mem::take(&mut self.diagnostics)));
        };
        let start = Path {
            from: "started".to_owned(),
            event: "start".to_owned(),
            guards: vec![
                "slice.run == null".to_owned(),
                "event.kind == 'start'".to_owned(),
            ],
            scope: BTreeMap::new(),
            facts: vec![format!(
                "{{op: 'create', objectType: {run_type_zen}, key: event.runId, values: {{run_id: event.runId, status: 'started'}}}}"
            )],
            key: "event.runId".to_owned(),
            source_map_context: Vec::new(),
            source_map_owners: Vec::new(),
        };
        let root_cont = vec![Frame::Rest {
            steps: self.def.body.clone(),
            index: 0,
            outer: BTreeSet::new(),
            source_map_context: Vec::new(),
        }];
        self.run_worklist(start, root_cont)?;
        self.finish_outcome()
    }

    fn init_source_map(&mut self) {
        if !self.options.include_flow_source_map {
            return;
        }
        let graph = flow_source_map_graph(&self.def.body);
        if !graph.steps.is_empty() {
            for step in &graph.steps {
                self.source_map_links
                    .insert(step.id.clone(), SourceMapLinks::default());
            }
            self.source_map_graph = Some(graph);
        }
    }

    fn run_worklist(&mut self, start: Path, root_cont: Vec<Frame>) -> Result<(), LowerBreak> {
        match self.proceed(start, root_cont) {
            Ok(()) | Err(LowerBreak::Bound) => {}
            Err(err @ (LowerBreak::Budget(_) | LowerBreak::Canonical(_))) => return Err(err),
        }
        while let Some(task) = self.queue.pop_front() {
            let res = self.dispatch_task(task);
            match res {
                Ok(()) => {}
                Err(LowerBreak::Bound) => break,
                Err(err @ (LowerBreak::Budget(_) | LowerBreak::Canonical(_))) => return Err(err),
            }
        }
        Ok(())
    }

    fn dispatch_task(&mut self, task: ResumeTask) -> Result<(), LowerBreak> {
        match task {
            ResumeTask::Sleep {
                step,
                state,
                names,
                source_map_context,
                owners,
                next,
            } => self.resume_sleep(&step, &state, &names, source_map_context, owners, next),
            ResumeTask::Awaiting(task) => self.awaiting(&task),
            ResumeTask::Retrying(task) => self.retrying_resume(&task),
        }
    }

    fn finish_outcome(&mut self) -> Result<LowerFlowOutcome, LowerBreak> {
        if self.rows.len() > FLOW_MAX_ROWS {
            self.report(
                "NORN-FLOW-UNBOUNDED",
                format!(
                    "the flow lowers to {} decision rows, over the bound of {FLOW_MAX_ROWS}",
                    self.rows.len()
                ),
                self.def.at.as_ref(),
            );
        }
        if !self.diagnostics.is_empty() {
            return Ok(LowerFlowOutcome::Err(std::mem::take(&mut self.diagnostics)));
        }
        let entity = self.build_entity()?;
        let rule = self.build_rule()?;
        if !self.diagnostics.is_empty() {
            return Ok(LowerFlowOutcome::Err(std::mem::take(&mut self.diagnostics)));
        }
        let source_map = self.build_source_map()?;
        let flow_source_map = self.build_flow_source_map()?;
        let mut labels = Vec::new();
        step_labels(&self.def.body, &mut labels);
        let flow = FlowDisplay {
            id: self.def.meta.id.clone(),
            version: self.def.meta.version.clone(),
            title: self.def.meta.title.clone(),
            steps: labels,
        };
        Ok(LowerFlowOutcome::Ok(Box::new(LowerFlowOutput {
            artifacts: vec![entity, rule],
            source_map,
            flow_source_map,
            flow,
        })))
    }
}
