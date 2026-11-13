//! A page machine composed with the backend lifecycle it acts on (UI-LIFECYCLE-1, #1236).
//!
//! `WidgetMachine.lifecycle` names an object type's lifecycle, the page transitions that fire one
//! of its transitions (`syncs`), and the lifecycle states each page state may be shown in
//! (`assumes`). The page and the lifecycle are lowered to the shared soundness core and composed
//! ([`soundness::compose`]): a synced page transition fires only together with its lifecycle
//! transition, every other page transition fires while the object stays put, and every lifecycle
//! transition the page does not fire (another actor, a timer, an event) may fire at any time. The
//! page opens on the object in any lifecycle state it can be in that the opening state assumes.
//!
//! A product state ends only when both sides have ([`compose::product_ending`]), so these laws
//! catch what neither machine shows alone:
//!
//! - a page that shows an ending while the object has not reached one, with no way on, is stuck
//!   (`stuck`), with a shortest run through both machines;
//! - a page state shown while the object is somewhere it assumes it is not is
//!   `assumption_violated`, with a shortest run;
//! - a page transition synced with a lifecycle transition is the `accepted` outcome of an action
//!   call (`sync_not_accepted`), and that action's decision can emit the transition: its closed
//!   fact schema names the lifecycle and the transition (`sync_unfired`). The checker asks the
//!   schema the evaluator enforces, not the expressions that compute a value;
//! - an ending reachable only through guards is Unknown (`completion_unknown`, information), and
//!   a synced lifecycle transition no run takes is `uncovered_transition` (information).
//!
//! Like the page laws, this is structural and adds no runtime authority.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::{
    ArtifactEnvelope, ArtifactPayload, JsonValue, WidgetCallOutcome, WidgetMachine, WidgetMachineCall,
    WidgetMachineEventSource, WidgetMachineLifecycle, WidgetUx,
};

use crate::action_set::Model;
use crate::entity_instance::EffectiveLifecycle;
use crate::lifecycle_writes::{FactSchema, fact_decisions};
use crate::lifecycles::Machine;
use crate::soundness::compose::{self, Product, Sync};
use crate::soundness::{self, Completion, Net};
use crate::{Diagnostic, DiagnosticCode, DiagnosticRemediation, DiagnosticSeverity, Span};

const UNKNOWN_LIFECYCLE: &str = "widget.lifecycle.unknown_lifecycle";
const UNKNOWN_REFERENCE: &str = "widget.lifecycle.unknown_reference";
const SYNC_NOT_ACCEPTED: &str = "widget.lifecycle.sync_not_accepted";
const SYNC_UNFIRED: &str = "widget.lifecycle.sync_unfired";
const STUCK: &str = "widget.lifecycle.stuck";
const COMPLETION_UNKNOWN: &str = "widget.lifecycle.completion_unknown";
const ASSUMPTION_VIOLATED: &str = "widget.lifecycle.assumption_violated";
const UNCOVERED_TRANSITION: &str = "widget.lifecycle.uncovered_transition";

/// Every code these laws emit besides the composition's own refusals
/// ([`compose::ComposeError::code`], `lifecycle.soundness.*`).
pub const CODES: [&str; 8] = [
    UNKNOWN_LIFECYCLE,
    UNKNOWN_REFERENCE,
    SYNC_NOT_ACCEPTED,
    SYNC_UNFIRED,
    STUCK,
    COMPLETION_UNKNOWN,
    ASSUMPTION_VIOLATED,
    UNCOVERED_TRANSITION,
];

const INFO: [&str; 2] = [COMPLETION_UNKNOWN, UNCOVERED_TRANSITION];

struct Finding {
    code: &'static str,
    pointer: String,
    message: String,
}

/// Compose every page machine of one widget that names a lifecycle with that lifecycle.
pub(crate) fn check(
    owner: &ArtifactEnvelope,
    ux: &WidgetUx,
    combined: &[ArtifactEnvelope],
    model: &Model<'_>,
) -> Vec<Diagnostic> {
    let mut findings = Vec::new();
    for (index, page) in ux.pages.iter().enumerate() {
        let Some(machine) = &page.machine else {
            continue;
        };
        let Some(lifecycle) = &machine.lifecycle else {
            continue;
        };
        let base = format!("/payload/ux/pages/{index}/machine/lifecycle");
        let page = Page {
            ux,
            machine,
            lifecycle,
            base: &base,
            combined,
        };
        page.check(model, &mut findings);
    }
    findings
        .into_iter()
        .map(|finding| Diagnostic {
            code: DiagnosticCode::new(finding.code),
            severity: if INFO.contains(&finding.code) {
                DiagnosticSeverity::Info
            } else {
                DiagnosticSeverity::Error
            },
            message: finding.message,
            span: Some(Span::pointer(finding.pointer)),
            artifact_id: Some(owner.artifact_id.clone()),
            remediation: Some(DiagnosticRemediation::FixDefinition),
        })
        .collect()
}

struct Page<'p> {
    ux: &'p WidgetUx,
    machine: &'p WidgetMachine,
    lifecycle: &'p WidgetMachineLifecycle,
    base: &'p str,
    combined: &'p [ArtifactEnvelope],
}

/// The backend side: the effective lifecycle as a machine and a net.
struct Backend<'l> {
    machine: Machine<'l>,
    net: Net,
}

impl Backend<'_> {
    fn state(&self, id: &str) -> Option<usize> {
        self.machine.states.iter().position(|state| *state == id)
    }

    fn transition(&self, id: &str) -> Option<usize> {
        self.machine
            .transitions
            .iter()
            .position(|edge| edge.id.as_str() == id)
    }
}

impl Page<'_> {
    fn push(findings: &mut Vec<Finding>, code: &'static str, pointer: String, message: String) {
        findings.push(Finding {
            code,
            pointer,
            message,
        });
    }

    fn effective<'m>(
        &self,
        model: &'m Model<'_>,
        findings: &mut Vec<Finding>,
    ) -> Option<&'m EffectiveLifecycle> {
        let kind = self.lifecycle.object_type.as_str();
        let property = self.lifecycle.property.as_str();
        let at = |member: &str| format!("{}/{member}", self.base);
        let Some(defined) = model.object_type(kind) else {
            Self::push(
                findings,
                UNKNOWN_LIFECYCLE,
                at("objectType"),
                format!("{kind:?} is not an object type of the artifact set or the installed base"),
            );
            return None;
        };
        match &defined.lifecycles {
            Err(reason) => {
                Self::push(
                    findings,
                    UNKNOWN_LIFECYCLE,
                    at("objectType"),
                    format!("the lifecycles of {kind:?} cannot be resolved ({reason})"),
                );
                None
            }
            Ok(lifecycles) => {
                let found = lifecycles
                    .iter()
                    .find(|effective| effective.lifecycle.property.as_str() == property);
                if found.is_none() {
                    Self::push(
                        findings,
                        UNKNOWN_LIFECYCLE,
                        at("property"),
                        format!("{kind:?} declares no lifecycle on {property:?}"),
                    );
                }
                found
            }
        }
    }

    fn check(&self, model: &Model<'_>, findings: &mut Vec<Finding>) {
        let Some(effective) = self.effective(model, findings) else {
            return;
        };
        let machine = Machine::declared(
            &effective.lifecycle,
            effective.states.iter().map(String::as_str).collect(),
        );
        let net = crate::lifecycle_soundness::net(&machine);
        let backend = Backend { machine, net };
        let Some(sync) = self.sync_table(&backend, findings) else {
            return;
        };
        let Some(assumed) = self.assumptions(&backend, findings) else {
            return;
        };
        let ui = crate::ui_machine::page_net(self.machine);
        let opening = ui.initial.first().copied();
        let backend_net = self.opening(&backend, opening, &assumed);
        let product = match compose::compose(&ui, &backend_net, &sync) {
            Ok(product) => product,
            Err(error) => {
                Self::push(
                    findings,
                    error.code(),
                    self.base.to_owned(),
                    format!(
                        "the page and the {:?} lifecycle cannot be composed ({error:?})",
                        self.lifecycle.property
                    ),
                );
                return;
            }
        };
        self.completion(&ui, &backend, &product, findings);
        let allows = |ui_state: usize, backend_state: usize| {
            assumed
                .get(&ui_state)
                .is_none_or(|states| states.contains(&backend_state))
        };
        let mut reported = BTreeSet::new();
        for (state, run) in product.assumption_violations(allows) {
            let Some((ui_state, backend_state)) = product.pairs.get(state).copied() else {
                continue;
            };
            if !reported.insert(ui_state) {
                continue;
            }
            let pointer = self.assumption_pointer(ui_state);
            let message = format!(
                "page state `{}` is shown while the object is `{}`, which it does not assume{}",
                self.ui_state(ui_state),
                backend.machine.states.get(backend_state).copied().unwrap_or_default(),
                self.run(&ui, &backend, &product, &run)
            );
            Self::push(findings, ASSUMPTION_VIOLATED, pointer, message);
        }
        let taken = product.backend_labels(&backend_net);
        for (index, entry) in self.lifecycle.syncs.iter().enumerate() {
            if let Some(label) = backend.transition(&entry.lifecycle_transition)
                && !taken.contains(&label)
            {
                Self::push(
                    findings,
                    UNCOVERED_TRANSITION,
                    format!("{}/syncs/{index}", self.base),
                    format!(
                        "no run of the page takes `{}`: the page never fires it",
                        entry.lifecycle_transition
                    ),
                );
            }
        }
    }

    /// The backend net the page opens on: every lifecycle state the object can be in (reachable
    /// from its initial states) that the page's opening state assumes.
    fn opening(
        &self,
        backend: &Backend<'_>,
        opening: Option<usize>,
        assumed: &BTreeMap<usize, BTreeSet<usize>>,
    ) -> Net {
        let verdict = soundness::analyse(&backend.net);
        let allowed = opening.and_then(|state| assumed.get(&state));
        let mut net = backend.net.clone();
        net.initial = (0..net.states)
            .filter(|state| {
                verdict
                    .states
                    .get(*state)
                    .is_some_and(|found| found.reachable)
                    && allowed.is_none_or(|states| states.contains(state))
            })
            .collect();
        net
    }

    fn ui_state(&self, index: usize) -> &str {
        self.machine
            .states
            .get(index)
            .map_or("", |state| state.state_id.as_str())
    }

    fn state_pointer(&self, ui_state: usize) -> String {
        let base = self.base.trim_end_matches("/lifecycle");
        format!("{base}/states/{ui_state}")
    }

    fn assumption_pointer(&self, ui_state: usize) -> String {
        let id = self.ui_state(ui_state);
        self.lifecycle
            .assumes
            .iter()
            .position(|entry| entry.state_id == id)
            .map_or_else(
                || self.state_pointer(ui_state),
                |index| format!("{}/assumes/{index}", self.base),
            )
    }

    /// The sync table, checked: every entry names an `accepted` page transition of an action call
    /// whose decision can emit the lifecycle transition. `None` when an entry is unusable.
    fn sync_table(&self, backend: &Backend<'_>, findings: &mut Vec<Finding>) -> Option<Vec<Sync>> {
        let mut table = Vec::new();
        let mut usable = true;
        for (index, entry) in self.lifecycle.syncs.iter().enumerate() {
            let pointer = format!("{}/syncs/{index}", self.base);
            let page = self
                .machine
                .transitions
                .iter()
                .position(|transition| transition.transition_id == entry.transition_id);
            let lifecycle = backend.transition(&entry.lifecycle_transition);
            let (Some(page), Some(lifecycle)) = (page, lifecycle) else {
                Self::push(
                    findings,
                    UNKNOWN_REFERENCE,
                    pointer,
                    format!(
                        "the sync names page transition `{}` and lifecycle transition `{}`; {}",
                        entry.transition_id,
                        entry.lifecycle_transition,
                        if page.is_none() {
                            "the page declares no such transition"
                        } else {
                            "the lifecycle declares no such transition"
                        }
                    ),
                );
                usable = false;
                continue;
            };
            match self.fired_action(page) {
                None => {
                    Self::push(
                        findings,
                        SYNC_NOT_ACCEPTED,
                        format!("{pointer}/transitionId"),
                        format!(
                            "`{}` is not the accepted outcome of an action call, so it fires no lifecycle transition",
                            entry.transition_id
                        ),
                    );
                    usable = false;
                }
                Some(binding) => {
                    if let Err(reason) = self.emits(binding, &entry.lifecycle_transition) {
                        Self::push(
                            findings,
                            SYNC_UNFIRED,
                            format!("{pointer}/lifecycleTransition"),
                            format!(
                                "the action behind `{}` cannot fire `{}`: {reason}",
                                entry.transition_id, entry.lifecycle_transition
                            ),
                        );
                    }
                }
            }
            table.push(Sync::Together {
                ui: page,
                backend: lifecycle,
            });
        }
        usable.then_some(table)
    }

    /// The action binding whose `accepted` outcome page transition `index` is.
    fn fired_action(&self, index: usize) -> Option<&str> {
        let transition = self.machine.transitions.get(index)?;
        let event = self
            .machine
            .events
            .iter()
            .find(|event| event.event_id == transition.on)?;
        let WidgetMachineEventSource::Outcome { outcome, .. } = &event.source else {
            return None;
        };
        if *outcome != WidgetCallOutcome::Accepted {
            return None;
        }
        let from = self
            .machine
            .states
            .iter()
            .find(|state| state.state_id == transition.from)?;
        match from.awaits.as_ref()? {
            WidgetMachineCall::Action { binding_id } => Some(binding_id.as_str()),
            WidgetMachineCall::Read { .. } | WidgetMachineCall::Operation { .. } => None,
        }
    }

    /// Whether the action of `binding` can emit lifecycle transition `id`: its synchronous
    /// decision's closed fact schema (the one the evaluator validates every emitted record
    /// against) names this lifecycle and this transition.
    fn emits(&self, binding: &str, id: &str) -> Result<(), String> {
        let bound = self
            .ux
            .action_bindings
            .iter()
            .find(|candidate| candidate.binding_id == binding)
            .ok_or_else(|| format!("the contract declares no action binding `{binding}`"))?;
        let action = self
            .combined
            .iter()
            .find(|artifact| {
                artifact.artifact_id == bound.action.id
                    && artifact.version == bound.action.version
                    && matches!(artifact.payload, ArtifactPayload::Action(_))
            })
            .ok_or_else(|| format!("action `{}` is not in the artifact set", bound.action.id))?;
        let payload = action.payload.to_json_value();
        let rule = payload
            .as_object()
            .and_then(|object| object.get("synchronousRule"))
            .and_then(JsonValue::as_object)
            .ok_or_else(|| "the action runs no synchronous decision".to_owned())?;
        let (Some(rule_id), Some(rule_version)) = (
            rule.get("artifactId").and_then(JsonValue::as_string),
            rule.get("version").and_then(JsonValue::as_string),
        ) else {
            return Err("the action's synchronous rule reference is malformed".to_owned());
        };
        let decision = self
            .combined
            .iter()
            .find(|artifact| artifact.artifact_id == rule_id && artifact.version == rule_version)
            .ok_or_else(|| format!("decision `{rule_id}` is not in the artifact set"))?;
        let decisions = fact_decisions([decision]);
        let Some((_, output)) = decisions.first() else {
            return Err(format!("decision `{rule_id}` emits no fact changes"));
        };
        let Some(schema) = FactSchema::of(output) else {
            return Err(format!("decision `{rule_id}` emits no fact changes"));
        };
        let names = |member: &str| schema.values(member).unwrap_or_default();
        if !names("lifecycle").contains(&self.lifecycle.property.as_str()) {
            return Err(format!(
                "decision `{rule_id}` transitions no {:?} lifecycle",
                self.lifecycle.property
            ));
        }
        if !names("transition").contains(&id) {
            return Err(format!(
                "decision `{rule_id}`'s fact schema does not name transition `{id}`"
            ));
        }
        Ok(())
    }

    /// The assumptions by page state index, as lifecycle state index sets. `None` when an entry
    /// names an undeclared state.
    fn assumptions(
        &self,
        backend: &Backend<'_>,
        findings: &mut Vec<Finding>,
    ) -> Option<BTreeMap<usize, BTreeSet<usize>>> {
        let mut assumed = BTreeMap::new();
        let mut usable = true;
        for (index, entry) in self.lifecycle.assumes.iter().enumerate() {
            let pointer = format!("{}/assumes/{index}", self.base);
            let Some(state) = self
                .machine
                .states
                .iter()
                .position(|state| state.state_id == entry.state_id)
            else {
                Self::push(
                    findings,
                    UNKNOWN_REFERENCE,
                    format!("{pointer}/stateId"),
                    format!("the page declares no state `{}`", entry.state_id),
                );
                usable = false;
                continue;
            };
            let mut states = BTreeSet::new();
            for (position, name) in entry.lifecycle_states.iter().enumerate() {
                if let Some(found) = backend.state(name) {
                    states.insert(found);
                } else {
                    Self::push(
                        findings,
                        UNKNOWN_REFERENCE,
                        format!("{pointer}/lifecycleStates/{position}"),
                        format!("the lifecycle declares no state `{name}`"),
                    );
                    usable = false;
                }
            }
            assumed.insert(state, states);
        }
        usable.then_some(assumed)
    }

    /// Option to complete over the product: one finding per page state, for its first pair.
    fn completion(
        &self,
        ui: &Net,
        backend: &Backend<'_>,
        product: &Product,
        findings: &mut Vec<Finding>,
    ) {
        let verdict = soundness::analyse(product.net());
        let mut reported = BTreeSet::new();
        for (state, found) in verdict.states.iter().enumerate() {
            let Some((ui_state, backend_state)) = product.pairs.get(state).copied() else {
                continue;
            };
            let code = if found.stuck() {
                STUCK
            } else if found.reachable && found.completion == Completion::Unknown {
                COMPLETION_UNKNOWN
            } else {
                continue;
            };
            if !reported.insert((code, ui_state)) {
                continue;
            }
            let run = soundness::witness(product.net(), state).unwrap_or_default();
            let object = backend.machine.states.get(backend_state).copied().unwrap_or_default();
            let message = if code == STUCK {
                format!(
                    "page state `{}` with the object `{object}`: no ending of both is reachable{}",
                    self.ui_state(ui_state),
                    self.run(ui, backend, product, &run)
                )
            } else {
                format!(
                    "page state `{}` with the object `{object}`: an ending of both is reachable only through guarded transitions{}",
                    self.ui_state(ui_state),
                    self.run(ui, backend, product, &run)
                )
            };
            Self::push(findings, code, self.state_pointer(ui_state), message);
        }
    }

    /// A product run as the steps of each side.
    fn run(&self, ui: &Net, backend: &Backend<'_>, product: &Product, run: &[usize]) -> String {
        let steps: Vec<String> = run
            .iter()
            .filter_map(|edge| product.net().edges.get(*edge))
            .filter_map(|edge| product.origins.get(edge.label))
            .map(|origin| {
                let page = origin
                    .ui
                    .and_then(|edge| ui.edges.get(edge))
                    .and_then(|edge| self.machine.transitions.get(edge.label))
                    .map(|transition| format!("page {}", transition.transition_id));
                let object = origin
                    .backend
                    .and_then(|edge| backend.net.edges.get(edge))
                    .and_then(|edge| backend.machine.transitions.get(edge.label))
                    .map(|transition| format!("object {}", transition.id.as_str()));
                match (page, object) {
                    (Some(page), Some(object)) => format!("{page} with {object}"),
                    (Some(step), None) | (None, Some(step)) => step,
                    (None, None) => String::new(),
                }
            })
            .collect();
        if steps.is_empty() {
            String::new()
        } else {
            format!("; shortest run: {}", steps.join(", "))
        }
    }
}
