//! PROOF-LIFECYCLE-1: append source-bound conditional chart goals; never solver evidence.
/// Finite, law-filtered transition-only relation; no solver authority.
pub mod finite;

use super::ChallengeError;
use crate::{EffectiveCatalog, EffectiveObjectType};
use norn_ir::{ArtifactEnvelope, ArtifactKind, LawIR, LawRoot};
use serde_json::{Value, json};

pub(super) fn append(
    catalog: &EffectiveCatalog,
    pool: &[ArtifactEnvelope],
    goals: &mut Vec<Value>,
) -> Result<(), ChallengeError> {
    let laws = law_bindings(pool)?;
    let mut bytes = 0_usize;
    for name in catalog.object_type_names() {
        let object = catalog
            .object_type_named(name)
            .and_then(Result::ok)
            .ok_or(ChallengeError::Closure)?;
        if object.lifecycles.is_empty() {
            continue;
        }
        let reason = unsupported_binding(object, &laws).unwrap_or_else(||
            "conditional finite lifecycle goal requires a qualified bounds profile and checked result".into());
        for machine in &object.lifecycles {
            let chart = &machine.lifecycle;
            let count = chart
                .terminal
                .len()
                .saturating_add(chart.transitions.len())
                .saturating_add(1);
            if goals.len().saturating_add(count) > 8_192 {
                return Err(ChallengeError::Limit);
            }
            let source = norn_canonical::serialize_json_verbatim(
                &json!({"states":machine.states,"lifecycle":chart}),
            )
            .map_err(|_error| ChallengeError::Closure)?;
            bytes = bytes.saturating_add(source.len());
            if bytes > 8_388_608 {
                return Err(ChallengeError::Limit);
            }
            let digest = super::exact_commitment(b"effective-lifecycle", &[&source])?;
            let statement = json!({
                "rule":"norn.lifecycle.transition-only/1",
                "objectType":name,"property":chart.property,"lifecycleDigest":digest,
                "maxObjects":1,"maxSteps":machine.states.len(),
                "initial":"one admitted initialized object; all applicable pointwise laws hold",
                "relation":"directly declared transition; guards hold in pre-state; all applicable pointwise laws hold in post-state",
                "frame":"same object; all other properties and external references frozen; no other effects",
                "unsupported":"guards, laws, timing, cross-lifecycle reads or effects outside the qualified fragment yield Unknown"
            });
            for terminal in &chart.terminal {
                push(
                    goals,
                    &mut bytes,
                    &json!({"kind":"lifecycle_terminal_reachability","terminal":terminal}),
                    &statement,
                    &reason,
                )?;
            }
            push(
                goals,
                &mut bytes,
                &json!({"kind":"lifecycle_no_stuck_non_terminal"}),
                &statement,
                &reason,
            )?;
            for transition in &chart.transitions {
                push(
                    goals,
                    &mut bytes,
                    &json!({"kind":"lifecycle_transition_non_vacuity","transition":transition.id}),
                    &statement,
                    &reason,
                )?;
            }
        }
    }
    Ok(())
}

fn push(
    goals: &mut Vec<Value>,
    bytes: &mut usize,
    goal: &Value,
    statement: &Value,
    reason: &str,
) -> Result<(), ChallengeError> {
    let value = json!({"goal":goal,"statement":statement,"assessment":"unknown",
        "reason":reason});
    let encoded = norn_canonical::serialize_json_verbatim(&value)
        .map_err(|_error| ChallengeError::Closure)?;
    *bytes = bytes.saturating_add(encoded.len());
    if *bytes > 8_388_608 {
        return Err(ChallengeError::Limit);
    }
    goals.push(value);
    Ok(())
}

// Match the native governing-law resolver's applicability and B2 scope boundary.
fn unsupported_binding(
    object: &EffectiveObjectType,
    laws: &[(&ArtifactEnvelope, LawIR)],
) -> Option<String> {
    let is_a = |name: &str| {
        object.lineage.iter().any(|ty| ty.as_str() == name)
            || object.interfaces.iter().any(|ty| ty.as_str() == name)
    };
    for (artifact, law) in laws {
        if !is_a(&law.subject) && !law.scope.object_types.iter().any(|ty| is_a(ty)) {
            continue;
        }
        if !is_a(&law.subject)
            || !matches!(&law.scope.root, LawRoot::Entity(name) if name == &law.subject)
            || law.scope.object_types != [law.subject.as_str()]
        {
            return Some(format!(
                "UnsupportedUntilB2: governing law {} ({}) has unsupported root/binding {:?}",
                artifact.artifact_id, law.id, law.scope.root
            ));
        }
    }
    None
}

fn law_bindings(
    pool: &[ArtifactEnvelope],
) -> Result<Vec<(&ArtifactEnvelope, LawIR)>, ChallengeError> {
    pool.iter()
        .filter(|artifact| artifact.kind() == ArtifactKind::Law)
        .map(|artifact| {
            let law = artifact
                .payload
                .to_json_value()
                .decode()
                .map_err(|_error| ChallengeError::Closure)?;
            Ok((artifact, law))
        })
        .collect()
}
