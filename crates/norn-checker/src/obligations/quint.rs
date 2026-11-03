//! Bounded declaration lowering. Solver status and activation authority live elsewhere.
use super::{ApprovedContract, Challenge, ChallengeError, DefinitionSource};
use norn_ir::{ActionRoot, ArtifactKind, LawIR};
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
    pub goals: Vec<Result<String, String>>,
}
impl ApprovedContract {
    /// PROOF-BIND-1: reconstruct admission and challenge before lowering declarations.
    /// Each supported goal returns a deterministic module; errors remain visible unknowns.
    /// # Errors
    /// Returns the ordinary admission/challenge error, never a solver verdict.
    pub fn quint(
        &self,
        source: &DefinitionSource,
        registry: &BTreeSet<norn_ir::PropertyTypeName>,
        bounds: &Bounds,
    ) -> Result<Batch, ChallengeError> {
        let challenge = self.challenge(source, registry)?;
        let model = lower(source, registry, bounds);
        let goals = challenge.body()["obligations"]
            .as_array()
            .ok_or(ChallengeError::Closure)?
            .iter()
            .map(|goal| {
                let kind = goal["goal"]["kind"].as_str().unwrap_or("");
                let law = goal["law"]
                    .as_u64()
                    .and_then(|i| usize::try_from(i).ok())
                    .and_then(|i| challenge.body()["lawStatements"].get(i));
                let writer = goal["goal"]["writer"]
                    .as_u64()
                    .and_then(|i| usize::try_from(i).ok())
                    .and_then(|i| challenge.body()["writerStatements"].get(i));
                match &model {
                    Ok((law_id, module, _))
                        if law.is_some_and(|l| l["artifactId"] == *law_id)
                            && (kind == "initialization"
                                || kind == "non_vacuity"
                                || (kind == "preservation"
                                    && writer.is_some_and(|w| {
                                        w["identity"]["artifactId"] == bounds.action
                                    }))) =>
                    {
                        Ok(module.clone())
                    }
                    Ok(_) => Err(format!(
                        "unsupported goal {kind}: outside selected one-create pointwise relation"
                    )),
                    Err(reason) => Err(reason.clone()),
                }
            })
            .collect();
        let interpretation = model
            .as_ref()
            .map(|(_, _, types)| types.clone())
            .unwrap_or(Value::Null);
        Ok(Batch {
            challenge,
            bounds: json!({"profile":"norn.quint.one-create/1","action":bounds.action,"inputs":bounds.inputs,"identity":bounds.identity,"seed":bounds.seed,"initialStore":"empty","maxObjects":1,"maxSteps":1,"absentInputs":"all non-identity fields not in inputs","interpretation":interpretation}),
            goals,
        })
    }
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
    demand(scale <= 6, "decimal scale exceeds exact bounded profile")?;
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    demand(
        !whole.is_empty()
            && fraction.len() <= scale as usize
            && fraction.bytes().all(|b| b.is_ascii_digit()),
        "inexact decimal literal",
    )?;
    let sign = if whole.starts_with('-') { -1 } else { 1 };
    let whole = whole
        .parse::<i64>()
        .map_err(|_| "invalid bounded integer")?;
    let fraction = if fraction.is_empty() {
        0
    } else {
        fraction.parse::<i64>().map_err(|_| "invalid fraction")?
    };
    let result = whole
        .checked_mul(10_i64.pow(scale))
        .and_then(|v| {
            v.checked_add(
                sign * fraction
                    * 10_i64.pow(
                        scale
                            - u32::try_from(text.split_once('.').map_or(0, |(_, f)| f.len()))
                                .unwrap_or(0),
                    ),
            )
        })
        .ok_or("decimal overflow")?;
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
    let lexer = regex_lite::Regex::new(r"^\s*(self\.[A-Za-z_][A-Za-z_0-9]*|input\.[A-Za-z_][A-Za-z_0-9]*|[0-9]+(?:\.[0-9]+)?|==|!=|<=|>=|[()+<>-]|and\b|or\b|not\b|true\b|false\b)").map_err(|_| "lexer")?;
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
fn lower(
    source: &DefinitionSource,
    registry: &BTreeSet<norn_ir::PropertyTypeName>,
    bounds: &Bounds,
) -> Result<(String, String, Value), String> {
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
        serde_json::to_value(selected.payload.to_json_value()).map_err(|_| "action")?;
    let action: ActionRoot = serde_json::from_value(action_json.clone()).map_err(|_| "action")?;
    demand(
        action.slice.is_empty()
            && action.commands.is_empty()
            && action.synchronous_rule.is_none()
            && action.quantity_effect.is_none()
            && action.create_links.is_empty()
            && action.extensions.is_none(),
        "unsupported transitive action effects or reads",
    )?;
    let effect = &action_json["effectProgram"];
    demand(
        effect["kind"] == "upsert_input_object" && effect["writeMode"]["kind"] == "create_only",
        "only create-only input writers supported",
    )?;
    let subject = text(&effect["objectKind"])?;
    let identity = text(&effect["objectIdField"])?;
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
        object.unsupported().is_none()
            && object.lifecycles.is_empty()
            && object.owned_collections.is_empty(),
        "unsupported effective object contract",
    )?;
    let mut fields = BTreeMap::new();
    let mut assignments = BTreeMap::new();
    let mut choices = Vec::new();
    let mut variables = Vec::new();
    let mut types = BTreeMap::new();
    let mut numeric = None;
    let schema = &action_json["inputSchema"]["properties"];
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
                && property.constraints.iter().all(|c| c.is_empty()),
            "unsupported property constraints",
        )?;
        let crate::EffectiveKind::Base(kind) = &property.kind else {
            return Err("unresolved property type".into());
        };
        let declared = serde_json::to_value(kind).map_err(|_| "property")?;
        if id == identity {
            demand(
                declared["kind"] == "text" && !bounds.inputs.contains_key(id),
                "identity must be singleton text",
            )?;
            object
                .check_stored_property_value(
                    id,
                    &norn_ir::RuntimeValue::String(bounds.identity.clone()),
                )
                .map_err(|_| "identity outside declaration")?;
            continue;
        }
        demand(
            declared["kind"] == "decimal"
                && declared.get("currencySource").is_none()
                && declared.get("perUnit").is_none()
                && declared.get("rounding").is_none(),
            "only fixed-unit decimals supported",
        )?;
        demand(
            schema[id]["nornType"] == declared
                && schema[id]
                    .as_object()
                    .is_some_and(|s| s.keys().all(|k| k == "type" || k == "nornType")),
            "input decimal contract differs",
        )?;
        let scale = declared["scale"]
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .ok_or("scale")?;
        let interpretation = json!({"scale":scale,"unit":declared["unit"]});
        demand(
            numeric.as_ref().is_none_or(|n| n == &interpretation),
            "mixed scales or units unsupported",
        )?;
        numeric = Some(interpretation);
        let variable = format!("v{index}");
        variables.push(variable.clone());
        types.insert(variable.clone(), json!({"property":id,"type":declared}));
        if let Some((lo, hi)) = bounds.inputs.get(id) {
            let (lo, hi) = (scaled(lo, scale)?, scaled(hi, scale)?);
            demand(lo <= hi && hi - lo <= 1024, "invalid finite interval")?;
            choices.push(format!("nondet i{index} = ({lo}).to({hi}).oneOf()"));
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
        .and_then(|n| n["scale"].as_u64())
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
    for required in action_json["inputSchema"]["required"]
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
            let default = serde_json::to_value(&property.default).map_err(|_| "default")?;
            fields.insert(id.to_owned(), scaled(text(&default)?, scale)?.to_string());
        }
    }
    let laws: Vec<_> = pool
        .iter()
        .filter(|a| a.kind() == ArtifactKind::Law)
        .filter_map(|a| {
            serde_json::from_value::<LawIR>(serde_json::to_value(a.payload.to_json_value()).ok()?)
                .ok()
                .map(|law| (a, law))
        })
        .filter(|(_, law)| law.subject == subject)
        .collect();
    demand(laws.len() == 1, "exactly one pointwise law required")?;
    let (artifact, law) = laws.first().ok_or("law")?;
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
        _ => return Err("unsupported temporal law".into()),
    };
    let condition = law
        .condition
        .as_ref()
        .map(|c| expression(c.as_str(), &assignments, scale))
        .transpose()?
        .unwrap_or_else(|| "true".into());
    let initial = variables
        .iter()
        .map(|v| format!("{v}' = 0"))
        .collect::<Vec<_>>()
        .join(", ");
    let updates = assignments
        .iter()
        .map(|(id, v)| {
            fields
                .get(id)
                .map(|e| format!("{v}' = {e}"))
                .ok_or("missing writer field")
        })
        .collect::<Result<Vec<_>, _>>()?
        .join(", ");
    let frame = variables
        .iter()
        .map(|v| format!("{v}' = {v}"))
        .collect::<Vec<_>>()
        .join(", ");
    let module = format!(
        "module generated {{\n{}\nvar present: bool\naction init = all {{ present' = false, {initial} }}\naction write = {{\n{}\nall {{ not(present), {}, present' = true, {updates} }}\n}}\naction step = any {{ write, all {{ present' = present, {frame} }} }}\nval law = not(present) or not({condition}) or ({response})\nval unreachable = not(present and ({condition}) and ({response}))\n}}\n",
        variables
            .iter()
            .map(|v| format!("var {v}: int"))
            .collect::<Vec<_>>()
            .join("\n"),
        choices.join("\n"),
        if guards.is_empty() {
            "true".into()
        } else {
            format!("({})", guards.join(") and ("))
        }
    );
    Ok((artifact.artifact_id.clone(), module, json!(types)))
}
