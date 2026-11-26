//! Unconstrained caller-write report for `RED-TEAM-1`.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::{ActionEffectProgram, LawResponse};

use crate::law_honesty::{LawEnforcementStatus, classify_law};

use super::{
    ExtractedSet, UnconstrainedWriteFinding, effective_object_key_props,
    effective_object_lifecycles, effective_object_properties, effective_protected_props,
    type_governed_subjects,
};

fn collect_governed_properties(set: &ExtractedSet) -> BTreeMap<String, BTreeSet<String>> {
    let mut by_subject = BTreeMap::<String, BTreeSet<String>>::new();
    for (type_name, ty) in &set.object_types {
        let entry = by_subject.entry(type_name.clone()).or_default();
        for inv in &ty.invariants {
            if let Ok(reads) = norn_expr::expression_reads(inv.expression.as_str(), "self") {
                entry.extend(reads.fields);
            }
        }
    }
    for (iface_name, iface) in &set.interfaces {
        let entry = by_subject.entry(iface_name.clone()).or_default();
        for inv in &iface.invariants {
            if let Ok(reads) = norn_expr::expression_reads(inv.expression.as_str(), "self") {
                entry.extend(reads.fields);
            }
        }
    }
    for (artifact_id, law) in &set.laws {
        // Only credit laws actually enforced on commit, never declared-only laws.
        if classify_law(artifact_id, law).enforcement != LawEnforcementStatus::Enforced {
            continue;
        }
        let entry = by_subject.entry(law.subject.clone()).or_default();
        if let Some(cond) = &law.condition
            && let Ok(reads) = norn_expr::expression_reads(cond.as_str(), "self")
        {
            entry.extend(reads.fields);
        }
        if let LawResponse::Predicate { expression } = &law.response
            && let Ok(reads) = norn_expr::expression_reads(expression.as_str(), "self")
        {
            entry.extend(reads.fields);
        }
    }
    let mut resolved = BTreeMap::new();
    for (type_name, ty) in &set.object_types {
        let mut combined = BTreeSet::new();
        for subj in type_governed_subjects(set, ty) {
            if let Some(fields) = by_subject.get(&subj) {
                combined.extend(fields.iter().cloned());
            }
        }
        resolved.insert(type_name.clone(), combined);
    }
    resolved
}

pub(super) fn collect_unconstrained_writes(set: &ExtractedSet) -> Vec<UnconstrainedWriteFinding> {
    let governed_props = collect_governed_properties(set);
    let mut out = Vec::new();
    for action in &set.actions {
        let Some(ActionEffectProgram::UpsertInputObject { object_kind, .. }) =
            &action.effect_program
        else {
            continue;
        };
        let Some(ty) = set.object_types.get(object_kind) else {
            continue;
        };
        let Some(props) = action
            .input_schema
            .get("properties")
            .and_then(norn_ir::JsonValue::as_object)
        else {
            continue;
        };
        let derived: BTreeSet<&str> = action.derive.iter().map(|d| d.field.as_str()).collect();
        let mut checked_inputs = BTreeSet::new();
        for pre in &action.preconditions {
            if let Ok(reads) = norn_expr::expression_reads(pre.as_str(), "input") {
                checked_inputs.extend(reads.fields);
            }
        }
        let effective_props = effective_object_properties(set, ty);
        let key_props = effective_object_key_props(set, ty);
        let protected_props = effective_protected_props(set, ty);
        let lc_props: BTreeSet<&str> = effective_object_lifecycles(set, ty)
            .into_iter()
            .map(|lc| lc.property.as_str())
            .collect();
        let link_fields: BTreeSet<&str> = action
            .create_links
            .iter()
            .map(|l| l.target_id_field.as_str())
            .collect();
        let governed = governed_props.get(object_kind);

        for field in props.keys() {
            let Some(prop) = effective_props.iter().find(|p| p.id.as_str() == field) else {
                continue;
            };
            if prop.protected
                || protected_props.contains(field.as_str())
                || prop.computed.is_some()
                || key_props.contains(field.as_str())
                || lc_props.contains(field.as_str())
                || link_fields.contains(field.as_str())
                || derived.contains(field.as_str())
                || checked_inputs.contains(field.as_str())
                || governed.is_some_and(|set| set.contains(field.as_str()))
            {
                continue;
            }
            out.push(UnconstrainedWriteFinding {
                action_id: action.action_id.clone(),
                object_type: object_kind.clone(),
                property: field.clone(),
            });
        }
    }
    out.sort();
    out
}
