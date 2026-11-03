//! PROOF-QUINT-RUN-1: bounded acceptance with real backends and exact result binding.
use norn_checker::obligations::evidence::{EvidenceIdentity, EvidenceRecord, EvidenceTool};
use norn_checker::obligations::{ApprovedContract, DefinitionSource, quint::Bounds};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    io::{Read as _, Write as _},
    path::Path,
};
type Result<T> = std::result::Result<T, Box<dyn Error>>;
fn digest(path: &Path) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(std::fs::read(path)?)))
}
fn field<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    value
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing {name}").into())
}
fn scenario(trace: &Value, bounds: &Value, law: &Value) -> Result<Value> {
    let fields = bounds
        .get("interpretation")
        .and_then(Value::as_object)
        .ok_or("variable mapping")?;
    let mut states = Vec::new();
    for step in trace.as_array().ok_or("trace")? {
        let state = step.get("state").ok_or("state")?;
        let present = state
            .get("present")
            .and_then(Value::as_bool)
            .ok_or("existence flag")?;
        let mut values = BTreeMap::new();
        for (variable, declaration) in fields {
            let coefficient = state
                .get(variable)
                .and_then(Value::as_i64)
                .ok_or("exact trace integer")?;
            let kind = declaration.get("type").ok_or("declared type")?;
            if present {
                values.insert(field(declaration, "property")?, json!({"coefficient":coefficient.to_string(),"scale":kind.get("scale"),"unit":kind.get("unit")}));
            }
        }
        states.push(json!({"exists":present,"values":values}));
    }
    Ok(
        json!({"law":law,"writer":bounds.get("action"),"identity":bounds.get("identity"),"states":states,"interpretation":"writer proposal before runtime law enforcement"}),
    )
}
// PROOF-QUINT-RUN-1: absence of a witness retains Unknown assurance.
fn assessment(kind: &str, status: &str) -> (&'static str, &'static str) {
    let verdict = match (kind, status) {
        ("non_vacuity", "violation") | ("initialization" | "preservation", "ok") => "bounded-pass",
        ("initialization" | "preservation", "violation") => "refuted",
        _ => "unknown",
    };
    (
        verdict,
        if verdict == "unknown" {
            "unknown"
        } else {
            "exhaustive-finite/bounded"
        },
    )
}
#[allow(
    clippy::too_many_lines,
    reason = "bounded acceptance driver; all backend and binding steps remain explicit"
)]
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let [mode, original, mutant, base, profile, work, _tools] = args.as_slice() else {
        return Err(
            "usage: quint prepare|bind ORIGINAL MUTANT BASE BOUNDS OUTPUT_DIR TOOL_ROOT".into(),
        );
    };
    let profile: Value = serde_json::from_str(&std::fs::read_to_string(profile)?)?;
    let bounds = Bounds {
        action: field(&profile, "action")?.into(),
        identity: field(&profile, "identity")?.into(),
        inputs: serde_json::from_value(profile.get("inputs").ok_or("inputs")?.clone())?,
        seed: profile.get("seed").and_then(Value::as_u64).ok_or("seed")?,
    };
    let base = std::fs::read_to_string(base)?;
    let original_text = std::fs::read_to_string(original)?;
    let original = DefinitionSource::parse(&original_text, &base)?;
    let restored = DefinitionSource::parse(&original_text, &base)?;
    let mutant = DefinitionSource::parse(&std::fs::read_to_string(mutant)?, &base)?;
    let registry = BTreeSet::new();
    let authority = ApprovedContract::approve(
        &original,
        &registry,
        json!({"expression":"norn-zen/6","lowering":"norn.quint.one-create/1"}),
        json!({"store":"empty","objects":1,"steps":1}),
        json!({"level":"exhaustive-finite/bounded"}),
    )?;
    std::fs::create_dir_all(work)?;
    if mode != "prepare" && mode != "bind" {
        return Err("invalid driver mode".into());
    }
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let control: Value = serde_json::from_str(&input)?;
    let implementation = json!({"driver":digest(&std::env::current_exe()?)?,"toolchain":control.get("toolchain").ok_or("trusted toolchain")?});
    let mut jobs = Vec::new();
    let tool = EvidenceTool {
        id: "norn.generated-quint".into(),
        version: "quint-0.32.0/tla-ts-0.0.7/apalache-0.56.1".into(),
        implementation_sha256: format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&implementation)?)
        ),
    };
    let mut cases = Vec::new();
    let mut prior: Option<(EvidenceRecord, String)> = None;
    let mut original_models = None;
    let mut stale_rejected = false;
    for (label, source) in [
        ("original", &original),
        ("mutant", &mutant),
        ("restored", &restored),
    ] {
        let batch = authority.quint(source, &registry, &bounds)?;
        if label == "original" {
            original_models = Some(batch.goals.clone());
        }
        if label == "restored" && original_models.as_ref() != Some(&batch.goals) {
            return Err("restoration changed generated models".into());
        }
        let eligible = batch.goals.iter().filter(|g| g.is_ok()).count();
        let mut discharged = 0_usize;
        let mut goals = Vec::new();
        for (index, model) in batch.goals.iter().enumerate() {
            let statement = batch
                .challenge
                .body()
                .get("obligations")
                .and_then(|v| v.get(index))
                .ok_or("goal")?;
            let kind = field(statement.get("goal").ok_or("goal")?, "kind")?;
            let model = match model {
                Ok(model) => model,
                Err(reason) => {
                    goals.push(json!({"goal":index,"status":"unknown","reason":reason}));
                    continue;
                }
            };
            let law = statement
                .get("law")
                .and_then(Value::as_u64)
                .and_then(|index| usize::try_from(index).ok())
                .and_then(|index| batch.challenge.body().get("lawStatements")?.get(index))
                .and_then(|law| law.get("artifactId"))
                .ok_or("source law identity")?;
            let path = Path::new(work).join(format!("{label}-{index}.qnt"));
            let result = Path::new(work).join(format!("{label}-{index}.json"));
            let key = format!("{label}-{index}");
            let invariant = if kind == "non_vacuity" {
                "unreachable"
            } else {
                "law"
            };
            let step = if kind == "initialization" {
                "idle"
            } else {
                "step"
            };
            let model_sha = format!("{:x}", Sha256::digest(model.as_bytes()));
            if mode == "prepare" {
                std::fs::write(&path, model)?;
                jobs.push(json!({"key":key,"model":path,"result":result,"modelSha":model_sha,"challenge":batch.challenge.sha256(),"invariant":invariant,"step":step,"seed":bounds.seed,"expectedStates":profile.get("expectedStates").ok_or("expected states")?}));
                continue;
            }
            let checked = control
                .get("results")
                .and_then(|r| r.get(&key))
                .ok_or("trusted result pins")?;
            if field(checked, "modelSha")? != model_sha
                || field(checked, "challenge")? != batch.challenge.sha256()
                || digest(&path)? != model_sha
            {
                return Err("stale checked model or challenge".into());
            }
            let bytes = std::fs::read_to_string(&result)?;
            let output: Value = serde_json::from_str(&bytes)?;
            let status = field(&output, "status")?;
            let (verdict, level) = assessment(kind, status);
            let passed = verdict == "bounded-pass";
            let evidence_bounds = serde_json::to_string(
                &json!({"declaredBoundsProfile":profile,"profile":batch.bounds,"modelSha256":digest(&path)?,"invariant":invariant,"step":step,"toolchain":implementation,"expectedStates":profile.get("expectedStates"),"workers":1,"exclusions":profile.get("exclusions").ok_or("explicit exclusions required")?}),
            )?;
            let pins = batch.challenge.evidence_pins(
                u32::try_from(index)?,
                tool.clone(),
                &evidence_bounds,
                bounds.seed,
            )?;
            let record = batch.challenge.record_evidence(&pins, &bytes)?;
            let trusted = field(checked, "resultSha")?.to_owned();
            if digest(&result)? != trusted {
                return Err("checked result bytes changed".into());
            }
            if batch.challenge.evidence_identity(&pins, &trusted, &record)
                != EvidenceIdentity::Current
            {
                return Err("fresh evidence identity mismatch".into());
            }
            if label == "mutant"
                && let Some((old, trusted)) = &prior
                && record.pins.goal_index == old.pins.goal_index
            {
                stale_rejected = batch.challenge.evidence_identity(&pins, trusted, old)
                    == EvidenceIdentity::Unknown;
            }
            if prior.is_none() && label == "original" {
                prior = Some((record.clone(), trusted));
            }
            if passed {
                discharged = discharged.saturating_add(1);
            }
            goals.push(json!({"goal":index,"kind":kind,"status":verdict,"level":level,"reason":output.get("reason"),"scenario":output.get("trace").filter(|trace| !trace.is_null()).map(|t|scenario(t,&batch.bounds,law)).transpose()?,"record":record}));
        }
        let counterexample = label != "mutant"
            || goals.iter().any(|goal| {
                goal.get("kind").and_then(Value::as_str) == Some("preservation")
                    && goal.get("status").and_then(Value::as_str) == Some("refuted")
            });
        let expected = if label == "mutant" { 2 } else { 3 };
        cases.push(json!({"case":label,"challenge":batch.challenge.sha256(),"total":batch.goals.len(),"eligible":eligible,"discharged":discharged,"acceptancePassed":eligible==3 && batch.goals.len()==10 && discharged==expected && counterexample,"goals":goals}));
    }
    if mode == "prepare" {
        writeln!(
            std::io::stdout().lock(),
            "{}",
            serde_json::to_string(&json!({"jobs":jobs}))?
        )?;
        return Ok(());
    }
    let restored = cases.first().and_then(|c| c.get("challenge"))
        == cases.last().and_then(|c| c.get("challenge"));
    let accepted = restored
        && stale_rejected
        && cases
            .iter()
            .all(|c| c.get("acceptancePassed").and_then(Value::as_bool) == Some(true));
    writeln!(
        std::io::stdout().lock(),
        "{}",
        serde_json::to_string(
            &json!({"cases":cases,"staleEvidence":"unknown","staleRejected":stale_rejected,"acceptancePassed":accepted})
        )?
    )?;
    if !accepted {
        return Err("bounded original/mutant/restored acceptance failed".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::assessment;
    #[test]
    fn missing_witness_and_unsupported_goals_keep_unknown_assurance() {
        assert_eq!(assessment("non_vacuity", "ok"), ("unknown", "unknown"));
        assert_eq!(assessment("law_semantics", "ok"), ("unknown", "unknown"));
        assert_eq!(
            assessment("preservation", "unknown"),
            ("unknown", "unknown")
        );
        assert_eq!(
            assessment("non_vacuity", "violation"),
            ("bounded-pass", "exhaustive-finite/bounded")
        );
        assert_eq!(
            assessment("preservation", "violation"),
            ("refuted", "exhaustive-finite/bounded")
        );
        assert_eq!(
            assessment("initialization", "ok"),
            ("bounded-pass", "exhaustive-finite/bounded")
        );
    }
}
