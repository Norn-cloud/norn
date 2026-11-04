//! PROOF-LIFECYCLE-1: append source-bound conditional chart goals; never solver evidence.
use super::ChallengeError;
use crate::EffectiveCatalog;
use serde_json::{Value, json};

pub(super) fn append(
    catalog: &EffectiveCatalog,
    goals: &mut Vec<Value>,
) -> Result<(), ChallengeError> {
    let mut bytes = 0_usize;
    for name in catalog.object_type_names() {
        let object = catalog
            .object_type_named(name)
            .and_then(Result::ok)
            .ok_or(ChallengeError::Closure)?;
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
                    json!({"kind":"lifecycle_terminal_reachability","terminal":terminal}),
                    &statement,
                )?;
            }
            push(
                goals,
                &mut bytes,
                json!({"kind":"lifecycle_no_stuck_non_terminal"}),
                &statement,
            )?;
            for transition in &chart.transitions {
                push(
                    goals,
                    &mut bytes,
                    json!({"kind":"lifecycle_transition_non_vacuity","transition":transition.id}),
                    &statement,
                )?;
            }
        }
    }
    Ok(())
}

fn push(
    goals: &mut Vec<Value>,
    bytes: &mut usize,
    goal: Value,
    statement: &Value,
) -> Result<(), ChallengeError> {
    let value = json!({"goal":goal,"statement":statement,"assessment":"unknown",
        "reason":"conditional finite lifecycle goal requires a qualified bounds profile and checked result"});
    let encoded = norn_canonical::serialize_json_verbatim(&value)
        .map_err(|_error| ChallengeError::Closure)?;
    *bytes = bytes.saturating_add(encoded.len());
    if *bytes > 8_388_608 {
        return Err(ChallengeError::Limit);
    }
    goals.push(value);
    Ok(())
}
