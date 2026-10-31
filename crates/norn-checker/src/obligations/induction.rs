//! Small, source-bound induction statements, not a general solver or install gate.
use std::collections::{BTreeMap, BTreeSet};

use norn_expr::expression_reads;
use norn_ir::{ArtifactEnvelope, LawIR, LawModality, LawResponse, PropertyTypeName, Transition};
use serde_json::{Value, json};

use super::ChallengeError;
use crate::{EffectiveCatalog, EffectiveObjectType, verification::LawWriter};

pub(super) struct Induction {
    pub(super) laws: Vec<Value>,
    pub(super) writers: Vec<Value>,
    pub(super) goals: Vec<Value>,
}

// These templates are part of the immutable challenge. Indexed records bind
// their parameters to approved law envelopes and effective transition digests.
pub(super) fn rules() -> Value {
    json!({
        "profile":"norn.induction-statements/2",
        "law_at":{
            "always":"(scope(law,s) and condition(law,s)) implies response(law,s)",
            "never":"(scope(law,s) and condition(law,s)) implies not response(law,s)",
            "absentCondition":"true",
            "otherModalities":"unknown"
        },
        "initialization":{
            "quantifier":"forall post",
            "premise":"admitted_initial(law.subject,post,closure)",
            "conclusion":"law_at(law,post)"
        },
        "preservation":{
            "quantifier":"forall pre,post",
            "premise":"admitted_state(pre,closure) and admitted_state(post,closure) and law_at(law,pre) and writer_relation(writer,pre,post)",
            "conclusion":"law_at(law,post)"
        },
        "transition_only":{
            "pre":"pre[writer.property] in transition.from",
            "guards":"all transition.guards evaluated with self=pre",
            "post":"post[writer.property] = transition.to",
            "frame":"same object identity and concrete type; every other stored property equal; no other effects",
            "binding":"transition is the complete effective Transition at transitionDigest; state domains and initial states are bound by closure"
        },
        "non_vacuity":{
            "quantifier":"exists reachable state",
            "conclusion":"scope(law,state) and condition(law,state)",
            "evidence":"requires an admitted initialization and reachable witness; never universal proof"
        },
        "law_semantics":{"requirement":"qualified interpretation of the complete approved LawIR under semantics and assumptions","assessment":"unknown"},
        "open_boundary":{"requirement":"preservation for the referenced runtime entrance under the same closure","assessment":"unknown"},
        "unknown":{"relation":"uninterpreted; no discharge rule","assessment":"unknown"},
        "authority":"transition_frame discharges only the conditional preservation statement; no whole-law proof or activation authority"
    })
}

pub(super) fn build(
    pool: &[ArtifactEnvelope],
    approved: &BTreeMap<String, String>,
    writers: &BTreeSet<LawWriter>,
    boundaries: &BTreeSet<String>,
    registry: &BTreeSet<PropertyTypeName>,
) -> Result<Induction, ChallengeError> {
    let catalog = EffectiveCatalog::from_artifacts_with_registry(pool, registry);
    let mut result = Induction {
        laws: Vec::new(),
        writers: Vec::new(),
        goals: Vec::new(),
    };
    let mut relation_bytes = 0_usize;
    for writer in writers {
        result
            .writers
            .push(writer_statement(writer, &catalog, &mut relation_bytes)?);
    }
    let by_id: BTreeMap<_, _> = pool.iter().map(|a| (a.artifact_id.as_str(), a)).collect();
    for (law_index, (id, identity)) in approved.iter().enumerate() {
        let artifact = by_id.get(id.as_str()).ok_or(ChallengeError::Closure)?;
        let law: LawIR = serde_json::from_value(
            serde_json::to_value(artifact.payload.to_json_value())
                .map_err(|_error| ChallengeError::Closure)?,
        )
        .map_err(|_error| ChallengeError::Closure)?;
        // Full LawIR (scope/condition/modality/response/timing) is bound once,
        // including all referenced contracts in the whole-set closure.
        result.laws.push(json!({"artifactId":id,"sha256":identity}));
        for kind in ["initialization", "non_vacuity", "law_semantics"] {
            result
                .goals
                .push(json!({"law":law_index,"goal":{"kind":kind},
                "statement":{"rule":kind,"law":law_index},"assessment":"unknown"}));
        }
        let reads = frame_reads(&law, &catalog);
        for (writer_index, writer) in writers.iter().enumerate() {
            let assessment = if transition_frame(&law, reads.as_ref(), writer) {
                "transition_frame"
            } else {
                "unknown"
            };
            result.goals.push(
                json!({"law":law_index,"goal":{"kind":"preservation","writer":writer_index},
                "statement":{"rule":"preservation","law":law_index,"writer":writer_index},
                "assessment":assessment}),
            );
        }
        for (boundary_index, _) in boundaries.iter().enumerate() {
            result.goals.push(json!({"law":law_index,"goal":{"kind":"open_boundary","boundary":boundary_index},
                "statement":{"rule":"open_boundary","law":law_index,"boundary":boundary_index},"assessment":"unknown"}));
        }
    }
    Ok(result)
}

fn effective_transition<'a>(
    writer: &LawWriter,
    catalog: &'a EffectiveCatalog,
) -> Result<Option<&'a Transition>, ChallengeError> {
    let LawWriter::Transition {
        object_type,
        property,
        transition,
    } = writer
    else {
        return Ok(None);
    };
    let object = catalog
        .object_type_named(object_type)
        .and_then(Result::ok)
        .ok_or(ChallengeError::Closure)?;
    let lifecycle = object.lifecycle(property).ok_or(ChallengeError::Closure)?;
    lifecycle
        .lifecycle
        .transitions
        .iter()
        .find(|t| t.id.as_str() == transition)
        .map(Some)
        .ok_or(ChallengeError::Closure)
}

fn writer_statement(
    writer: &LawWriter,
    catalog: &EffectiveCatalog,
    relation_bytes: &mut usize,
) -> Result<Value, ChallengeError> {
    let Some(transition) = effective_transition(writer, catalog)? else {
        return Ok(json!({"identity":writer.to_json(),"relation":{"rule":"unknown"}}));
    };
    // Charge inherited guard/state text before serializing or hashing it. Do
    // not clone large transition payloads once per law or per goal.
    let text_bytes = transition
        .from
        .iter()
        .map(String::len)
        .chain(transition.guards.iter().map(|guard| guard.as_str().len()))
        .fold(
            transition
                .to
                .len()
                .saturating_add(transition.id.as_str().len())
                .saturating_add(transition.description.as_ref().map_or(0, String::len)),
            usize::saturating_add,
        );
    *relation_bytes =
        relation_bytes.saturating_add(text_bytes.saturating_mul(6).saturating_add(512));
    if *relation_bytes > 8_388_608 {
        return Err(ChallengeError::Limit);
    }
    let bytes = norn_canonical::serialize_json_verbatim(transition)
        .map_err(|_error| ChallengeError::Closure)?;
    norn_canonical::parse_json_verbatim(&bytes).map_err(|_error| ChallengeError::Closure)?;
    let digest = super::exact_commitment(b"effective-transition", &[&bytes])?;
    Ok(
        json!({"identity":writer.to_json(),"relation":{"rule":"transition_only",
        "transitionDigest":digest}}),
    )
}

// Only direct stored-field reads of a pointwise predicate law qualify. This
// deliberately excludes computed transitive dependencies, temporal semantics,
// lifecycle scope selectors and behavior/invariant response slots.
fn frame_reads(law: &LawIR, catalog: &EffectiveCatalog) -> Option<BTreeSet<String>> {
    if !matches!(law.modality, LawModality::Always {} | LawModality::Never {})
        || law.timing.is_some()
        || !law.scope.lifecycle_states.is_empty()
        || !law.scope.commitments.is_empty()
        || law.scope.object_types.len() != 1
        || law.scope.object_types.first()? != &law.subject
    {
        return None;
    }
    let LawResponse::Predicate { expression } = &law.response else {
        return None;
    };
    let object = catalog.object_type_named(&law.subject)?.ok()?;
    if object.unsupported().is_some() {
        return None;
    }
    let mut fields = independent_reads(expression.as_str(), object)?;
    if let Some(condition) = &law.condition {
        fields.extend(independent_reads(condition.as_str(), object)?);
    }
    Some(fields)
}

fn independent_reads(expression: &str, object: &EffectiveObjectType) -> Option<BTreeSet<String>> {
    let reads = expression_reads(expression, "self").ok()?;
    // `actor` is the only other operand in the admitted law environment.
    // Object frame equality places no constraint on per-request principals.
    let actor = expression_reads(expression, "actor").ok()?;
    if reads.whole_root
        || actor.whole_root
        || !actor.fields.is_empty()
        || reads.fields.iter().any(|field| {
            object
                .property(field)
                .is_none_or(|property| property.computed || property.unsupported().is_some())
        })
    {
        return None;
    }
    Some(reads.fields)
}

fn transition_frame(law: &LawIR, reads: Option<&BTreeSet<String>>, writer: &LawWriter) -> bool {
    let LawWriter::Transition {
        object_type,
        property,
        ..
    } = writer
    else {
        return false;
    };
    object_type == &law.subject && reads.is_some_and(|fields| !fields.contains(property))
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "private induction statement identity regression"
)]
mod exact_identity_tests {
    use super::*;

    #[test]
    fn effective_writer_statement_commits_exact_guard_bytes() {
        let statement = |guard: &str| {
            let mut values: Vec<Value> =
                serde_json::from_str(include_str!("../../../../fixtures/law-ir/entities.json"))
                    .expect("entities");
            values[0]["payload"]["objectType"]["lifecycles"][0]["transitions"][0]["guards"] =
                json!([guard]);
            let artifacts: Vec<ArtifactEnvelope> = values
                .into_iter()
                .map(|value| serde_json::from_value(value).expect("entity"))
                .collect();
            let registry = BTreeSet::new();
            let catalog = EffectiveCatalog::from_artifacts_with_registry(&artifacts, &registry);
            let writer = LawWriter::Transition {
                object_type: "erpnext.SalesOrder".into(),
                property: "status".into(),
                transition: "shipped".into(),
            };
            let transition = effective_transition(&writer, &catalog)
                .expect("effective")
                .expect("transition");
            let normalized =
                norn_canonical::canonical_json_of(transition).expect("Norn1 transition");
            let statement = writer_statement(&writer, &catalog, &mut 0).expect("statement");
            (normalized, statement)
        };
        let (left_norn1, left) = statement("len('é') == 2");
        let (right_norn1, right) = statement("len('e\u{301}') == 2");
        assert_eq!(left_norn1, right_norn1);
        assert_eq!(left["identity"], right["identity"]);
        assert_eq!(left["relation"]["rule"], "transition_only");
        assert_ne!(
            left["relation"]["transitionDigest"],
            right["relation"]["transitionDigest"]
        );
    }
}
