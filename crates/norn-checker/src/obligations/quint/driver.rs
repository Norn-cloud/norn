//! PROOF-QUINT-RUN-1: bounded acceptance with real backends and exact result binding.
use crate::obligations::evidence::{EvidenceIdentity, EvidenceRecord, EvidenceTool};
use crate::obligations::{ApprovedContract, DefinitionSource, quint::Bounds};
use norn_ir::verification::guarantees::{
    GoalGuarantee, GuaranteeLevel, GuaranteeReference, GuaranteeScalar, GuaranteeScenario,
    GuaranteeScenarioKind, GuaranteeState, GuaranteeVerdict, GuaranteesDocument, GuaranteesFormat,
};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    io::{Read as _, Write as _},
    path::Path,
};
type Result<T> = std::result::Result<T, Box<dyn Error>>;

/// Native driver operation; unrecognized command-line values never enter the library.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverMode {
    /// Generate deterministic checker-owned backend jobs.
    Prepare,
    /// Bind independently checked result bytes.
    Bind,
}

/// Native input paths, parsed by the binary rather than the trusted binder.
#[derive(Debug)]
pub struct DriverRequest<'a> {
    /// Requested operation.
    pub mode: DriverMode,
    /// Original declarations.
    pub original: &'a Path,
    /// Mutated declarations.
    pub mutant: &'a Path,
    /// Independent retained base.
    pub base: &'a Path,
    /// Complete bounds profile.
    pub profile: &'a Path,
    /// Generated models and actual result bytes.
    pub work: &'a Path,
}
fn digest(path: &Path) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(std::fs::read(path)?)))
}
fn field<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    value
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing {name}").into())
}
fn scenario(trace: &Value, bounds: &Value, kind: &str) -> Result<GuaranteeScenario> {
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
                values.insert(
                    field(declaration, "property")?.to_owned(),
                    GuaranteeScalar {
                        coefficient,
                        scale: u32::try_from(
                            kind.get("scale")
                                .and_then(Value::as_u64)
                                .ok_or("declared scale")?,
                        )?,
                        unit: serde_json::from_value(
                            kind.get("unit").cloned().unwrap_or(Value::Null),
                        )?,
                    },
                );
            }
        }
        states.push(GuaranteeState {
            exists: present,
            values,
        });
    }
    Ok(GuaranteeScenario {
        kind: if kind == "non_vacuity" {
            GuaranteeScenarioKind::NonVacuityWitness
        } else {
            GuaranteeScenarioKind::Counterexample
        },
        writer_artifact_id: field(bounds, "action")?.into(),
        identity: field(bounds, "identity")?.into(),
        states,
    })
}
// PROOF-GUARANTEE-1: presentation carries actual bound records, never authority.
fn assessment(kind: &str, status: &str, reason: &str) -> (GuaranteeVerdict, GuaranteeLevel) {
    let verdict = match (kind, status) {
        ("non_vacuity", "violation") | ("initialization" | "preservation", "ok") => {
            GuaranteeVerdict::BoundedPass {}
        }
        ("initialization" | "preservation", "violation") => GuaranteeVerdict::Refuted {},
        _ => GuaranteeVerdict::Unknown {
            reason: reason.into(),
        },
    };
    let level = if matches!(verdict, GuaranteeVerdict::Unknown { .. }) {
        GuaranteeLevel::Unknown {
            reason: reason.into(),
        }
    } else {
        GuaranteeLevel::ExhaustiveFiniteBounded {}
    };
    (verdict, level)
}

fn supported_goal(
    index: u32,
    kind: &str,
    record: EvidenceRecord,
    bounds: &Value,
) -> Result<GoalGuarantee> {
    let output: Value = serde_json::from_str(&record.result_json)?;
    let status = field(&output, "status")?;
    let reason = output
        .get("reason")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("backend supplied no qualified verdict");
    let (verdict, level) = assessment(kind, status, reason);
    Ok(GoalGuarantee {
        index,
        goal_kind: kind.into(),
        eligible: true,
        verdict,
        level,
        scenario: output
            .get("trace")
            .filter(|trace| !trace.is_null())
            .map(|t| scenario(t, bounds, kind))
            .transpose()?,
        evidence: Some(record),
    })
}
fn unsupported_goal(index: u32, kind: &str, reason: &str) -> GoalGuarantee {
    GoalGuarantee {
        index,
        goal_kind: kind.into(),
        eligible: false,
        verdict: GuaranteeVerdict::Unknown {
            reason: reason.into(),
        },
        level: GuaranteeLevel::Unknown {
            reason: reason.into(),
        },
        evidence: None,
        scenario: None,
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CaseReport {
    #[serde(rename = "case")]
    label: String,
    challenge: String,
    total: usize,
    eligible: usize,
    discharged: usize,
    acceptance_passed: bool,
    documents: Vec<GuaranteesDocument>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AcceptanceReport {
    cases: Vec<CaseReport>,
    stale_evidence: GuaranteeVerdict,
    stale_rejected: bool,
    acceptance_passed: bool,
}
fn documents(
    source: &DefinitionSource,
    batch: &super::Batch,
    profile: &Value,
    goals: &[GoalGuarantee],
) -> Result<Vec<GuaranteesDocument>> {
    let mut documents = Vec::new();
    for (law_index, identity) in batch
        .challenge
        .body()
        .get("lawStatements")
        .and_then(Value::as_array)
        .ok_or("law statements")?
        .iter()
        .enumerate()
    {
        let artifact_id = field(identity, "artifactId")?;
        let artifact = source
            .artifacts
            .iter()
            .chain(&source.base)
            .find(|a| a.artifact_id == artifact_id)
            .ok_or("governing law artifact")?;
        let law: norn_ir::LawIR =
            serde_json::from_value(serde_json::to_value(artifact.payload.to_json_value())?)?;
        let selected: Vec<_> = goals
            .iter()
            .filter(|g| {
                batch
                    .challenge
                    .body()
                    .get("obligations")
                    .and_then(|v| usize::try_from(g.index).ok().and_then(|index| v.get(index)))
                    .and_then(|v| v.get("law"))
                    .and_then(Value::as_u64)
                    == u64::try_from(law_index).ok()
            })
            .cloned()
            .collect();
        let eligible = selected.iter().filter(|g| g.eligible).count();
        let discharged = selected
            .iter()
            .filter(|g| matches!(g.verdict, GuaranteeVerdict::BoundedPass {}))
            .count();
        let verdict = if selected
            .iter()
            .any(|g| matches!(g.verdict, GuaranteeVerdict::Refuted {}))
        {
            GuaranteeVerdict::Refuted {}
        } else if eligible > 0 && discharged == eligible {
            GuaranteeVerdict::BoundedPass {}
        } else {
            GuaranteeVerdict::Unknown {
                reason: "eligible obligations lack a complete agreeing backend result".into(),
            }
        };
        let level = if matches!(verdict, GuaranteeVerdict::Unknown { .. }) {
            GuaranteeLevel::Unknown {
                reason: "eligible obligations lack qualified evidence".into(),
            }
        } else {
            GuaranteeLevel::ExhaustiveFiniteBounded {}
        };
        let mut tools = Vec::new();
        for record in selected.iter().filter_map(|g| g.evidence.as_ref()) {
            if !tools.contains(&record.pins.tool) {
                tools.push(record.pins.tool.clone());
            }
        }
        documents.push(GuaranteesDocument {
            format: GuaranteesFormat::V1, law_artifact_id: artifact_id.into(), law_id: law.id,
            law_version: artifact.version.clone(), verdict, level,
            challenge_sha256: selected.iter().find_map(|g|g.evidence.as_ref()).map_or_else(||batch.challenge.sha256().to_owned(), |record|record.pins.challenge_sha256.clone()),
            bounds_json: selected.iter().find_map(|g|g.evidence.as_ref()).map(|record|record.pins.bounds_json.clone()).map_or_else(||serde_json::to_string(&json!({"declaredBoundsProfile":profile,"profile":batch.bounds,"exclusions":profile.get("exclusions")})), Ok)?,
            tools, total: u32::try_from(selected.len())?, eligible: u32::try_from(eligible)?, discharged: u32::try_from(discharged)?, goals: selected,
            runtime_enforcement: GuaranteeReference::NotYet { reason: "no separate runtime enforcement evidence supplied by the server".into() },
            parity: GuaranteeReference::NotYet { reason: "matching raw backend verdicts are retained per goal; no separate server parity reference supplied".into() },
        });
    }
    Ok(documents)
}
#[allow(
    clippy::too_many_lines,
    reason = "bounded acceptance driver; all backend and binding steps remain explicit"
)]
/// Prepare generated jobs or bind independently checked results on a native host.
/// # Errors
/// Refuses invalid inputs, stale pins, changed models/results or failed acceptance.
pub fn run(request: &DriverRequest<'_>) -> Result<()> {
    let DriverRequest {
        mode,
        original,
        mutant,
        base,
        profile,
        work,
    } = *request;
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
            Sha256::digest(norn_canonical::serialize_json_verbatim(&implementation)?)
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
                    goals.push(unsupported_goal(u32::try_from(index)?, kind, reason));
                    continue;
                }
            };
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
            if mode == DriverMode::Prepare {
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
            let goal = supported_goal(u32::try_from(index)?, kind, record, &batch.bounds)?;
            if matches!(goal.verdict, GuaranteeVerdict::BoundedPass {}) {
                discharged = discharged.saturating_add(1);
            }
            goals.push(goal);
        }
        if mode == DriverMode::Prepare {
            continue;
        }
        let counterexample = label != "mutant"
            || goals.iter().any(|g| {
                g.goal_kind == "preservation" && matches!(g.verdict, GuaranteeVerdict::Refuted {})
            });
        let expected = if label == "mutant" { 2 } else { 3 };
        cases.push(CaseReport {
            label: label.into(),
            challenge: batch.challenge.sha256().into(),
            total: batch.goals.len(),
            eligible,
            discharged,
            acceptance_passed: eligible == 3
                && batch.goals.len() == 10
                && discharged == expected
                && counterexample,
            documents: documents(source, &batch, &profile, &goals)?,
        });
    }
    if mode == DriverMode::Prepare {
        writeln!(
            std::io::stdout().lock(),
            "{}",
            serde_json::to_string(&json!({"jobs":jobs}))?
        )?;
        return Ok(());
    }
    let restored = cases.first().map(|c| &c.challenge) == cases.last().map(|c| &c.challenge);
    let accepted = restored && stale_rejected && cases.iter().all(|c| c.acceptance_passed);
    if accepted {
        for case in &cases {
            for (index, document) in case.documents.iter().enumerate() {
                std::fs::write(
                    work.join(format!("{}-law-{index}.guarantees.json", case.label)),
                    format!("{}\n", serde_json::to_string_pretty(document)?),
                )?;
            }
        }
    }
    writeln!(
        std::io::stdout().lock(),
        "{}",
        serde_json::to_string(&AcceptanceReport {
            cases,
            stale_evidence: GuaranteeVerdict::Unknown {
                reason: "evidence belongs to the original Challenge and is stale under the mutant"
                    .into()
            },
            stale_rejected,
            acceptance_passed: accepted,
        })?
    )?;
    if !accepted {
        return Err("bounded original/mutant/restored acceptance failed".into());
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "bounded binder fixtures"
)]
mod tests {
    use super::{GuaranteeLevel, GuaranteeVerdict, assessment, scenario};
    use serde_json::json;

    #[test]
    fn counterexample_reconstruction_uses_declared_exact_units_and_empty_state() {
        let bounds = json!({"action":"test.writer","identity":"one","interpretation":{
            "v0":{"property":"mass","type":{"scale":2,"unit":{"system":"ucum","code":"g"}}}
        }});
        let trace = json!([
            {"state":{"present":false,"v0":0}},
            {"state":{"present":true,"v0":125}}
        ]);
        let result = scenario(&trace, &bounds, "preservation").expect("scenario");
        assert!(!result.states[0].exists);
        assert!(result.states[0].values.is_empty());
        assert_eq!(
            serde_json::to_value(&result.states[1].values["mass"]).expect("typed quantity"),
            json!({"coefficient":"125","scale":2,"unit":{"system":"ucum","code":"g"}})
        );
        for bad in [json!(1.25), json!("125"), json!(null)] {
            assert!(
                scenario(
                    &json!([{"state":{"present":true,"v0":bad}}]),
                    &bounds,
                    "preservation"
                )
                .is_err()
            );
        }
    }
    #[test]
    fn missing_witness_and_unsupported_goals_keep_unknown_assurance() {
        for (kind, status) in [
            ("non_vacuity", "ok"),
            ("law_semantics", "ok"),
            ("preservation", "unknown"),
        ] {
            assert_eq!(
                assessment(kind, status, "missing witness"),
                (
                    GuaranteeVerdict::Unknown {
                        reason: "missing witness".into()
                    },
                    GuaranteeLevel::Unknown {
                        reason: "missing witness".into()
                    }
                )
            );
        }
        for (kind, status) in [
            ("non_vacuity", "violation"),
            ("initialization", "ok"),
            ("preservation", "ok"),
        ] {
            assert_eq!(
                assessment(kind, status, "backend parity"),
                (
                    GuaranteeVerdict::BoundedPass {},
                    GuaranteeLevel::ExhaustiveFiniteBounded {}
                )
            );
        }
        assert_eq!(
            assessment("preservation", "violation", "backend parity"),
            (
                GuaranteeVerdict::Refuted {},
                GuaranteeLevel::ExhaustiveFiniteBounded {}
            )
        );
    }
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one retained end-to-end correspondence fixture"
    )]
    fn recorded_guarantees_match_bound_results() {
        use super::{
            ApprovedContract, Bounds, DefinitionSource, GuaranteesDocument, documents,
            supported_goal, unsupported_goal,
        };
        use serde::Deserialize;
        use serde_json::value::RawValue;
        use sha2::{Digest, Sha256};
        use std::collections::BTreeSet;
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Sources<'a> {
            #[serde(borrow)]
            with_loss: &'a RawValue,
            #[serde(borrow)]
            without_loss: &'a RawValue,
        }
        #[derive(Deserialize)]
        struct RawChallenge<'a> {
            #[serde(borrow)]
            obligations: Vec<&'a RawValue>,
        }
        // Match JSON.stringify's whitespace removal for this integer/string corpus,
        // keeping the declared field order and literal spelling intact.
        fn compact(text: &str) -> String {
            let mut out = String::new();
            let (mut quoted, mut escaped) = (false, false);
            for ch in text.chars() {
                if quoted {
                    out.push(ch);
                    if escaped {
                        escaped = false;
                    } else if ch == '\\' {
                        escaped = true;
                    } else if ch == '"' {
                        quoted = false;
                    }
                } else if ch == '"' {
                    quoted = true;
                    out.push(ch);
                } else if !ch.is_ascii_whitespace() {
                    out.push(ch);
                }
            }
            out
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let corpus =
            std::fs::read_to_string(root.join("crates/norn-cell/tests/fixtures/constitution.json"))
                .expect("actual declarations");
        let sources: Sources<'_> = serde_json::from_str(&corpus).expect("source corpus");
        let base = [
            "core.units",
            "core.Process.interface",
            "core.Conversion.interface",
        ]
        .iter()
        .map(|name| {
            compact(
                &std::fs::read_to_string(root.join(format!("registry/core/artifacts/{name}.json")))
                    .expect("base artifact"),
            )
        })
        .collect::<Vec<_>>()
        .join(",");
        let base = format!("[{base}]");
        let original =
            DefinitionSource::parse(&compact(sources.with_loss.get()), &base).expect("original");
        let registry = BTreeSet::new();
        let authority = ApprovedContract::approve(
            &original,
            &registry,
            json!({"expression":"norn-zen/6","lowering":"norn.quint.one-create/1"}),
            json!({"store":"empty","objects":1,"steps":1}),
            json!({"level":"exhaustive-finite/bounded"}),
        )
        .expect("protected contract");
        let profile: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(
                root.join("verification/profiles/constitution-one-create.json"),
            )
            .expect("profile"),
        )
        .expect("bounds");
        let bounds = Bounds {
            action: profile["action"].as_str().expect("action").into(),
            identity: profile["identity"].as_str().expect("identity").into(),
            inputs: serde_json::from_value(profile["inputs"].clone()).expect("intervals"),
            seed: 0,
        };
        for (label, text) in [
            ("original", sources.with_loss.get()),
            ("mutant", sources.without_loss.get()),
        ] {
            let source = DefinitionSource::parse(&compact(text), &base).expect("actual source");
            let batch = authority
                .quint(&source, &registry, &bounds)
                .expect("generated challenge");
            let path = root.join(format!(
                "fixtures/demo/guarantees/constitution.{label}.guarantees.json"
            ));
            let bytes = std::fs::read_to_string(&path).expect("captured real binder document");
            let recorded: GuaranteesDocument =
                serde_json::from_str(&bytes).expect("typed document");
            let mut goals = Vec::new();
            for (index, model) in batch.goals.iter().enumerate() {
                let index32 = u32::try_from(index).expect("index");
                let kind = batch.challenge.body()["obligations"][index]["goal"]["kind"]
                    .as_str()
                    .expect("kind");
                if let Err(reason) = model {
                    goals.push(unsupported_goal(index32, kind, reason));
                    continue;
                }
                let record = recorded.goals[index]
                    .evidence
                    .clone()
                    .expect("actual evidence");
                // These are retained run bytes, not a fresh verdict by this test binary.
                // Feature-specific Serde map order may change a new binary's exact digest.
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(&record.challenge_json)
                        .expect("retained challenge"),
                    *batch.challenge.body()
                );
                assert_eq!(record.pins.challenge_sha256, recorded.challenge_sha256);
                assert_eq!(record.pins.goal_index, index32);
                assert_eq!(
                    record.pins.challenge_sha256,
                    crate::obligations::exact_commitment(
                        b"challenge",
                        &[record.challenge_json.as_bytes()]
                    )
                    .expect("challenge digest")
                );
                let raw: RawChallenge<'_> =
                    serde_json::from_str(&record.challenge_json).expect("raw goals");
                assert_eq!(
                    record.pins.goal_sha256,
                    crate::obligations::exact_commitment(
                        b"evidence-goal",
                        &[raw.obligations[index].get().as_bytes()]
                    )
                    .expect("goal digest")
                );
                assert_eq!(
                    record.result_sha256,
                    format!("{:x}", Sha256::digest(record.result_json.as_bytes()))
                );
                let pinned: serde_json::Value =
                    serde_json::from_str(&record.pins.bounds_json).expect("bound profile");
                assert_eq!(pinned["profile"], batch.bounds);
                assert_eq!(pinned["declaredBoundsProfile"], profile);
                assert_eq!(
                    pinned["modelSha256"],
                    format!(
                        "{:x}",
                        Sha256::digest(model.as_ref().expect("supported").as_bytes())
                    )
                );
                goals.push(
                    supported_goal(index32, kind, record, &batch.bounds)
                        .expect("typed actual result"),
                );
            }
            let rendered =
                documents(&source, &batch, &profile, &goals).expect("per-law presentation");
            assert_eq!(rendered.len(), 1);
            let document = &rendered[0];
            assert_eq!(
                (document.total, document.eligible, document.discharged),
                (10, 3, if label == "original" { 3 } else { 2 })
            );
            assert_eq!(document.goals.iter().filter(|g| !g.eligible).count(), 7);
            if label == "mutant" {
                let trace = document
                    .goals
                    .iter()
                    .find(|g| matches!(g.verdict, GuaranteeVerdict::Refuted {}))
                    .expect("actual refutation")
                    .scenario
                    .as_ref()
                    .expect("counterexample");
                assert!(!trace.states[0].exists);
                assert!(trace.states[0].values.is_empty());
                for (name, expected) in [
                    ("input_fine_g", 2),
                    ("output_fine_g", 1),
                    ("loss_fine_g", 0),
                ] {
                    let value = &trace.states[1].values[name];
                    assert_eq!((value.coefficient, value.scale), (expected, 0));
                    assert_eq!(value.unit.as_ref().expect("unit").code, "g");
                }
            }
            let schema = norn_ir::verification::guarantees::guarantees_document_schema();
            let validator = jsonschema::validator_for(schema.as_value()).expect("schema");
            assert!(validator.is_valid(&serde_json::to_value(document).expect("wire value")));
            let text = format!(
                "{}\n",
                serde_json::to_string_pretty(document).expect("exact binder serialization")
            );
            assert_eq!(
                std::fs::read_to_string(path).expect("recorded bytes"),
                text,
                "regenerate with NORN_BLESS=1 only after a fully passing backend run"
            );
        }
    }
}
