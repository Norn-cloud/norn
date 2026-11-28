//! Strict fail-closed parser for `norn test` declarative JSON scenarios.

use std::path::{Path, PathBuf};

use norn_api::local_cell::LocalActor;
use serde_json::{Map, Value};

use super::pack::{PackInput, read_json_file};
use super::scenario::{
    ActionStep, ApplyStep, DefsStep, GetObjectStep, ListObjectsStep, PlanStep, Scenario,
    ScenarioStep, StepScope, TestArgs,
};
use super::scenario_assert::StepExpect;

const MAX_SCENARIO_BYTES: &str = "16 MiB scenario file limit";

/// Load and strictly validate one scenario JSON file.
pub(super) fn load_scenario(path: &Path, args: &TestArgs) -> Result<Scenario, String> {
    let raw = read_json_file(path, MAX_SCENARIO_BYTES)?;
    let obj = raw
        .as_object()
        .ok_or_else(|| format!("{}: scenario root must be a JSON object", path.display()))?;
    reject_unknown_keys(
        obj,
        &[
            "name",
            "org",
            "env",
            "clockMs",
            "pack",
            "basePacks",
            "steps",
        ],
        "scenario",
    )?;
    let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
    let name = req_string(obj, "name", "scenario")?;
    let (org, env) = (
        opt_string(obj, "org", "scenario")?
            .or_else(|| args.org.clone())
            .ok_or_else(|| format!("{}: `org` (or --org) is required", path.display()))?,
        opt_string(obj, "env", "scenario")?
            .or_else(|| args.env.clone())
            .ok_or_else(|| format!("{}: `env` (or --env) is required", path.display()))?,
    );
    let clock_ms = opt_u64(obj, "clockMs", "scenario")?;
    let pack = obj.get("pack").map_or_else(
        || Ok(cli_default_pack(args)),
        |v| parse_pack_object(v, base_dir).map(Some),
    )?;
    let mut base_packs = args.base.clone();
    if let Some(val) = obj.get("basePacks") {
        let items = val
            .as_array()
            .ok_or_else(|| format!("{}: `basePacks` must be an array of paths", path.display()))?;
        for (idx, item) in items.iter().enumerate() {
            let text = item.as_str().ok_or_else(|| {
                format!("{}: `basePacks[{idx}]` must be a string", path.display())
            })?;
            base_packs.push(resolve_rel(base_dir, text));
        }
    }
    let steps_val = obj
        .get("steps")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{}: `steps` must be a JSON array", path.display()))?;
    let mut steps = Vec::with_capacity(steps_val.len());
    for (idx, step_val) in steps_val.iter().enumerate() {
        steps.push(parse_step(
            step_val,
            base_dir,
            &format!("{}: steps[{idx}]", path.display()),
        )?);
    }
    Ok(Scenario {
        name,
        source_path: path.to_path_buf(),
        org,
        env,
        clock_ms,
        pack,
        base_packs,
        steps,
    })
}

fn cli_default_pack(args: &TestArgs) -> Option<PackInput> {
    if let Some(source) = &args.source {
        Some(PackInput::Source(source.clone()))
    } else {
        args.artifacts
            .as_ref()
            .map(|artifacts| PackInput::Artifacts(artifacts.clone()))
    }
}

fn resolve_rel(base_dir: &Path, raw: &str) -> PathBuf {
    let candidate = Path::new(raw);
    if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        base_dir.join(candidate)
    }
}

fn parse_pack_object(val: &Value, base_dir: &Path) -> Result<PackInput, String> {
    let obj = val
        .as_object()
        .ok_or_else(|| "`pack` must be an object with `source` or `artifacts`".to_owned())?;
    reject_unknown_keys(obj, &["source", "artifacts"], "pack")?;
    match (
        opt_string(obj, "source", "pack")?,
        opt_string(obj, "artifacts", "pack")?,
    ) {
        (Some(path), None) => Ok(PackInput::Source(resolve_rel(base_dir, &path))),
        (None, Some(path)) => Ok(PackInput::Artifacts(resolve_rel(base_dir, &path))),
        (Some(_), Some(_)) | (None, None) => {
            Err("`pack` must declare exactly one of `source` or `artifacts`".to_owned())
        }
    }
}

fn parse_step(val: &Value, base_dir: &Path, ctx: &str) -> Result<ScenarioStep, String> {
    let obj = val
        .as_object()
        .ok_or_else(|| format!("{ctx}: step must be a JSON object"))?;
    let op = req_string(obj, "op", ctx)?;
    match op.as_str() {
        "advance_clock" => {
            reject_unknown_keys(obj, &["op", "ms"], ctx)?;
            Ok(ScenarioStep::AdvanceClock {
                ms: req_u64(obj, "ms", ctx)?,
            })
        }
        "defs" => parse_defs_step(obj, ctx),
        "plan" => parse_plan_step(obj, base_dir, ctx),
        "apply" => parse_apply_step(obj, base_dir, ctx),
        "action" => parse_action_step(obj, ctx),
        "get_object" => parse_get_object_step(obj, ctx),
        "list_objects" => parse_list_objects_step(obj, ctx),
        other => Err(format!("{ctx}: unknown step op `{other}`")),
    }
}

fn parse_defs_step(obj: &Map<String, Value>, ctx: &str) -> Result<ScenarioStep, String> {
    reject_unknown_keys(
        obj,
        &["op", "actor", "org", "env", "version", "expect"],
        ctx,
    )?;
    Ok(ScenarioStep::Defs(Box::new(DefsStep {
        actor: parse_opt_actor(obj, ctx)?,
        scope: parse_step_scope(obj, ctx)?,
        version: opt_u64(obj, "version", ctx)?,
        expect: parse_expect(
            obj,
            &[
                "ok",
                "refusalCode",
                "diagnosticCode",
                "minDefinitions",
                "version",
            ],
            ctx,
        )?,
    })))
}

fn parse_plan_step(
    obj: &Map<String, Value>,
    base_dir: &Path,
    ctx: &str,
) -> Result<ScenarioStep, String> {
    reject_unknown_keys(
        obj,
        &["op", "actor", "org", "env", "source", "artifacts", "expect"],
        ctx,
    )?;
    Ok(ScenarioStep::Plan(Box::new(PlanStep {
        actor: parse_opt_actor(obj, ctx)?,
        scope: parse_step_scope(obj, ctx)?,
        pack: parse_step_pack(obj, base_dir, ctx)?,
        expect: parse_expect(
            obj,
            &[
                "ok",
                "refusalCode",
                "diagnosticCode",
                "unchanged",
                "baseVersion",
                "nextVersion",
                "rippleIds",
            ],
            ctx,
        )?,
    })))
}

fn parse_apply_step(
    obj: &Map<String, Value>,
    base_dir: &Path,
    ctx: &str,
) -> Result<ScenarioStep, String> {
    let allowed = [
        "op",
        "actor",
        "org",
        "env",
        "source",
        "artifacts",
        "expectedVersion",
        "expect",
    ];
    reject_unknown_keys(obj, &allowed, ctx)?;
    Ok(ScenarioStep::Apply(Box::new(ApplyStep {
        actor: parse_opt_actor(obj, ctx)?,
        scope: parse_step_scope(obj, ctx)?,
        pack: parse_step_pack(obj, base_dir, ctx)?,
        expected_version: opt_u64(obj, "expectedVersion", ctx)?,
        expect: parse_expect(
            obj,
            &[
                "ok",
                "refusalCode",
                "diagnosticCode",
                "unchanged",
                "definitionsVersion",
            ],
            ctx,
        )?,
    })))
}

fn parse_action_step(obj: &Map<String, Value>, ctx: &str) -> Result<ScenarioStep, String> {
    let allowed = [
        "op",
        "actionId",
        "idempotencyKey",
        "actor",
        "org",
        "env",
        "version",
        "input",
        "expect",
    ];
    reject_unknown_keys(obj, &allowed, ctx)?;
    let input = obj
        .get("input")
        .cloned()
        .ok_or_else(|| format!("{ctx}: `input` is required"))?;
    let expect_keys = [
        "ok",
        "refusalCode",
        "diagnosticCode",
        "assertReceipt",
        "replayed",
        "objectType",
        "objectId",
        "version",
        "outputContains",
    ];
    Ok(ScenarioStep::Action(Box::new(ActionStep {
        action_id: req_string(obj, "actionId", ctx)?,
        idempotency_key: req_string(obj, "idempotencyKey", ctx)?,
        actor: parse_opt_actor(obj, ctx)?,
        scope: parse_step_scope(obj, ctx)?,
        version: opt_u64(obj, "version", ctx)?,
        input,
        expect: parse_expect(obj, &expect_keys, ctx)?,
    })))
}

fn parse_get_object_step(obj: &Map<String, Value>, ctx: &str) -> Result<ScenarioStep, String> {
    reject_unknown_keys(
        obj,
        &["op", "type", "id", "actor", "org", "env", "expect"],
        ctx,
    )?;
    let expect_keys = [
        "ok",
        "refusalCode",
        "diagnosticCode",
        "withheld",
        "version",
        "fields",
    ];
    Ok(ScenarioStep::GetObject(Box::new(GetObjectStep {
        object_type: req_string(obj, "type", ctx)?,
        id: req_string(obj, "id", ctx)?,
        actor: parse_opt_actor(obj, ctx)?,
        scope: parse_step_scope(obj, ctx)?,
        expect: parse_expect(obj, &expect_keys, ctx)?,
    })))
}

fn parse_list_objects_step(obj: &Map<String, Value>, ctx: &str) -> Result<ScenarioStep, String> {
    let allowed = [
        "op", "type", "actor", "org", "env", "limit", "sort", "filter", "expect",
    ];
    reject_unknown_keys(obj, &allowed, ctx)?;
    let limit = match obj.get("limit") {
        None => None,
        Some(Value::Number(num)) => Some(num.to_string()),
        Some(Value::String(text)) => Some(text.clone()),
        Some(Value::Null | Value::Bool(_) | Value::Array(_) | Value::Object(_)) => {
            return Err(format!("{ctx}: `limit` must be an integer or string"));
        }
    };
    let filter = match obj.get("filter") {
        None => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(val) => Some(val.to_string()),
    };
    Ok(ScenarioStep::ListObjects(Box::new(ListObjectsStep {
        object_type: req_string(obj, "type", ctx)?,
        actor: parse_opt_actor(obj, ctx)?,
        scope: parse_step_scope(obj, ctx)?,
        limit,
        sort: opt_string(obj, "sort", ctx)?,
        filter,
        expect: parse_expect(
            obj,
            &["ok", "refusalCode", "diagnosticCode", "count", "minCount"],
            ctx,
        )?,
    })))
}

fn parse_step_pack(
    obj: &Map<String, Value>,
    base_dir: &Path,
    ctx: &str,
) -> Result<PackInput, String> {
    match (
        opt_string(obj, "source", ctx)?,
        opt_string(obj, "artifacts", ctx)?,
    ) {
        (Some(path), None) => Ok(PackInput::Source(resolve_rel(base_dir, &path))),
        (None, Some(path)) => Ok(PackInput::Artifacts(resolve_rel(base_dir, &path))),
        (Some(_), Some(_)) | (None, None) => Err(format!(
            "{ctx}: must declare exactly one of `source` or `artifacts`"
        )),
    }
}

fn parse_step_scope(obj: &Map<String, Value>, ctx: &str) -> Result<StepScope, String> {
    Ok(StepScope {
        org: opt_string(obj, "org", ctx)?,
        env: opt_string(obj, "env", ctx)?,
    })
}

fn parse_opt_actor(obj: &Map<String, Value>, ctx: &str) -> Result<Option<LocalActor>, String> {
    let Some(val) = obj.get("actor") else {
        return Ok(None);
    };
    let actor_ctx = format!("{ctx}.actor");
    let actor_obj = val
        .as_object()
        .ok_or_else(|| format!("{ctx}: `actor` must be a JSON object"))?;
    reject_unknown_keys(actor_obj, &["userId", "roles"], &actor_ctx)?;
    let user_id = req_string(actor_obj, "userId", &actor_ctx)?;
    let roles_val = actor_obj
        .get("roles")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{actor_ctx}: `roles` must be an array of strings"))?;
    let mut roles = Vec::with_capacity(roles_val.len());
    for (idx, item) in roles_val.iter().enumerate() {
        let role = item
            .as_str()
            .ok_or_else(|| format!("{actor_ctx}.roles[{idx}]: role must be a string"))?;
        roles.push(role.to_owned());
    }
    Ok(Some(LocalActor::new(user_id, roles)))
}

fn parse_expect(
    obj: &Map<String, Value>,
    allowed: &[&str],
    ctx: &str,
) -> Result<StepExpect, String> {
    let Some(val) = obj.get("expect") else {
        return Ok(StepExpect::default());
    };
    let expect_ctx = format!("{ctx}.expect");
    let exp = val
        .as_object()
        .ok_or_else(|| format!("{expect_ctx}: must be a JSON object"))?;
    reject_unknown_keys(exp, allowed, &expect_ctx)?;
    Ok(StepExpect {
        ok: opt_bool(exp, "ok", &expect_ctx)?,
        refusal_code: opt_string(exp, "refusalCode", &expect_ctx)?,
        diagnostic_code: opt_string(exp, "diagnosticCode", &expect_ctx)?,
        min_definitions: opt_usize(exp, "minDefinitions", &expect_ctx)?,
        version: opt_u64(exp, "version", &expect_ctx)?,
        unchanged: opt_bool(exp, "unchanged", &expect_ctx)?,
        base_version: opt_u64(exp, "baseVersion", &expect_ctx)?,
        next_version: opt_u64(exp, "nextVersion", &expect_ctx)?,
        definitions_version: opt_u64(exp, "definitionsVersion", &expect_ctx)?,
        ripple_ids: opt_string_vec(exp, "rippleIds", &expect_ctx)?,
        assert_receipt: opt_bool(exp, "assertReceipt", &expect_ctx)?,
        replayed: opt_bool(exp, "replayed", &expect_ctx)?,
        object_type: opt_string(exp, "objectType", &expect_ctx)?,
        object_id: opt_string(exp, "objectId", &expect_ctx)?,
        output_contains: opt_object(exp, "outputContains", &expect_ctx)?,
        withheld: opt_bool(exp, "withheld", &expect_ctx)?,
        fields: opt_object(exp, "fields", &expect_ctx)?,
        count: opt_usize(exp, "count", &expect_ctx)?,
        min_count: opt_usize(exp, "minCount", &expect_ctx)?,
    })
}

fn reject_unknown_keys(
    obj: &Map<String, Value>,
    allowed: &[&str],
    ctx: &str,
) -> Result<(), String> {
    let mut unknown: Vec<&str> = obj
        .keys()
        .map(String::as_str)
        .filter(|key| !allowed.contains(key))
        .collect();
    if unknown.is_empty() {
        return Ok(());
    }
    unknown.sort_unstable();
    Err(format!("{ctx}: unknown field(s): {}", unknown.join(", ")))
}

fn req_string(obj: &Map<String, Value>, key: &str, ctx: &str) -> Result<String, String> {
    obj.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("{ctx}: `{key}` must be a string"))
}

fn opt_string(obj: &Map<String, Value>, key: &str, ctx: &str) -> Result<Option<String>, String> {
    obj.get(key)
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{ctx}: `{key}` must be a string"))
        })
        .transpose()
}

fn opt_bool(obj: &Map<String, Value>, key: &str, ctx: &str) -> Result<Option<bool>, String> {
    obj.get(key)
        .map(|v| {
            v.as_bool()
                .ok_or_else(|| format!("{ctx}: `{key}` must be a boolean"))
        })
        .transpose()
}

fn req_u64(obj: &Map<String, Value>, key: &str, ctx: &str) -> Result<u64, String> {
    obj.get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{ctx}: `{key}` must be a non-negative integer"))
}

fn opt_u64(obj: &Map<String, Value>, key: &str, ctx: &str) -> Result<Option<u64>, String> {
    obj.get(key)
        .map(|v| {
            v.as_u64()
                .ok_or_else(|| format!("{ctx}: `{key}` must be a non-negative integer"))
        })
        .transpose()
}

fn opt_usize(obj: &Map<String, Value>, key: &str, ctx: &str) -> Result<Option<usize>, String> {
    opt_u64(obj, key, ctx)?
        .map(|n| {
            usize::try_from(n)
                .map_err(|err| format!("{ctx}: `{key}` exceeds platform usize: {err}"))
        })
        .transpose()
}

fn opt_string_vec(
    obj: &Map<String, Value>,
    key: &str,
    ctx: &str,
) -> Result<Option<Vec<String>>, String> {
    let Some(val) = obj.get(key) else {
        return Ok(None);
    };
    let items = val
        .as_array()
        .ok_or_else(|| format!("{ctx}: `{key}` must be an array of strings"))?;
    items
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{ctx}: `{key}[{idx}]` must be a string"))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

fn opt_object(
    obj: &Map<String, Value>,
    key: &str,
    ctx: &str,
) -> Result<Option<Map<String, Value>>, String> {
    obj.get(key)
        .map(|v| {
            v.as_object()
                .cloned()
                .ok_or_else(|| format!("{ctx}: `{key}` must be a JSON object"))
        })
        .transpose()
}
