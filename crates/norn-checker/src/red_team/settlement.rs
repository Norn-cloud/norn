//! Unauthenticated external settlement Datalog check for `RED-TEAM-1`.

use std::collections::BTreeSet;

use norn_datalog::{
    Budget, ExecBudget, Facts, Literal, Mode, Program, RelationKind, Rule, evaluate,
};
use norn_ir::ActionEffectProgram;

use super::{
    ExtractedSet, UnauthenticatedSettlementFinding, as_str, atom, effective_object_lifecycles,
    pred, push_fact, s,
};

fn collect_external_settlement_facts(set: &ExtractedSet) -> Facts {
    let mut facts = Facts::new();
    let mut seen_external = BTreeSet::<String>::new();
    for (type_name, ty) in &set.object_types {
        if effective_object_lifecycles(set, ty)
            .iter()
            .any(|lc| !lc.events.is_empty())
            && seen_external.insert(type_name.clone())
        {
            push_fact(&mut facts, "has_external_event", vec![s(type_name)]);
        }
    }
    for action in &set.actions {
        if !action.commands.is_empty() {
            if let Some(program) = &action.effect_program {
                let kind = program.target().object_kind.to_owned();
                if seen_external.insert(kind.clone()) {
                    push_fact(&mut facts, "has_external_event", vec![s(&kind)]);
                }
            }
            for write in &action.writes {
                if seen_external.insert(write.entity_kind.clone()) {
                    push_fact(
                        &mut facts,
                        "has_external_event",
                        vec![s(&write.entity_kind)],
                    );
                }
            }
        }
        if let Some(ActionEffectProgram::FireTransition {
            object_kind,
            lifecycle,
            transition,
            accept_receipt,
            ..
        }) = &action.effect_program
        {
            if accept_receipt.is_some() {
                push_fact(&mut facts, "uses_receipt", vec![s(&action.action_id)]);
            }
            let is_fact = set
                .object_types
                .get(object_kind)
                .and_then(|ty| {
                    effective_object_lifecycles(set, ty)
                        .into_iter()
                        .find(|lc| lc.property == *lifecycle)
                })
                .and_then(|lc| lc.transitions.iter().find(|tr| tr.id == *transition))
                .is_some_and(norn_ir::Transition::is_fact);
            if is_fact {
                push_fact(
                    &mut facts,
                    "fact_transition",
                    vec![s(&action.action_id), s(object_kind), s(transition.as_str())],
                );
            }
        }
    }
    facts
}

pub(super) fn run_external_settlement_check(
    set: &ExtractedSet,
    eval_errors: &mut Vec<String>,
) -> Vec<UnauthenticatedSettlementFinding> {
    let program = Program {
        version: norn_datalog::Version::V1,
        predicates: vec![
            pred("has_external_event", 1, RelationKind::Input),
            pred("fact_transition", 3, RelationKind::Input),
            pred("uses_receipt", 1, RelationKind::Input),
            pred("unauth_settlement", 3, RelationKind::Derived),
        ],
        rules: vec![Rule {
            id: "receipt_bound_external_fact".to_owned(),
            mode: Mode::Validation,
            head: atom("unauth_settlement", &["A", "T", "Tr"]),
            body: vec![
                Literal::Positive(atom("has_external_event", &["T"])),
                Literal::Positive(atom("fact_transition", &["A", "T", "Tr"])),
                Literal::Negative(atom("uses_receipt", &["A"])),
            ],
        }],
    };
    let facts = collect_external_settlement_facts(set);
    let eval = match evaluate(&program, &facts, Budget::default(), ExecBudget::default()) {
        Ok(eval) => eval,
        Err(err) => {
            eval_errors.push(format!("external_settlement: {err:?}"));
            return Vec::new();
        }
    };
    let mut out: Vec<UnauthenticatedSettlementFinding> = eval
        .relations
        .into_iter()
        .find(|rel| rel.predicate == "unauth_settlement")
        .into_iter()
        .flat_map(|rel| rel.tuples)
        .filter_map(|tuple| match tuple.as_slice() {
            [a, t, tr] => Some(UnauthenticatedSettlementFinding {
                action_id: as_str(a).to_owned(),
                object_type: as_str(t).to_owned(),
                transition_id: as_str(tr).to_owned(),
            }),
            _ => None,
        })
        .collect();
    out.sort();
    out
}
