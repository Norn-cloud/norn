//! PROOF-TRANSITION-1: checker-owned, backend-neutral finite declaration relation.
use super::{ApprovedContract, Challenge, ChallengeError, DefinitionSource};
use norn_ir::verification::finite::{CreateRelation, FiniteInput, ScalarState};
use norn_ir::{ActionRoot, ArtifactKind, LawIR, ZenExpression};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// One-create, empty-store bounds. Missing fields are explicitly absent inputs.
#[derive(Debug, Clone)]
pub struct Bounds {
    /// Selected admitted action artifact.
    pub action: String,
    /// Inclusive decimal-text input intervals, keyed by declared property ID.
    pub inputs: BTreeMap<String, (String, String)>,
    /// Singleton identity; arbitrary identity predicates are unsupported.
    pub identity: String,
    /// Reproducibility pin, never a sampling limit.
    pub seed: u64,
}
/// Complete generated inventory, including unsupported goals.
#[derive(Debug)]
pub struct Batch {
    /// Fresh checker-owned challenge.
    pub challenge: Challenge,
    /// Exact bounds and declaration interpretation for evidence binding.
    pub bounds: Value,
    /// One result per generated obligation, in challenge order.
    pub goals: Vec<Result<CreateRelation, String>>,
}
impl ApprovedContract {
    /// PROOF-BIND-1: reconstruct admission and challenge before lowering declarations.
    /// Each supported goal returns a closed finite relation; errors remain visible unknowns.
    /// # Errors
    /// Returns the ordinary admission/challenge error, never a solver verdict.
    pub fn finite(
        &self,
        source: &DefinitionSource,
        registry: &BTreeSet<norn_ir::PropertyTypeName>,
        bounds: &Bounds,
    ) -> Result<Batch, ChallengeError> {
        let challenge = self.challenge(source, registry)?;
        let models: BTreeMap<_, _> = at(challenge.body(), "lawStatements")
            .as_array()
            .ok_or(ChallengeError::Closure)?
            .iter()
            .filter_map(|law| at(law, "artifactId").as_str())
            .map(|id| (id.to_owned(), lower(source, registry, bounds, id)))
            .collect();
        let goals = at(challenge.body(), "obligations")
            .as_array()
            .ok_or(ChallengeError::Closure)?
            .iter()
            .map(|goal| {
                let kind = at(at(goal, "goal"), "kind").as_str().unwrap_or("");
                let law = at(goal, "law")
                    .as_u64()
                    .and_then(|i| usize::try_from(i).ok())
                    .and_then(|i| at(challenge.body(), "lawStatements").get(i));
                let writer = at(at(goal, "goal"), "writer")
                    .as_u64()
                    .and_then(|i| usize::try_from(i).ok())
                    .and_then(|i| at(challenge.body(), "writerStatements").get(i));
                let model = law
                    .and_then(|law| at(law, "artifactId").as_str())
                    .and_then(|id| models.get(id));
                match model {
                    Some(Ok(model))
                        if law.is_some_and(|l| {
                            at(l, "artifactId").as_str() == Some(model.law_artifact.as_str())
                        }) && (kind == "initialization"
                            || kind == "non_vacuity"
                            || (kind == "preservation"
                                && writer.is_some_and(|w| {
                                    at(at(w, "identity"), "artifactId").as_str()
                                        == Some(bounds.action.as_str())
                                }))) =>
                    {
                        Ok(model.clone())
                    }
                    Some(Ok(_)) => Err(format!(
                        "unsupported goal {kind}: outside selected one-create pointwise relation"
                    )),
                    Some(Err(reason)) => Err(reason.clone()),
                    None => Err("generated goal has no governing law identity".into()),
                }
            })
            .collect();
        let interpretation = models
            .values()
            .find_map(|model| model.as_ref().ok())
            .map_or(Value::Null, |model| {
                let types: BTreeMap<_, _> = model
                    .state
                    .iter()
                    .map(|field| {
                        (
                            &field.symbol,
                            json!({"property":field.property,"type":field.property_type}),
                        )
                    })
                    .collect();
                json!(types)
            });
        Ok(Batch {
            challenge,
            bounds: json!({"profile":"norn.quint.one-create/1","action":bounds.action,"inputs":bounds.inputs,"identity":bounds.identity,"seed":bounds.seed,"initialStore":"empty","maxObjects":1,"maxSteps":1,"authorization":"pre-authorized; permission policy outside profile","absentInputs":"all non-identity fields not in inputs","interpretation":interpretation}),
            goals,
        })
    }
}
fn at<'a>(value: &'a Value, key: &str) -> &'a Value {
    value.get(key).unwrap_or(&Value::Null)
}
fn text(value: &Value) -> Result<&str, String> {
    value
        .as_str()
        .ok_or_else(|| "unsupported or absent declaration".into())
}
fn demand(ok: bool, reason: &str) -> Result<(), String> {
    if ok { Ok(()) } else { Err(reason.into()) }
}
fn scaled(text: &str, scale: u32) -> Result<i64, String> {
    demand(
        scale <= 6 && text.len() <= 64,
        "decimal exceeds exact bounded profile",
    )?;
    norn_ir::RuntimeValue::from_json_text(text).map_err(|_error| "invalid exact decimal")?;
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    let fraction = fraction.trim_end_matches('0');
    let padding = scale
        .checked_sub(u32::try_from(fraction.len()).map_err(|_error| "decimal scale")?)
        .ok_or("inexact decimal literal")?;
    let zeros = "0".repeat(usize::try_from(padding).map_err(|_error| "decimal scale")?);
    let result = format!("{whole}{fraction}{zeros}")
        .parse::<i64>()
        .map_err(|_error| "bounded decimal overflow")?;
    demand(
        result.unsigned_abs() <= 1_000_000,
        "decimal exceeds bounded arithmetic range",
    )?;
    Ok(result)
}
// The admitted common grammar has identical precedence in ZEN and Quint. Lex every
// byte; never substitute substrings or pass unrecognized source through to Quint.
pub(super) fn expression(
    source: &str,
    fields: &BTreeMap<String, String>,
    scale: u32,
) -> Result<String, String> {
    demand(source.len() <= 4096, "expression exceeds lowering budget")?;
    let lexer = regex_lite::Regex::new(r"^\s*(self\.[A-Za-z_][A-Za-z_0-9]*|input\.[A-Za-z_][A-Za-z_0-9]*|[0-9]+(?:\.[0-9]+)?|==|!=|<=|>=|[()+<>-]|and\b|or\b|not\b|true\b|false\b)").map_err(|_error| "lexer")?;
    let mut rest = source;
    let mut tokens = Vec::new();
    while !rest.trim().is_empty() {
        let token = lexer
            .captures(rest)
            .ok_or("unsupported expression syntax")?;
        let full = token.get(0).ok_or("token")?;
        let word = token.get(1).ok_or("token")?.as_str();
        let value = if let Some(field) = word
            .strip_prefix("self.")
            .or_else(|| word.strip_prefix("input."))
        {
            fields.get(field).cloned().ok_or("unsupported field read")?
        } else if word.as_bytes().first().is_some_and(u8::is_ascii_digit) {
            scaled(word, scale)?.to_string()
        } else {
            word.to_owned()
        };
        demand(
            value.len() <= 4096,
            "expanded expression exceeds exact arithmetic budget",
        )?;
        tokens.push(value);
        demand(tokens.len() <= 256, "expression arithmetic budget")?;
        rest = rest.get(full.end()..).ok_or("token boundary")?;
    }
    let result = tokens.join(" ");
    demand(
        result.len() <= 4096,
        "expanded expression exceeds exact arithmetic budget",
    )?;
    Ok(result)
}
#[allow(
    clippy::too_many_lines,
    reason = "one closed lowering profile with explicit refusal at every unsupported declaration"
)]
fn lower(
    source: &DefinitionSource,
    registry: &BTreeSet<norn_ir::PropertyTypeName>,
    bounds: &Bounds,
    law_id: &str,
) -> Result<CreateRelation, String> {
    let pool: Vec<_> = source
        .base
        .iter()
        .chain(&source.artifacts)
        .cloned()
        .collect();
    let selected = pool
        .iter()
        .find(|a| a.artifact_id == bounds.action && a.kind() == ArtifactKind::Action)
        .ok_or("selected action absent")?;
    let action_json =
        serde_json::to_value(selected.payload.to_json_value()).map_err(|_error| "action")?;
    let action: ActionRoot =
        serde_json::from_value(action_json.clone()).map_err(|_error| "action")?;
    demand(
        action.slice.is_empty()
            && action.commands.is_empty()
            && action.synchronous_rule.is_none()
            && action.quantity_effect.is_none()
            && action.create_links.is_empty()
            && action.extensions.is_none()
            && action.access.is_none(),
        "unsupported transitive action effects or reads",
    )?;
    let effect = at(&action_json, "effectProgram");
    demand(
        at(effect, "kind") == "upsert_input_object"
            && at(at(effect, "writeMode"), "kind") == "create_only",
        "only create-only input writers supported",
    )?;
    let subject = text(at(effect, "objectKind"))?;
    let identity = text(at(effect, "objectIdField"))?;
    demand(
        !bounds.identity.is_empty() && bounds.identity.len() <= 128 && bounds.inputs.len() <= 16,
        "bounds exceed profile",
    )?;
    let catalog = crate::EffectiveCatalog::from_artifacts_with_registry(&pool, registry);
    let object = catalog
        .object_type_named(subject)
        .and_then(Result::ok)
        .ok_or("effective object missing")?;
    demand(
        object.properties.len() <= 16
            && !object.has_links
            && !object.has_invariants
            && object.keys.iter().all(|key| key.match_policy.is_none())
            && object.lifecycles.is_empty()
            && object.owned_collections.is_empty(),
        "unsupported effective object contract",
    )?;
    let mut fields = BTreeMap::new();
    let mut assignments = BTreeMap::new();
    let mut choices = Vec::new();
    let mut state = Vec::new();
    let mut numeric = None;
    let schema = at(at(&action_json, "inputSchema"), "properties");
    demand(
        schema
            .as_object()
            .is_some_and(|p| p.len() == object.properties.len()),
        "input/stored property mismatch",
    )?;
    for (index, property) in object.properties.iter().enumerate() {
        let id = property.id.as_str();
        demand(
            !property.computed
                && !property.nullable
                && property
                    .constraints
                    .iter()
                    .all(norn_ir::PropertyConstraints::is_empty),
            "unsupported property constraints",
        )?;
        let crate::EffectiveKind::Base(kind) = &property.kind else {
            return Err("unresolved property type".into());
        };
        let declared = serde_json::to_value(kind).map_err(|_error| "property")?;
        if id == identity {
            demand(
                at(&declared, "kind") == "text"
                    && !bounds.inputs.contains_key(id)
                    && at(schema, id) == &json!({"type":"string"}),
                "identity must be singleton text",
            )?;
            object
                .check_stored_property_value(
                    id,
                    &norn_ir::RuntimeValue::String(bounds.identity.clone()),
                )
                .map_err(|_error| "identity outside declaration")?;
            continue;
        }
        demand(
            at(&declared, "kind") == "decimal"
                && declared.get("currencySource").is_none()
                && declared.get("perUnit").is_none()
                && declared.get("rounding").is_none(),
            "only fixed-unit decimals supported",
        )?;
        demand(
            serde_json::from_value::<norn_ir::PropertyType>(at(at(schema, id), "nornType").clone())
                .ok()
                .as_ref()
                == Some(kind)
                && at(schema, id)
                    .as_object()
                    .is_some_and(|s| s.keys().all(|k| k == "type" || k == "nornType")),
            "input decimal contract differs",
        )?;
        let scale = at(&declared, "scale")
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .ok_or("scale")?;
        let interpretation = json!({"scale":scale,"unit":at(&declared, "unit")});
        demand(
            numeric.as_ref().is_none_or(|n| n == &interpretation),
            "mixed scales or units unsupported",
        )?;
        numeric = Some(interpretation);
        let variable = format!("v{index}");
        state.push(ScalarState {
            symbol: variable.clone(),
            property: property.id.clone(),
            property_type: kind.clone(),
        });
        if let Some((lo, hi)) = bounds.inputs.get(id) {
            let (lo, hi) = (scaled(lo, scale)?, scaled(hi, scale)?);
            demand(
                lo <= hi && hi.checked_sub(lo).is_some_and(|width| width <= 1024),
                "invalid finite interval",
            )?;
            choices.push(FiniteInput {
                symbol: format!("i{index}"),
                property: property.id.clone(),
                lower: lo,
                upper: hi,
            });
            fields.insert(id.to_owned(), format!("i{index}"));
        }
        assignments.insert(id.to_owned(), variable);
    }
    demand(
        bounds.inputs.keys().all(|k| assignments.contains_key(k)),
        "unknown bounds field",
    )?;
    let scale = numeric
        .as_ref()
        .and_then(|n| at(n, "scale").as_u64())
        .and_then(|n| u32::try_from(n).ok())
        .ok_or("no numeric properties")?;
    for derivation in &action.derive {
        demand(
            !derivation.input_required || fields.contains_key(&derivation.field),
            "required derivation input absent",
        )?;
        demand(
            assignments.contains_key(&derivation.field),
            "unsupported derivation target",
        )?;
        let value = expression(derivation.expression.as_str(), &fields, scale)?;
        fields.insert(derivation.field.clone(), format!("({value})"));
    }
    for required in at(at(&action_json, "inputSchema"), "required")
        .as_array()
        .ok_or("required input")?
    {
        let id = text(required)?;
        demand(
            id == identity || fields.contains_key(id),
            "required action input absent",
        )?;
    }
    let mut guards = Vec::new();
    for guard in &action.preconditions {
        guards.push(expression(guard.as_str(), &fields, scale)?);
    }
    for property in &object.properties {
        let id = property.id.as_str();
        if id != identity && !fields.contains_key(id) {
            let default = serde_json::to_value(&property.default).map_err(|_error| "default")?;
            fields.insert(id.to_owned(), scaled(text(&default)?, scale)?.to_string());
        }
    }
    let artifact = pool
        .iter()
        .find(|artifact| artifact.kind() == ArtifactKind::Law && artifact.artifact_id == law_id)
        .ok_or("governing law artifact absent")?;
    let law: LawIR = serde_json::from_value(
        serde_json::to_value(artifact.payload.to_json_value()).map_err(|_error| "law payload")?,
    )
    .map_err(|_error| "law payload")?;
    demand(
        law.subject == subject,
        "law outside selected writer subject",
    )?;
    demand(
        law.timing.is_none()
            && law.scope.object_types == [subject]
            && law.scope.lifecycle_states.is_empty()
            && law.scope.commitments.is_empty()
            && matches!(&law.scope.root, norn_ir::LawRoot::Entity(s) if s == subject),
        "unsupported law scope or timing",
    )?;
    let norn_ir::LawResponse::Predicate {
        expression: predicate,
    } = &law.response
    else {
        return Err("unsupported law response".into());
    };
    let response = expression(predicate.as_str(), &assignments, scale)?;
    let response = match law.modality {
        norn_ir::LawModality::Always {} => response,
        norn_ir::LawModality::Never {} => format!("not({response})"),
        norn_ir::LawModality::Eventually {} | norn_ir::LawModality::Until { .. } => {
            return Err("unsupported temporal law".into());
        }
    };
    let condition = law
        .condition
        .as_ref()
        .map(|c| expression(c.as_str(), &assignments, scale))
        .transpose()?
        .unwrap_or_else(|| "true".into());
    let updates = assignments
        .iter()
        .map(|(id, variable)| {
            fields
                .get(id)
                .map(|value| (variable.clone(), ZenExpression(value.clone())))
                .ok_or("missing writer field")
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(CreateRelation {
        law_artifact: artifact.artifact_id.clone(),
        writer_artifact: bounds.action.clone(),
        object_type: object.name.clone(),
        identity: bounds.identity.clone(),
        state,
        inputs: choices,
        updates,
        guards: guards.into_iter().map(ZenExpression).collect(),
        condition: ZenExpression(condition),
        response: ZenExpression(response),
    })
}
