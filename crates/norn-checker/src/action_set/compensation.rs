//! ACTION-COMPENSATION-1 (D4): Ring-0 checks on `ActionRoot.compensation`.
//!
//! An action declaring `compensation: { actionId }` names the compensating
//! action that reverses or offsets its effect. Before admission:
//! - the target `actionId` must resolve uniquely in the candidate set or
//!   installed base ([`codes::UNRESOLVED_COMPENSATION`],
//!   [`codes::INVALID_COMPENSATION`]);
//! - the forward action's envelope scope must be able to see the compensating
//!   action's scope ([`crate::artifact_set::codes::INCOMPATIBLE_SCOPE`]);
//! - the compensation graph must be acyclic (no self-compensation or longer
//!   compensation loops; [`codes::COMPENSATION_CYCLE`]);
//! - the compensating action must be ledgered, not `rejected`/`superseded`,
//!   require only scope dimensions already required by the forward action, and
//!   (when both actions fire transitions on the same effective lifecycle) be
//!   reachable from the forward transition's destination state
//!   ([`codes::INVALID_COMPENSATION`]).

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use norn_ir::{ActionEffectProgram, ActionRoot, ArtifactEnvelope, ArtifactLifecycle};

use super::{Model, codes, decode_action, finding};
use crate::Diagnostic;
use crate::artifact_set::scope_finding;
use crate::entity_scopes::ScopedViews;

const COMPENSATION_POINTER: &str = "/payload/compensation/actionId";

type ActionEntry<'a> = (&'a ArtifactEnvelope, &'a ActionRoot);

/// Check every candidate action's `compensation` declaration over the resolved
/// candidate set and installed `base`.
pub(super) fn compensation_findings(
    actions: &[(&ArtifactEnvelope, ActionRoot)],
    base: &[ArtifactEnvelope],
    views: &mut ScopedViews<'_>,
) -> Vec<Diagnostic> {
    let base_actions: Vec<(&ArtifactEnvelope, ActionRoot)> = base
        .iter()
        .filter_map(|artifact| decode_action(artifact).map(|root| (artifact, root)))
        .collect();
    let mut by_action_id: BTreeMap<&str, Vec<ActionEntry<'_>>> = BTreeMap::new();
    for (artifact, root) in actions.iter().chain(base_actions.iter()) {
        by_action_id
            .entry(root.action_id.as_str())
            .or_default()
            .push((*artifact, root));
    }

    let mut findings = Vec::new();
    for (artifact, root) in actions {
        let Some(compensation) = &root.compensation else {
            continue;
        };
        let target_id = compensation.action_id.as_str();
        let matches = by_action_id.get(target_id).map_or(&[][..], Vec::as_slice);
        let Some(&(comp_artifact, comp_root)) = (match matches {
            [] => {
                findings.push(finding(
                    codes::UNRESOLVED_COMPENSATION,
                    format!(
                        "compensation names action {target_id:?}, which is not an action of the artifact set or the installed base"
                    ),
                    artifact,
                    COMPENSATION_POINTER,
                ));
                None
            }
            [single] => Some(single),
            _multiple => {
                findings.push(finding(
                    codes::INVALID_COMPENSATION,
                    format!(
                        "compensating action {target_id:?} is invalid: actionId resolves ambiguously across multiple action artifacts"
                    ),
                    artifact,
                    COMPENSATION_POINTER,
                ));
                None
            }
        }) else {
            continue;
        };

        findings.extend(scope_finding(artifact, comp_artifact, COMPENSATION_POINTER));
        if let Some(cycle) =
            find_compensation_cycle(artifact, root.action_id.as_str(), target_id, &by_action_id)
        {
            findings.push(finding(
                codes::COMPENSATION_CYCLE,
                format!("action compensation forms a cycle: {cycle}"),
                artifact,
                COMPENSATION_POINTER,
            ));
        }
        check_compensation_contract(
            artifact,
            root,
            comp_artifact,
            comp_root,
            views.of(artifact).model(),
            &mut findings,
        );
    }
    findings
}

/// Follow unique compensation edges from `start_artifact` (`start_id ->
/// first_target`); returns the formatted cycle path (`a -> b -> a`) if the
/// chain revisits a uniquely resolved action artifact. Stops without reporting
/// a cycle if any edge resolves ambiguously or is unresolved.
fn find_compensation_cycle(
    start_artifact: &ArtifactEnvelope,
    start_id: &str,
    first_target: &str,
    by_action_id: &BTreeMap<&str, Vec<ActionEntry<'_>>>,
) -> Option<String> {
    let mut visited: BTreeSet<(&str, &str)> = BTreeSet::from([(
        start_artifact.artifact_id.as_str(),
        start_artifact.version.as_str(),
    )]);
    let mut chain: Vec<&str> = vec![start_id];
    let mut cursor = first_target;
    loop {
        let [(next_artifact, next_root)] = by_action_id.get(cursor)?.as_slice() else {
            return None;
        };
        let key = (
            next_artifact.artifact_id.as_str(),
            next_artifact.version.as_str(),
        );
        if !visited.insert(key) {
            chain.push(cursor);
            return Some(chain.join(" -> "));
        }
        chain.push(cursor);
        let next_comp = next_root.compensation.as_ref()?;
        cursor = next_comp.action_id.as_str();
    }
}

fn check_compensation_contract(
    artifact: &ArtifactEnvelope,
    root: &ActionRoot,
    comp_artifact: &ArtifactEnvelope,
    comp_root: &ActionRoot,
    model: &Model<'_>,
    findings: &mut Vec<Diagnostic>,
) {
    let target_id = comp_root.action_id.as_str();
    if matches!(
        comp_artifact.lifecycle,
        ArtifactLifecycle::Rejected | ArtifactLifecycle::Superseded
    ) {
        findings.push(finding(
            codes::INVALID_COMPENSATION,
            format!(
                "compensating action {target_id:?} is invalid: its artifact lifecycle is {:?}",
                comp_artifact.lifecycle
            ),
            artifact,
            COMPENSATION_POINTER,
        ));
    }
    if !comp_root.ledgered {
        findings.push(finding(
            codes::INVALID_COMPENSATION,
            format!(
                "compensating action {target_id:?} is invalid: compensating actions must be ledgered so reversal appends auditable history"
            ),
            artifact,
            COMPENSATION_POINTER,
        ));
    }
    let mut missing_scopes: Vec<&str> = Vec::new();
    for required in &comp_root.scope_required {
        if !root.scope_required.iter().any(|scope| scope == required)
            && !missing_scopes.contains(&required.as_str())
        {
            missing_scopes.push(required.as_str());
        }
    }
    if !missing_scopes.is_empty() {
        findings.push(finding(
            codes::INVALID_COMPENSATION,
            format!(
                "compensating action {target_id:?} is invalid: requires scope dimensions {missing_scopes:?} not required by forward action {:?}",
                root.action_id
            ),
            artifact,
            COMPENSATION_POINTER,
        ));
    }
    check_transition_reachability(artifact, root, comp_root, model, findings);
}

/// When both the forward action and its compensating action fire transitions on
/// the same effective lifecycle (resolving each effect's pinned entity artifact
/// and version first, covering both identical target definitions and a forward
/// subtype of the compensating target type), at least one source state of the
/// compensating transition must be reachable from the forward transition's
/// destination state.
fn check_transition_reachability(
    artifact: &ArtifactEnvelope,
    root: &ActionRoot,
    comp_root: &ActionRoot,
    model: &Model<'_>,
    findings: &mut Vec<Diagnostic>,
) {
    let (Some(fwd_program), Some(comp_program)) = (&root.effect_program, &comp_root.effect_program)
    else {
        return;
    };
    let (
        ActionEffectProgram::FireTransition {
            object_kind: fwd_kind,
            lifecycle: fwd_lc,
            transition: fwd_tr,
            ..
        },
        ActionEffectProgram::FireTransition {
            object_kind: comp_kind,
            lifecycle: comp_lc,
            transition: comp_tr,
            ..
        },
    ) = (fwd_program, comp_program)
    else {
        return;
    };
    if fwd_lc != comp_lc {
        return;
    }
    let (Some(fwd_admitted), Some(comp_admitted)) = (
        model.effect_target(fwd_program),
        model.effect_target(comp_program),
    ) else {
        return;
    };
    if fwd_kind == comp_kind
        && (fwd_admitted.artifact().artifact_id != comp_admitted.artifact().artifact_id
            || fwd_admitted.artifact().version != comp_admitted.artifact().version)
    {
        return;
    }
    let (Some(fwd_type), Some(_comp_type)) = (fwd_admitted.effective(), comp_admitted.effective())
    else {
        return;
    };
    if !fwd_type
        .lineage
        .iter()
        .any(|ancestor| ancestor.as_str() == comp_kind)
    {
        return;
    }
    let Some(effective_lc) = fwd_type.lifecycle(fwd_lc.as_str()) else {
        return;
    };
    let transitions = &effective_lc.lifecycle.transitions;
    let Some(fwd_def) = transitions.iter().find(|t| t.id == *fwd_tr) else {
        return;
    };
    let Some(comp_def) = transitions.iter().find(|t| t.id == *comp_tr) else {
        return;
    };
    let reachable = reachable_states(fwd_def.to.as_str(), transitions);
    if !comp_def
        .from
        .iter()
        .any(|source| reachable.contains(source.as_str()))
    {
        findings.push(finding(
            codes::INVALID_COMPENSATION,
            format!(
                "compensating action {:?} is invalid: transition {:?} on {:?}.{:?} fires only from {:?}, which is unreachable from forward transition {:?}'s target state {:?}",
                comp_root.action_id,
                comp_tr.as_str(),
                fwd_kind,
                fwd_lc.as_str(),
                comp_def.from,
                fwd_tr.as_str(),
                fwd_def.to,
            ),
            artifact,
            COMPENSATION_POINTER,
        ));
    }
}

fn reachable_states<'a>(
    start: &'a str,
    transitions: &'a [norn_ir::Transition],
) -> BTreeSet<&'a str> {
    let mut visited: BTreeSet<&'a str> = BTreeSet::from([start]);
    let mut queue: VecDeque<&'a str> = VecDeque::from([start]);
    while let Some(state) = queue.pop_front() {
        for transition in transitions {
            if transition.from.iter().any(|from| from == state)
                && visited.insert(transition.to.as_str())
            {
                queue.push_back(transition.to.as_str());
            }
        }
    }
    visited
}
