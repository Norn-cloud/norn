//! MVP-CONSTITUTION / T4-C2: finite runtime correspondence, not an inductive proof.
use std::{collections::BTreeMap, collections::BTreeSet, error::Error, io::Write as _};

use norn_cell::{CellStore, Model, SqliteCell};
use norn_checker::obligations::evidence::{EvidenceIdentity, EvidenceRecord, EvidenceTool};
use norn_checker::obligations::{ApprovedContract, Challenge, DefinitionSource};
use norn_ir::{
    ArtifactEnvelope, ArtifactPayload, EntityRootV2, InterfaceDef, ObjectTypeDef, Property,
    PropertyConstraints, PropertyId, PropertyType, RuntimeValue,
};
use norn_kernel::cell_host::JsonObject;
use norn_kernel::{
    ACTION_CALL_TYPE, ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial,
    ActionOutcome, CellHost, DecisionTurn, FactEvent, FactKey, LogicalTime, TypeCatalog, World,
    commit_action,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
type Cell = SqliteCell<FactEvent, JsonObject, JsonObject>;
const ACTION: &str = "constitution.melt.convert.action";
const MELT: &str = "constitution.Melt";

struct Allow;
impl ActionAuthorizer for Allow {
    fn authorize(&self, _: &ActionAuthzRequest<'_>) -> std::result::Result<(), ActionDenial> {
        Ok(())
    }
}

fn base() -> Result<Vec<ArtifactEnvelope>> {
    [
        include_str!("../../../registry/core/artifacts/core.units.json"),
        include_str!("../../../registry/core/artifacts/core.Process.interface.json"),
        include_str!("../../../registry/core/artifacts/core.Conversion.interface.json"),
    ]
    .into_iter()
    .map(|s| serde_json::from_str(s).map_err(Into::into))
    .collect()
}

// The host's existing action idempotency record is infrastructure, not a business writer.
fn audit_type(mut object: ObjectTypeDef) -> ObjectTypeDef {
    object.name = norn_ir::ObjectTypeName(ACTION_CALL_TYPE.into());
    object.identity.clear();
    object.implements.clear();
    object.properties = [
        "actionId",
        "actionArtifactId",
        "actionVersion",
        "effectKind",
        "object",
        "input",
        "effectiveInput",
        "derivedValues",
        "preconditionOutcomes",
        "actor",
        "turn",
        "transition",
        "from",
        "to",
        "rowOperations",
    ]
    .into_iter()
    .map(|id| Property {
        id: PropertyId(id.into()),
        description: None,
        property_type: PropertyType::JsonExt { schema: None },
        constraints: PropertyConstraints {
            nullable: true,
            ..PropertyConstraints::default()
        },
        default: None,
        markings: Vec::new(),
        computed: None,
    })
    .collect();
    object
}

fn authored(artifacts: &[ArtifactEnvelope]) -> Result<(ObjectTypeDef, norn_ir::ActionRoot)> {
    let mut object = None;
    let mut action = None;
    for a in artifacts {
        if let ArtifactPayload::Entity(p) = &a.payload {
            object = Some(p.to_json_value().decode::<EntityRootV2>()?.object_type);
        }
        if let ArtifactPayload::Action(p) = &a.payload {
            action = Some(p.to_json_value().decode::<norn_ir::ActionRoot>()?);
        }
    }
    Ok((
        object.ok_or("missing authored conversion")?,
        action.ok_or("missing authored writer")?,
    ))
}

fn field<'a>(value: &'a Value, key: &str) -> Result<&'a Value> {
    value
        .get(key)
        .ok_or_else(|| format!("missing result field {key}").into())
}

fn interfaces(base: &[ArtifactEnvelope]) -> Result<Vec<InterfaceDef>> {
    base.iter()
        .filter_map(|a| {
            let value = serde_json::to_value(a.payload.to_json_value()).ok()?;
            value.get("interface").cloned()
        })
        .map(serde_json::from_value)
        .collect::<std::result::Result<_, _>>()
        .map_err(Into::into)
}

fn exercise(artifacts: &[ArtifactEnvelope], base: &[ArtifactEnvelope]) -> Result<Value> {
    let (object, action) = authored(artifacts)?;
    let interfaces = interfaces(base)?;
    let definitions = [object.clone(), audit_type(object)];
    let model = Model::from_definitions_with_interfaces(&definitions, &[], &interfaces)?
        .with_governing_laws(artifacts)?;
    let catalog = TypeCatalog::new(definitions.iter().take(1))
        .with_interfaces(&interfaces)
        .with_governing_laws(artifacts);
    let mut checked = 0_u32;
    let mut committed = 0_u32;
    let mut nonzero_loss = 0_u32;
    let mut counterexamples = Vec::new();
    for input in 2_u32..=12 {
        for output in 1..=input {
            let required_loss = input
                .checked_sub(output)
                .ok_or("invalid bounded input/output pair")?;
            let mut cell = Cell::new(
                CellStore::open_in_memory(model.clone())?,
                "org_demo/dev",
                |c| c.payload.clone(),
            )?;
            let before = cell.store().read().dump()?;
            let request = ActionCall {
                action: &action,
                action_artifact_id: ACTION,
                action_version: "1.0.0",
                scope: BTreeMap::from([
                    ("orgId".into(), "org_demo".into()),
                    ("environmentId".into(), "dev".into()),
                ]),
                actor: "driver:operator".into(),
                input: serde_json::from_value(
                    json!({"id":"melt", "input_fine_g":input,"output_fine_g":output}),
                )?,
                idempotency_key: "convert".into(),
                object_type: None,
                turn: DecisionTurn {
                    id: "constitution".into(),
                    now: LogicalTime::from_millis(1),
                    world: World::Main,
                    consumes: None,
                },
            };
            checked = checked.checked_add(1).ok_or("case count overflow")?;
            match commit_action(&mut cell, &catalog, &request, &Allow) {
                Ok(ActionOutcome::Committed { .. }) => {
                    let values = cell
                        .load(&FactKey::new(MELT, "melt"))?
                        .values
                        .ok_or("missing committed conversion")?;
                    let expected = RuntimeValue::from_json_text(required_loss.to_string())?;
                    if values.get("loss_fine_g") != Some(&expected) {
                        return Err(
                            "committed mass does not satisfy the independent integer oracle".into(),
                        );
                    }
                    committed = committed.checked_add(1).ok_or("commit count overflow")?;
                    if input > output {
                        nonzero_loss = nonzero_loss.checked_add(1).ok_or("loss count overflow")?;
                    }
                }
                Ok(ActionOutcome::Replayed { .. }) => {
                    return Err("empty initial state unexpectedly replayed".into());
                }
                Err(error) => {
                    if error.code() != "core.laws.violated" {
                        return Err(format!("unexpected refusal: {error:?}").into());
                    }
                    if before != cell.store().read().dump()? {
                        return Err("refusal changed the complete SQLite state".into());
                    }
                    counterexamples.push(json!({"inputFineG":input,"outputFineG":output,"requiredLossFineG":required_loss,
                        "code":error.code(),"explanation":error.to_string(),"atomicUnchanged":true}));
                }
            }
        }
    }
    if checked != 77 {
        return Err("finite domain was not completely executed".into());
    }
    let status = if counterexamples.is_empty() {
        if committed != 77 || nonzero_loss != 66 {
            return Err("bounded pass lacks positive-loss reachable states".into());
        }
        "bounded_pass"
    } else {
        "counterexample"
    };
    Ok(
        json!({"status":status,"checkedCases":checked,"committedCases":committed,"positiveLossCases":nonzero_loss,
        "counterexampleCount":counterexamples.len(),"counterexample":counterexamples.first(),
        "counterexampleTo":"writer proposal; runtime commit safety remains enforced",
        "nonVacuity":if committed > 0 {json!({"status":"witnessed","inputFineG":2,
            "outputFineG":if nonzero_loss>0{1}else{2},"lossFineG":u32::from(nonzero_loss>0),
            "positiveLossWitness":nonzero_loss>0})}else{json!({"status":"not_witnessed"})}}),
    )
}

fn bind(
    challenge: &Challenge,
    result: &Value,
    tool: &EvidenceTool,
    bounds: &str,
) -> Result<(Vec<EvidenceRecord>, Vec<String>)> {
    let goals = field(challenge.body(), "obligations")?
        .as_array()
        .ok_or("missing generated goals")?;
    let mut records = Vec::new();
    let mut checked_digests = Vec::new();
    for (index, goal) in goals.iter().enumerate() {
        let kind = field(field(goal, "goal")?, "kind")?
            .as_str()
            .ok_or("missing generated goal kind")?;
        if !matches!(kind, "initialization" | "preservation" | "non_vacuity") {
            continue;
        }
        let pins = challenge.evidence_pins(u32::try_from(index)?, tool.clone(), bounds, 0)?;
        let mut payload = result
            .as_object()
            .ok_or("runtime result is not an object")?
            .clone();
        payload.insert("evidenceLevel".into(), json!("bounded-runtime-conformance"));
        payload.insert("goalDischarged".into(), json!(false));
        payload.insert("proofStatus".into(), json!("unknown"));
        payload.insert(
            "generatedBackend".into(),
            json!("unknown: action relations are not lowered to executable solver jobs"),
        );
        if kind == "non_vacuity" {
            payload.insert(
                "status".into(),
                field(field(result, "nonVacuity")?, "status")?.clone(),
            );
        }
        let checked_payload = serde_json::to_string(&payload)?;
        let checked_digest = format!("{:x}", Sha256::digest(checked_payload.as_bytes()));
        let record = challenge.record_evidence(&pins, &checked_payload)?;
        if challenge.evidence_identity(&pins, &checked_digest, &record) != EvidenceIdentity::Current
        {
            return Err("fresh result identity refused".into());
        }
        records.push(record);
        checked_digests.push(checked_digest);
    }
    if records.len() != 3 {
        return Err("unexpected generated goal inventory; driver scope must be reviewed".into());
    }
    Ok((records, checked_digests))
}

fn main() -> Result<()> {
    let fixture: Value = serde_json::from_str(include_str!("../tests/fixtures/constitution.json"))?;
    let with_loss: Vec<ArtifactEnvelope> =
        serde_json::from_value(field(&fixture, "withLoss")?.clone())?;
    let without_loss: Vec<ArtifactEnvelope> =
        serde_json::from_value(field(&fixture, "withoutLoss")?.clone())?;
    let base = base()?;
    let source = |a: &[ArtifactEnvelope]| -> Result<DefinitionSource> {
        Ok(DefinitionSource::parse(
            &serde_json::to_string(a)?,
            &serde_json::to_string(&base)?,
        )?)
    };
    let original: DefinitionSource = source(&with_loss)?;
    let mutated: DefinitionSource = source(&without_loss)?;
    let authority = ApprovedContract::approve(
        &original,
        &BTreeSet::new(),
        json!({"runtime":"native-action-commit","numeric":"exact-decimal","units":"pinned-core.units"}),
        json!({"initialState":"empty SQLite cell","turns":1,"scope":"org_demo/dev","universalClaim":false}),
        json!({"required":"bounded runtime correspondence; solver and induction unknown"}),
    )?;
    let good = authority.challenge(&original, &BTreeSet::new())?;
    let bad = authority.challenge(&mutated, &BTreeSet::new())?;
    if good.sha256() == bad.sha256() {
        return Err("writer mutation did not invalidate the generated challenge".into());
    }
    let tool = EvidenceTool {
        id: "norn.constitution.runtime-driver".into(),
        version: "1".into(),
        implementation_sha256: format!(
            "{:x}",
            Sha256::digest(std::fs::read(std::env::current_exe()?)?)
        ),
    };
    let bounds = serde_json::to_string(
        &json!({"inputFineG":[2,12],"outputFineG":"1..inputFineG","unit":"g","scale":0,"cases":77,"turns":1,"initialState":"empty","seed":0,
        "ids":["melt"],"lossInput":"absent; writer derives it; stored default is zero",
        "actor":"driver:operator","scope":"org_demo/dev","authorization":"pre-authorized fixture; permission policy not assessed"}),
    )?;
    let original_result = exercise(&with_loss, &base)?;
    let mutated_result = exercise(&without_loss, &base)?;
    let restored_result = exercise(&with_loss, &base)?;
    let restored = authority.challenge(&source(&with_loss)?, &BTreeSet::new())?;
    if good.sha256() != restored.sha256() {
        return Err("restored writer did not reproduce the generated challenge".into());
    }
    if field(&original_result, "status")? != "bounded_pass"
        || field(&mutated_result, "counterexampleCount")?.as_u64() != Some(66)
        || original_result != restored_result
    {
        return Err("positive/mutation/restoration correspondence failed".into());
    }
    let (original_records, original_digests) = bind(&good, &original_result, &tool, &bounds)?;
    let (bad_records, _) = bind(&bad, &mutated_result, &tool, &bounds)?;
    let (restored_records, _) = bind(&restored, &restored_result, &tool, &bounds)?;
    let prior = original_records.first().ok_or("no bound result")?;
    let changed_pins = bad.evidence_pins(prior.pins.goal_index, tool, &bounds, 0)?;
    let prior_digest = original_digests.first().ok_or("no checked result digest")?;
    if bad.evidence_identity(&changed_pins, prior_digest, prior) != EvidenceIdentity::Unknown {
        return Err("stale pass accepted for changed writer".into());
    }
    writeln!(
        std::io::stdout().lock(),
        "{}",
        serde_json::to_string(
            &json!({"scenario":"MVP-CONSTITUTION/T4-C2","complete":false,"runtimeProfileStatus":"passed","generatedProofStatus":"unknown",
        "assurance":"bounded runtime only; generated solver/inductive proof unknown",
        "withLoss":original_records,"lossRemoved":bad_records,"restored":restored_records,"staleOriginalForMutation":"PROOF-STALE/unknown",
        "openProofGoals":field(good.body(), "obligations")?})
        )?
    )?;
    Ok(())
}

#[test]
fn constitution_loss_mutation_refusal_and_restoration() -> Result<()> {
    main()
}
