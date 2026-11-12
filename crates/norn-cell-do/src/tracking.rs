//! MLflow-compatible experiment tracking: the wire adapter for the tracking
//! pack (`registry/packs/tracking`).
//!
//! Routes are `/v1/tracking/{orgId}/{environmentId}/api/2.0/mlflow/...`, the
//! subset of MLflow's tracking REST API (Apache-2.0, documented at
//! mlflow.org) that an unchanged `mlflow` client needs to create experiments
//! and runs, log params, metrics and tags, end runs and search them. Every
//! write is one of the pack's actions through the shared governed action
//! service, and every read is the shared authorized object read, as the
//! verified caller: this module only translates the wire format. It holds no
//! authority of its own and no tracking state.
//!
//! Anything outside the subset refuses in MLflow's error shape
//! (`{"error_code", "message"}`) and names what is unsupported; nothing
//! unimplemented answers with an empty success.

use std::collections::BTreeMap;

use norn_api::actions::{ActionQuery, call_action, decode_action_request};
use norn_api::host::DevHost;
use norn_api::object_query::ObjectQueryParams;
use norn_api::object_routes::{get_object_query, get_queried_object};
use norn_api::question_routes::{AdHocQuestions, QuestionRuntime};
use norn_api::refusal::Refusal;
use norn_api::scope::ScopeQuery;
use norn_cell::engine::SqlEngine;
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};

use crate::policy::CellObjectAccess;
use crate::server::Response;

/// Most attempts at one batch's sequence number before refusing. Each failed
/// attempt means another writer committed a batch of the same run between
/// this request's read of the run's head and its commit, so the bound is the
/// number of concurrent writers one request outlasts. The refusal is
/// `TEMPORARILY_UNAVAILABLE`, which the `mlflow` client retries.
pub const MAX_SEQ_ATTEMPTS: usize = 8;
/// MLflow's per-request limits for `runs/log-batch`.
const MAX_BATCH_METRICS: usize = 1000;
const MAX_BATCH_PARAMS: usize = 100;
const MAX_BATCH_TAGS: usize = 100;
const MAX_BATCH_ENTITIES: usize = 1000;
const MAX_KEY_CHARS: usize = 250;
const MAX_PARAM_VALUE_CHARS: usize = 6000;
const MAX_TAG_VALUE_CHARS: usize = 8000;
/// Largest search page: the grid query's own page bound.
const MAX_PAGE: u64 = 200;
const DEFAULT_EXPERIMENT_ID: &str = "0";
const DEFAULT_EXPERIMENT_NAME: &str = "Default";
/// Where artifacts go when an experiment names no location. The cell stores
/// only the URI; clients upload to it directly.
const DEFAULT_ARTIFACT_ROOT: &str = "s3://norn-tracking-artifacts";

const RUN: &str = "tracking.Run";
const EXPERIMENT: &str = "tracking.Experiment";
const RUN_LOG: &str = "tracking.RunLog";

/// One request to the tracking routes, after the host verified its caller.
pub struct TrackingRequest<'a> {
    /// The verified principal, if any.
    pub actor: Option<&'a str>,
    /// HTTP method.
    pub method: &'a str,
    /// The path after `/v1/tracking/`.
    pub rest: &'a str,
    /// Raw query string.
    pub query: &'a str,
    /// Request body.
    pub body: &'a str,
    /// Host clock, Unix seconds.
    pub now: u64,
}

/// An MLflow-shaped refusal.
#[derive(Debug)]
struct MlError {
    status: u16,
    code: &'static str,
    message: String,
}

impl MlError {
    fn invalid(message: impl Into<String>) -> Self {
        Self { status: 400, code: "INVALID_PARAMETER_VALUE", message: message.into() }
    }
    fn missing(message: impl Into<String>) -> Self {
        Self { status: 404, code: "RESOURCE_DOES_NOT_EXIST", message: message.into() }
    }
    fn exists(message: impl Into<String>) -> Self {
        Self { status: 400, code: "RESOURCE_ALREADY_EXISTS", message: message.into() }
    }
    fn endpoint(message: impl Into<String>) -> Self {
        Self { status: 404, code: "ENDPOINT_NOT_FOUND", message: message.into() }
    }
    fn internal(message: impl Into<String>) -> Self {
        Self { status: 500, code: "INTERNAL_ERROR", message: message.into() }
    }
    /// A cell refusal in MLflow's shape. The opaque denial stays opaque.
    fn from_refusal(refusal: &Refusal) -> Self {
        match refusal.status {
            401 | 403 => Self {
                status: 403,
                code: "PERMISSION_DENIED",
                message: "the request is not authorized".into(),
            },
            404 => Self::missing(refusal.message.clone()),
            413 | 400 | 409 | 422 | 429 => Self::invalid(format!("{}: {}", refusal.code, refusal.message)),
            _ => Self::internal(format!("{}: {}", refusal.code, refusal.message)),
        }
    }
}

impl From<Refusal> for MlError {
    fn from(refusal: Refusal) -> Self {
        Self::from_refusal(&refusal)
    }
}

type Outcome<T> = Result<T, MlError>;

/// Serve one tracking request.
pub fn handle<E: SqlEngine>(
    host: &DevHost<E>,
    runtime: Option<&dyn AdHocQuestions<E>>,
    request: &TrackingRequest<'_>,
) -> Response {
    let result = route(host, runtime, request);
    let (status, body) = match result {
        Ok(document) => (200, document),
        Err(error) => (
            error.status,
            json!({ "error_code": error.code, "message": error.message }),
        ),
    };
    Response {
        status,
        body: serde_json::to_string(&body).unwrap_or_else(|_| String::from("{}")),
    }
}

fn route<E: SqlEngine>(
    host: &DevHost<E>,
    runtime: Option<&dyn AdHocQuestions<E>>,
    request: &TrackingRequest<'_>,
) -> Outcome<Value> {
    let mut parts = request.rest.splitn(3, '/');
    let (Some(org), Some(env), Some(tail)) = (parts.next(), parts.next(), parts.next()) else {
        return Err(MlError::endpoint("tracking routes are /v1/tracking/{orgId}/{environmentId}/api/2.0/mlflow/..."));
    };
    let Some(endpoint) = tail.strip_prefix("api/2.0/mlflow/") else {
        return Err(MlError::endpoint(format!("unsupported tracking path `{tail}`: only api/2.0/mlflow is served")));
    };
    // No runtime, no actor: the opaque denial, before anything is read.
    let (Some(runtime), Some(actor)) = (runtime, request.actor) else {
        return Err(MlError::from_refusal(&norn_api::authz::read_denied()));
    };
    let cell = Cell {
        host,
        runtime,
        actor,
        scope: ScopeQuery { org_id: Some(org.to_owned()), environment_id: Some(env.to_owned()) },
        now: request.now,
    };
    let args = Args::parse(request)?;
    cell.ensure_default_experiment()?;
    match (request.method, endpoint) {
        ("POST", "experiments/create") => cell.create_experiment(&args),
        ("GET", "experiments/get") => cell.get_experiment(&args.text("experiment_id")?),
        ("GET", "experiments/get-by-name") => cell.get_experiment(&experiment_id(&args.text("experiment_name")?)),
        ("POST", "experiments/search") => cell.search_experiments(&args),
        ("POST", "runs/create") => cell.create_run(&args),
        ("GET", "runs/get") => cell.get_run(&args.run_id()?).map(|run| json!({ "run": run })),
        ("POST", "runs/update") => cell.update_run(&args),
        ("POST", "runs/log-batch") => cell.log_batch(&args),
        ("POST", "runs/log-metric") => cell.log_single(&args, Single::Metric),
        ("POST", "runs/log-parameter") => cell.log_single(&args, Single::Param),
        ("POST", "runs/set-tag") => cell.log_single(&args, Single::Tag),
        ("POST", "runs/search") => cell.search_runs(&args),
        ("GET", "metrics/get-history") => cell.metric_history(&args),
        ("GET", "artifacts/list") => Err(MlError::endpoint(
            "artifacts/list is not supported: the cell stores only each run's artifact_uri; list the artifacts at that URI directly",
        )),
        (method, endpoint) => Err(MlError::endpoint(format!(
            "{method} api/2.0/mlflow/{endpoint} is not supported by this tracking server; supported: experiments create, get, get-by-name, search; runs create, get, update, log-batch, log-metric, log-parameter, set-tag, search; metrics get-history"
        ))),
    }
}

/// The request's arguments: the JSON body for `POST`, the query for `GET`.
struct Args(Map<String, Value>);

impl Args {
    fn parse(request: &TrackingRequest<'_>) -> Outcome<Self> {
        if request.method == "GET" {
            let mut map = Map::new();
            for (key, value) in form_urlencoded::parse(request.query.as_bytes()) {
                map.insert(key.into_owned(), Value::String(value.into_owned()));
            }
            return Ok(Self(map));
        }
        if request.body.trim().is_empty() {
            return Ok(Self(Map::new()));
        }
        match serde_json::from_str::<Value>(request.body) {
            Ok(Value::Object(map)) => Ok(Self(map)),
            Ok(_) => Err(MlError::invalid("the request body is not a JSON object")),
            Err(error) => Err(MlError::invalid(format!("the request body is not valid JSON: {error}"))),
        }
    }
    fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key).filter(|value| !value.is_null())
    }
    fn text(&self, key: &str) -> Outcome<String> {
        self.optional_text(key)?
            .ok_or_else(|| MlError::invalid(format!("Missing value for required parameter '{key}'")))
    }
    fn optional_text(&self, key: &str) -> Outcome<Option<String>> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::String(text)) => Ok(Some(text.clone())),
            Some(Value::Number(number)) => Ok(Some(number.to_string())),
            Some(_) => Err(MlError::invalid(format!("'{key}' must be a string"))),
        }
    }
    fn integer(&self, key: &str) -> Outcome<Option<i64>> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::Number(number)) => number
                .as_i64()
                .map(Some)
                .ok_or_else(|| MlError::invalid(format!("'{key}' must be an integer"))),
            Some(Value::String(text)) => text
                .parse()
                .map(Some)
                .map_err(|_| MlError::invalid(format!("'{key}' must be an integer"))),
            Some(_) => Err(MlError::invalid(format!("'{key}' must be an integer"))),
        }
    }
    fn list(&self, key: &str) -> Outcome<Vec<Value>> {
        match self.get(key) {
            None => Ok(Vec::new()),
            Some(Value::Array(items)) => Ok(items.clone()),
            Some(_) => Err(MlError::invalid(format!("'{key}' must be a list"))),
        }
    }
    /// `run_id`, or `run_uuid` from older clients; both must agree.
    fn run_id(&self) -> Outcome<String> {
        let id = self.optional_text("run_id")?;
        let uuid = self.optional_text("run_uuid")?;
        match (id, uuid) {
            (Some(id), Some(uuid)) if id != uuid => {
                Err(MlError::invalid("run_id and run_uuid name different runs"))
            }
            (Some(id), _) | (None, Some(id)) => Ok(id),
            (None, None) => Err(MlError::invalid("Missing value for required parameter 'run_id'")),
        }
    }
}

/// The experiment id for a name: stable, so names are unique by construction.
fn experiment_id(name: &str) -> String {
    if name == DEFAULT_EXPERIMENT_NAME {
        return DEFAULT_EXPERIMENT_ID.to_owned();
    }
    hex(&Sha256::digest(format!("norn.tracking.experiment/{name}")))
        .chars()
        .take(16)
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _infallible = write!(out, "{byte:02x}");
        out
    })
}

fn digest(value: &Value) -> String {
    hex(&Sha256::digest(serde_json::to_string(value).unwrap_or_default()))
}

/// A float64 as exact text: its shortest round-trip decimal, or `NaN`,
/// `Infinity`, `-Infinity`. JSON cannot carry the three specials, so MLflow
/// clients send them as those strings.
fn metric_text(value: &Value) -> Outcome<String> {
    let parsed = match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => match text.as_str() {
            "NaN" => Some(f64::NAN),
            "Infinity" => Some(f64::INFINITY),
            "-Infinity" => Some(f64::NEG_INFINITY),
            other => other.parse::<f64>().ok(),
        },
        _ => None,
    };
    let number = parsed.ok_or_else(|| MlError::invalid("a metric value must be a number"))?;
    Ok(if number.is_nan() {
        "NaN".to_owned()
    } else if number.is_infinite() {
        if number > 0.0 { "Infinity" } else { "-Infinity" }.to_owned()
    } else {
        format!("{number:?}")
    })
}

/// The stored text back as MLflow's JSON value: a number, or the special as a string.
fn metric_value(text: &str) -> Value {
    match text {
        "NaN" | "Infinity" | "-Infinity" => Value::String(text.to_owned()),
        other => other
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map_or_else(|| Value::String(other.to_owned()), Value::Number),
    }
}

enum Single {
    Metric,
    Param,
    Tag,
}

/// One logged batch, in the RunLog's stored form.
#[derive(Default)]
struct Batch {
    params: Vec<Value>,
    tags: Vec<Value>,
    metrics: Vec<Value>,
    end_time: Option<i64>,
    run_name: Option<String>,
}

/// Everything a run's batches say, folded in sequence order.
#[derive(Default)]
struct Folded {
    /// Earliest value per key.
    params: BTreeMap<String, String>,
    /// Later batches that logged a different value for an existing key.
    param_conflicts: Vec<Value>,
    /// Latest value per key.
    tags: BTreeMap<String, String>,
    /// Every point, in batch order.
    metrics: Vec<(String, String, i64, i64)>,
    end_time: Option<i64>,
    run_name: Option<String>,
}

struct Cell<'h, E: SqlEngine> {
    host: &'h DevHost<E>,
    runtime: &'h dyn AdHocQuestions<E>,
    actor: &'h str,
    scope: ScopeQuery,
    now: u64,
}

impl<E: SqlEngine> Cell<'_, E> {
    fn disclosure(&self) -> &dyn QuestionRuntime<E> {
        self.runtime
    }

    /// Call one pack action as the verified caller.
    fn act(&self, action: &str, input: &Value, key: &str) -> Result<norn_api::actions::ActionReceipt, Refusal> {
        let body = serde_json::to_string(&json!({ "input": input, "idempotencyKey": key }))
            .map_err(Refusal::host)?;
        let request = decode_action_request(&body)?;
        let query = ActionQuery {
            org_id: self.scope.org_id.clone(),
            environment_id: self.scope.environment_id.clone(),
            version: None,
        };
        call_action(
            self.host,
            &query,
            Some(self.actor),
            action,
            &request,
            self.disclosure(),
            norn_kernel::LogicalTime::from_millis(self.now.saturating_mul(1000)),
        )
    }

    /// One object's values, or `None` when it is absent (or invisible).
    fn read(&self, object_type: &str, id: &str) -> Outcome<Option<Map<String, Value>>> {
        match get_queried_object(self.host, &self.scope, Some(self.actor), object_type, id, &CellObjectAccess) {
            Ok(record) => {
                let Some(values) = record.values else {
                    return Err(MlError::from_refusal(&norn_api::authz::read_denied()));
                };
                let text = serde_json::to_string(&values).map_err(|error| MlError::internal(error.to_string()))?;
                match serde_json::from_str(&text) {
                    Ok(Value::Object(map)) => Ok(Some(map)),
                    _ => Err(MlError::internal("an object's values are not a JSON object")),
                }
            }
            Err(refusal) if refusal.code == "object.not_found" => Ok(None),
            Err(refusal) => Err(refusal.into()),
        }
    }

    fn ensure_default_experiment(&self) -> Outcome<()> {
        if self.read(EXPERIMENT, DEFAULT_EXPERIMENT_ID)?.is_some() {
            return Ok(());
        }
        let input = json!({
            "experiment_id": DEFAULT_EXPERIMENT_ID,
            "name": DEFAULT_EXPERIMENT_NAME,
            "artifact_location": format!("{DEFAULT_ARTIFACT_ROOT}/{DEFAULT_EXPERIMENT_ID}"),
            "creation_time": 0,
            "tags": "[]",
        });
        match self.act("tracking.experiment.create", &input, "tracking-default-experiment") {
            Ok(_) => Ok(()),
            // A concurrent request created it first.
            Err(refusal) if refusal.code == "action.write_mode_failed" => Ok(()),
            Err(refusal) => Err(refusal.into()),
        }
    }

    fn create_experiment(&self, args: &Args) -> Outcome<Value> {
        let name = args.text("name")?;
        if name.trim().is_empty() || name.chars().count() > 500 {
            return Err(MlError::invalid("experiment name must be 1 to 500 characters"));
        }
        let id = experiment_id(&name);
        let location = args
            .optional_text("artifact_location")?
            .unwrap_or_else(|| format!("{DEFAULT_ARTIFACT_ROOT}/{id}"));
        let tags = tag_list(&args.list("tags")?)?;
        let input = json!({
            "experiment_id": id,
            "name": name,
            "artifact_location": location,
            "creation_time": now_ms(self.now),
            "tags": serde_json::to_string(&tags).unwrap_or_default(),
        });
        match self.act("tracking.experiment.create", &input, &format!("experiment/{id}")) {
            Ok(_) => Ok(json!({ "experiment_id": id })),
            Err(refusal) if refusal.code == "action.write_mode_failed" || refusal.code == "action.idempotency_conflict" => {
                Err(MlError::exists(format!("Experiment '{name}' already exists.")))
            }
            Err(refusal) => Err(refusal.into()),
        }
    }

    fn get_experiment(&self, id: &str) -> Outcome<Value> {
        let experiment = self
            .read(EXPERIMENT, id)?
            .ok_or_else(|| MlError::missing(format!("No Experiment with id={id} exists")))?;
        Ok(json!({ "experiment": experiment_document(&experiment) }))
    }

    fn search_experiments(&self, args: &Args) -> Outcome<Value> {
        if let Some(filter) = args.optional_text("filter")?.filter(|filter| !filter.trim().is_empty()) {
            return Err(MlError::invalid(format!(
                "experiment filter `{filter}` is not supported: experiments/search accepts no filter"
            )));
        }
        if !args.list("order_by")?.is_empty() {
            return Err(MlError::invalid("experiments/search accepts no order_by"));
        }
        view_type(args.optional_text("view_type")?.as_deref())?;
        let page = self.page(EXPERIMENT, None, None, args)?;
        let experiments: Vec<Value> = page.0.iter().map(experiment_document).collect();
        Ok(with_token(json!({ "experiments": experiments }), page.1))
    }

    fn create_run(&self, args: &Args) -> Outcome<Value> {
        let experiment = args
            .optional_text("experiment_id")?
            .unwrap_or_else(|| DEFAULT_EXPERIMENT_ID.to_owned());
        let location = self
            .read(EXPERIMENT, &experiment)?
            .and_then(|values| values.get("artifact_location").and_then(Value::as_str).map(str::to_owned))
            .ok_or_else(|| MlError::missing(format!("No Experiment with id={experiment} exists")))?;
        let user = args.optional_text("user_id")?.unwrap_or_default();
        let start = args.integer("start_time")?.unwrap_or_else(|| now_ms(self.now));
        let tags = tag_list(&args.list("tags")?)?;
        // The id is a digest of the request, so a retried create replays.
        let seed = json!([experiment, user, args.optional_text("run_name")?, start, tags]);
        let run_id: String = digest(&seed).chars().take(32).collect();
        let name = args
            .optional_text("run_name")?
            .or_else(|| run_name_tag(&tags))
            .unwrap_or_else(|| format!("run-{}", run_id.chars().take(8).collect::<String>()));
        let input = json!({
            "run_id": run_id,
            "experiment_id": experiment,
            "run_name": name,
            "user_id": user,
            "start_time": start,
            "artifact_uri": format!("{location}/{run_id}/artifacts"),
            "tags": serde_json::to_string(&tags).unwrap_or_default(),
        });
        self.act("tracking.run.create", &input, &format!("run/{run_id}"))
            .map_err(MlError::from)?;
        Ok(json!({ "run": self.get_run(&run_id)? }))
    }

    /// The run's stored object, or `RESOURCE_DOES_NOT_EXIST`.
    fn run(&self, run_id: &str) -> Outcome<Map<String, Value>> {
        self.read(RUN, run_id)?
            .ok_or_else(|| MlError::missing(format!("Run with id={run_id} not found")))
    }

    fn get_run(&self, run_id: &str) -> Outcome<Value> {
        let run = self.run(run_id)?;
        let folded = self.fold(run_id)?;
        Ok(run_document(run_id, &run, &folded))
    }

    fn update_run(&self, args: &Args) -> Outcome<Value> {
        let run_id = args.run_id()?;
        let run = self.run(&run_id)?;
        let current = run.get("status").and_then(Value::as_str).unwrap_or("RUNNING").to_owned();
        let status = args.optional_text("status")?;
        let transition = match status.as_deref() {
            None => None,
            Some(next) if next == current => None,
            Some("FINISHED") if current == "RUNNING" => Some("tracking.run.finish"),
            Some("FAILED") if current == "RUNNING" => Some("tracking.run.fail"),
            Some("KILLED") if current == "RUNNING" => Some("tracking.run.kill"),
            Some(next) => {
                return Err(MlError::invalid(format!(
                    "a run's status cannot change from {current} to {next}; a RUNNING run may become FINISHED, FAILED or KILLED"
                )));
            }
        };
        let update = json!({ "status": status, "end_time": args.integer("end_time")?, "run_name": args.optional_text("run_name")? });
        let key = digest(&update);
        let batch = Batch {
            end_time: args.integer("end_time")?,
            run_name: args.optional_text("run_name")?,
            ..Batch::default()
        };
        // The end time and name commit first, then the status: a retry after a
        // crash between the two replays the first and completes the second.
        if batch.end_time.is_some() || batch.run_name.is_some() {
            self.append(&run_id, &batch, &format!("{run_id}/update/{key}/1"))?;
        }
        if let Some(action) = transition {
            self.act(action, &json!({ "run_id": run_id }), &format!("{run_id}/update/{key}/2"))
                .map_err(MlError::from)?;
        }
        let run = self.get_run(&run_id)?;
        Ok(json!({ "run_info": run.get("info").cloned().unwrap_or(Value::Null) }))
    }

    fn log_single(&self, args: &Args, kind: Single) -> Outcome<Value> {
        let run_id = args.run_id()?;
        let key = args.text("key")?;
        let mut batch = Batch::default();
        match kind {
            Single::Metric => {
                let value = args.get("value").ok_or_else(|| MlError::invalid("Missing value for required parameter 'value'"))?;
                batch.metrics.push(json!({
                    "key": key,
                    "value": metric_text(value)?,
                    "timestamp": args.integer("timestamp")?.unwrap_or_else(|| now_ms(self.now)),
                    "step": args.integer("step")?.unwrap_or(0),
                }));
            }
            Single::Param => batch.params.push(json!({ "key": key, "value": args.text("value")? })),
            Single::Tag => batch.tags.push(json!({ "key": key, "value": args.text("value")? })),
        }
        check_entries(&batch)?;
        self.log(&run_id, &batch, &digest(&Value::Object(args.0.clone())))
    }

    fn log_batch(&self, args: &Args) -> Outcome<Value> {
        let run_id = args.run_id()?;
        let (metrics, params, tags) = (args.list("metrics")?, args.list("params")?, args.list("tags")?);
        if metrics.len() > MAX_BATCH_METRICS
            || params.len() > MAX_BATCH_PARAMS
            || tags.len() > MAX_BATCH_TAGS
            || metrics.len() + params.len() + tags.len() > MAX_BATCH_ENTITIES
        {
            return Err(MlError::invalid(format!(
                "A batch logging request can contain at most {MAX_BATCH_METRICS} metrics, {MAX_BATCH_PARAMS} params and {MAX_BATCH_TAGS} tags, and at most {MAX_BATCH_ENTITIES} entities in total. Got {} metrics, {} params and {} tags.",
                metrics.len(),
                params.len(),
                tags.len()
            )));
        }
        let mut batch = Batch::default();
        for metric in &metrics {
            let key = entry_key(metric)?;
            let value = metric.get("value").ok_or_else(|| MlError::invalid("a metric needs a value"))?;
            batch.metrics.push(json!({
                "key": key,
                "value": metric_text(value)?,
                "timestamp": metric.get("timestamp").and_then(Value::as_i64).unwrap_or_else(|| now_ms(self.now)),
                "step": metric.get("step").and_then(Value::as_i64).unwrap_or(0),
            }));
        }
        batch.params = key_values(&params)?;
        batch.tags = key_values(&tags)?;
        check_entries(&batch)?;
        self.log(&run_id, &batch, &digest(&Value::Object(args.0.clone())))
    }

    /// Log one batch after refusing a param that changes a logged value.
    fn log(&self, run_id: &str, batch: &Batch, request_digest: &str) -> Outcome<Value> {
        self.run(run_id)?;
        if !batch.params.is_empty() {
            let logged = self.fold(run_id)?.params;
            for param in &batch.params {
                let (key, value) = (field(param, "key"), field(param, "value"));
                if let Some(old) = logged.get(key).filter(|old| *old != value) {
                    return Err(MlError::invalid(format!(
                        "Changing param values is not allowed. Param with key='{key}' was already logged with value='{old}' for run ID='{run_id}'. Attempted logging new value '{value}'."
                    )));
                }
            }
        }
        if batch.params.is_empty() && batch.tags.is_empty() && batch.metrics.is_empty() {
            return Ok(json!({}));
        }
        self.append(run_id, batch, &format!("{run_id}/{request_digest}"))?;
        Ok(json!({}))
    }

    /// Append one RunLog at the run's next sequence number, retrying a number
    /// another writer took. `key` is the request's idempotency key: a retry
    /// of a batch that already landed ends as a replay, never a second batch.
    fn append(&self, run_id: &str, batch: &Batch, key: &str) -> Outcome<()> {
        let mut seq = self.head(run_id)?;
        let batch_digest = digest(&json!([batch.params, batch.tags, batch.metrics, batch.end_time, batch.run_name]));
        for _ in 0..MAX_SEQ_ATTEMPTS {
            let mut input = json!({
                "run_id": run_id,
                "seq": seq,
                "log_id": format!("{run_id}/{seq}"),
                "digest": batch_digest,
                "params": serde_json::to_string(&batch.params).unwrap_or_default(),
                "tags": serde_json::to_string(&batch.tags).unwrap_or_default(),
                "metrics": serde_json::to_string(&batch.metrics).unwrap_or_default(),
            });
            if let Some(object) = input.as_object_mut() {
                if let Some(end) = batch.end_time {
                    object.insert("end_time".into(), json!(end));
                }
                if let Some(name) = &batch.run_name {
                    object.insert("run_name".into(), json!(name));
                }
                if seq > 0 {
                    object.insert("prev_id".into(), json!(format!("{run_id}/{}", seq - 1)));
                }
            }
            let action = if seq == 0 { "tracking.runlog.first" } else { "tracking.runlog.add" };
            match self.act(action, &input, key) {
                Ok(_) => return Ok(()),
                // This key already committed, at an earlier number: the batch landed.
                Err(refusal) if refusal.code == "action.idempotency_conflict" => return Ok(()),
                // Another writer took this number; try the next one.
                Err(refusal) if refusal.code == "action.write_mode_failed" => seq += 1,
                Err(refusal) if refusal.status == 500 => {
                    return Err(MlError::internal(format!(
                        "run {run_id}: batch {seq} could not be written; a record may hold that position at a non-canonical id ({})",
                        refusal.code
                    )));
                }
                Err(refusal) => return Err(refusal.into()),
            }
        }
        Err(MlError {
            status: 503,
            code: "TEMPORARILY_UNAVAILABLE",
            message: format!(
                "run {run_id}: {MAX_SEQ_ATTEMPTS} concurrent writers took this batch's sequence number in turn; retry"
            ),
        })
    }

    fn exists(&self, run_id: &str, seq: u64) -> Outcome<bool> {
        Ok(self.read(RUN_LOG, &format!("{run_id}/{seq}"))?.is_some())
    }

    /// The number of batches the run holds: the first canonical id that is
    /// absent, found in O(log n) reads.
    fn head(&self, run_id: &str) -> Outcome<u64> {
        if !self.exists(run_id, 0)? {
            return Ok(0);
        }
        let (mut present, mut absent) = (0_u64, 1_u64);
        while self.exists(run_id, absent)? {
            present = absent;
            absent = absent.saturating_mul(2).saturating_add(1);
        }
        while absent - present > 1 {
            let middle = present + (absent - present) / 2;
            if self.exists(run_id, middle)? {
                present = middle;
            } else {
                absent = middle;
            }
        }
        Ok(absent)
    }

    /// Fold every batch of the run, in sequence order. A batch that is not at
    /// its canonical id refuses the whole read: never a partial run.
    fn fold(&self, run_id: &str) -> Outcome<Folded> {
        let head = self.head(run_id)?;
        let mut folded = Folded::default();
        for seq in 0..head {
            let Some(batch) = self.read(RUN_LOG, &format!("{run_id}/{seq}"))? else {
                return Err(MlError::internal(format!(
                    "run {run_id}: batch {seq} is not at its canonical id {run_id}/{seq}; the run's log cannot be read"
                )));
            };
            if batch.get("run_id").and_then(Value::as_str) != Some(run_id)
                || batch.get("seq").and_then(Value::as_u64) != Some(seq)
            {
                return Err(MlError::internal(format!(
                    "run {run_id}: the record at {run_id}/{seq} belongs to another run or position"
                )));
            }
            fold_batch(&mut folded, &batch, seq);
        }
        Ok(folded)
    }

    fn search_runs(&self, args: &Args) -> Outcome<Value> {
        let experiments: Vec<String> = args
            .list("experiment_ids")?
            .iter()
            .filter_map(|id| id.as_str().map(str::to_owned).or_else(|| id.as_i64().map(|n| n.to_string())))
            .collect();
        if experiments.is_empty() || experiments.len() > 50 {
            return Err(MlError::invalid("runs/search needs 1 to 50 experiment_ids"));
        }
        view_type(args.optional_text("run_view_type")?.as_deref())?;
        let mut clauses = vec![json!({ "property": "experiment_id", "op": "in", "value": experiments })];
        if let Some(filter) = args.optional_text("filter")? {
            clauses.extend(run_filter(&filter)?);
        }
        let sort = run_order(&args.list("order_by")?)?;
        let (runs, token) = self.page(RUN, Some(json!({ "all": clauses })), Some(sort), args)?;
        let mut documents = Vec::with_capacity(runs.len());
        for run in &runs {
            let id = run.get("run_id").and_then(Value::as_str).unwrap_or_default();
            documents.push(run_document(id, run, &self.fold(id)?));
        }
        Ok(with_token(json!({ "runs": documents }), token))
    }

    /// One page of a type's objects; MLflow's `page_token` is the grid cursor.
    fn page(
        &self,
        object_type: &str,
        filter: Option<Value>,
        sort: Option<&str>,
        args: &Args,
    ) -> Outcome<(Vec<Map<String, Value>>, Option<String>)> {
        let limit = args.integer("max_results")?.map_or(MAX_PAGE, |n| u64::try_from(n).unwrap_or(1).clamp(1, MAX_PAGE));
        let params = ObjectQueryParams {
            limit: Some(limit.to_string()),
            sort: sort.map(str::to_owned),
            filter: filter.map(|filter| filter.to_string()),
            after: args.optional_text("page_token")?.filter(|token| !token.is_empty()),
        };
        let page = get_object_query(self.host, &self.scope, Some(self.actor), object_type, &params, &CellObjectAccess)?;
        let mut rows = Vec::with_capacity(page.records.len());
        for record in page.records {
            let Some(values) = record.values else { continue };
            let text = serde_json::to_string(&values).map_err(|error| MlError::internal(error.to_string()))?;
            if let Ok(Value::Object(map)) = serde_json::from_str(&text) {
                rows.push(map);
            }
        }
        Ok((rows, page.next))
    }

    fn metric_history(&self, args: &Args) -> Outcome<Value> {
        let run_id = args.run_id()?;
        let key = args.text("metric_key")?;
        if args.optional_text("page_token")?.is_some_and(|token| !token.is_empty()) {
            return Err(MlError::invalid("metrics/get-history returns every point in one page; page_token is not supported"));
        }
        self.run(&run_id)?;
        let metrics: Vec<Value> = self
            .fold(&run_id)?
            .metrics
            .into_iter()
            .filter(|(name, ..)| *name == key)
            .map(|(name, value, timestamp, step)| {
                json!({ "key": name, "value": metric_value(&value), "timestamp": timestamp, "step": step })
            })
            .collect();
        Ok(json!({ "metrics": metrics }))
    }
}

fn now_ms(seconds: u64) -> i64 {
    i64::try_from(seconds.saturating_mul(1000)).unwrap_or(i64::MAX)
}

fn field<'v>(entry: &'v Value, key: &str) -> &'v str {
    entry.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn entry_key(entry: &Value) -> Outcome<String> {
    entry
        .get("key")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| MlError::invalid("every metric, param and tag needs a string key"))
}

fn key_values(entries: &[Value]) -> Outcome<Vec<Value>> {
    entries
        .iter()
        .map(|entry| {
            let key = entry_key(entry)?;
            let value = entry
                .get("value")
                .and_then(|value| value.as_str().map(str::to_owned).or_else(|| (!value.is_null()).then(|| value.to_string())))
                .unwrap_or_default();
            Ok(json!({ "key": key, "value": value }))
        })
        .collect()
}

fn tag_list(tags: &[Value]) -> Outcome<Vec<Value>> {
    let tags = key_values(tags)?;
    check_entries(&Batch { tags: tags.clone(), ..Batch::default() })?;
    Ok(tags)
}

fn run_name_tag(tags: &[Value]) -> Option<String> {
    tags.iter()
        .find(|tag| field(tag, "key") == "mlflow.runName")
        .map(|tag| field(tag, "value").to_owned())
}

fn check_entries(batch: &Batch) -> Outcome<()> {
    let too_long = |entry: &Value, limit: usize| {
        field(entry, "key").chars().count() > MAX_KEY_CHARS || field(entry, "value").chars().count() > limit
    };
    if let Some(entry) = batch.params.iter().find(|entry| too_long(entry, MAX_PARAM_VALUE_CHARS)) {
        return Err(MlError::invalid(format!(
            "param '{}' exceeds the limits: keys at most {MAX_KEY_CHARS} characters, values at most {MAX_PARAM_VALUE_CHARS}",
            field(entry, "key").chars().take(40).collect::<String>()
        )));
    }
    if let Some(entry) = batch.tags.iter().find(|entry| too_long(entry, MAX_TAG_VALUE_CHARS)) {
        return Err(MlError::invalid(format!(
            "tag '{}' exceeds the limits: keys at most {MAX_KEY_CHARS} characters, values at most {MAX_TAG_VALUE_CHARS}",
            field(entry, "key").chars().take(40).collect::<String>()
        )));
    }
    if batch.metrics.iter().any(|entry| field(entry, "key").chars().count() > MAX_KEY_CHARS) {
        return Err(MlError::invalid(format!("metric keys are at most {MAX_KEY_CHARS} characters")));
    }
    Ok(())
}

fn view_type(view: Option<&str>) -> Outcome<()> {
    match view {
        None | Some("ACTIVE_ONLY" | "ALL") => Ok(()),
        Some(other) => Err(MlError::invalid(format!(
            "view type {other} is not supported: experiments and runs cannot be deleted here, so only ACTIVE_ONLY and ALL apply"
        ))),
    }
}

/// The supported filter subset: clauses joined by `and`, each
/// `attributes.status = '...'` or `attributes.run_name = '...'`.
fn run_filter(filter: &str) -> Outcome<Vec<Value>> {
    let mut clauses = Vec::new();
    for clause in filter.split(" and ").chain(std::iter::empty()).flat_map(|part| part.split(" AND ")) {
        let clause = clause.trim();
        if clause.is_empty() {
            continue;
        }
        let unsupported = || {
            MlError::invalid(format!(
                "filter clause `{clause}` is not supported: this server filters runs only by `attributes.status = '...'` and `attributes.run_name = '...'` joined by `and`"
            ))
        };
        let (left, right) = clause.split_once('=').ok_or_else(unsupported)?;
        let property = match left.trim().trim_end_matches('!') {
            "attributes.status" | "attribute.status" | "status" => "status",
            "attributes.run_name" | "attribute.run_name" | "run_name" | "tags.mlflow.runName" => "run_name",
            _ => return Err(unsupported()),
        };
        if left.trim().ends_with('!') {
            return Err(unsupported());
        }
        let value = right.trim();
        let value = value
            .strip_prefix('\'')
            .and_then(|value| value.strip_suffix('\''))
            .or_else(|| value.strip_prefix('"').and_then(|value| value.strip_suffix('"')))
            .ok_or_else(unsupported)?;
        clauses.push(json!({ "property": property, "op": "eq", "value": value }));
    }
    Ok(clauses)
}

fn run_order(order_by: &[Value]) -> Outcome<&'static str> {
    match order_by.iter().map(|value| value.as_str().unwrap_or_default().trim()).collect::<Vec<_>>().as_slice() {
        [] | ["attributes.start_time DESC" | "start_time DESC"] => Ok("start_time:desc"),
        ["attributes.start_time ASC" | "attributes.start_time" | "start_time ASC" | "start_time"] => Ok("start_time:asc"),
        other => Err(MlError::invalid(format!(
            "order_by {other:?} is not supported: this server orders runs only by attributes.start_time"
        ))),
    }
}

fn with_token(mut document: Value, token: Option<String>) -> Value {
    if let (Some(token), Some(object)) = (token, document.as_object_mut()) {
        object.insert("next_page_token".into(), Value::String(token));
    }
    document
}

fn parse_entries(values: &Map<String, Value>, key: &str) -> Vec<Value> {
    values
        .get(key)
        .and_then(Value::as_str)
        .and_then(|text| serde_json::from_str::<Vec<Value>>(text).ok())
        .unwrap_or_default()
}

fn fold_batch(folded: &mut Folded, batch: &Map<String, Value>, seq: u64) {
    for param in parse_entries(batch, "params") {
        let (key, value) = (field(&param, "key").to_owned(), field(&param, "value").to_owned());
        match folded.params.get(&key) {
            None => {
                folded.params.insert(key, value);
            }
            Some(kept) if *kept != value => folded.param_conflicts.push(json!({
                "key": key, "kept": kept, "ignored": value, "batch": seq,
            })),
            Some(_) => {}
        }
    }
    for tag in parse_entries(batch, "tags") {
        folded.tags.insert(field(&tag, "key").to_owned(), field(&tag, "value").to_owned());
    }
    for metric in parse_entries(batch, "metrics") {
        folded.metrics.push((
            field(&metric, "key").to_owned(),
            field(&metric, "value").to_owned(),
            metric.get("timestamp").and_then(Value::as_i64).unwrap_or(0),
            metric.get("step").and_then(Value::as_i64).unwrap_or(0),
        ));
    }
    if let Some(end) = batch.get("end_time").and_then(Value::as_i64) {
        folded.end_time = Some(end);
    }
    if let Some(name) = batch.get("run_name").and_then(Value::as_str) {
        folded.run_name = Some(name.to_owned());
    }
}

fn experiment_document(values: &Map<String, Value>) -> Value {
    let tags = parse_entries(values, "tags");
    json!({
        "experiment_id": values.get("experiment_id"),
        "name": values.get("name"),
        "artifact_location": values.get("artifact_location"),
        "lifecycle_stage": "active",
        "creation_time": values.get("creation_time"),
        "last_update_time": values.get("creation_time"),
        "tags": tags,
    })
}

fn run_document(run_id: &str, run: &Map<String, Value>, folded: &Folded) -> Value {
    let status = run.get("status").and_then(Value::as_str).unwrap_or("RUNNING");
    let terminal = status != "RUNNING";
    let name = folded
        .run_name
        .clone()
        .or_else(|| run.get("run_name").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default();
    let mut tags: BTreeMap<String, String> = parse_entries(run, "tags")
        .iter()
        .map(|tag| (field(tag, "key").to_owned(), field(tag, "value").to_owned()))
        .collect();
    tags.extend(folded.tags.clone());
    tags.insert("mlflow.runName".into(), name.clone());
    if !folded.param_conflicts.is_empty() {
        // Two writers logged different values for one param; the earlier wins.
        tags.insert(
            "norn.param_conflicts".into(),
            serde_json::to_string(&folded.param_conflicts).unwrap_or_default(),
        );
    }
    // Latest point per key: highest step, then latest timestamp, then latest logged.
    let mut latest: BTreeMap<&str, (i64, i64, &str)> = BTreeMap::new();
    for (key, value, timestamp, step) in &folded.metrics {
        let candidate = (*step, *timestamp, value.as_str());
        latest
            .entry(key.as_str())
            .and_modify(|kept| {
                if (candidate.0, candidate.1) >= (kept.0, kept.1) {
                    *kept = candidate;
                }
            })
            .or_insert(candidate);
    }
    let mut info = json!({
        "run_id": run_id,
        "run_uuid": run_id,
        "run_name": name,
        "experiment_id": run.get("experiment_id"),
        "user_id": run.get("user_id"),
        "status": status,
        "start_time": run.get("start_time"),
        "artifact_uri": run.get("artifact_uri"),
        "lifecycle_stage": "active",
    });
    if let (true, Some(end), Some(object)) = (terminal, folded.end_time, info.as_object_mut()) {
        object.insert("end_time".into(), json!(end));
    }
    json!({
        "info": info,
        "data": {
            "metrics": latest.iter().map(|(key, (step, timestamp, value))| json!({
                "key": key, "value": metric_value(value), "timestamp": timestamp, "step": step,
            })).collect::<Vec<_>>(),
            "params": folded.params.iter().map(|(key, value)| json!({ "key": key, "value": value })).collect::<Vec<_>>(),
            "tags": tags.iter().map(|(key, value)| json!({ "key": key, "value": value })).collect::<Vec<_>>(),
        },
        "inputs": {},
    })
}
