//! Host conformance seams (`apps/norn-cell-do/conformance`), in `conformance`
//! builds only, served only when `NORN_STAGE=test` and
//! `NORN_CONFORMANCE=synthetic`, and only for the synthetic scope. The native
//! counterpart of `conformance/worker.mjs`:
//!
//! - a fixed request clock (`x-norn-conformance-now`), so receipts and heads
//!   are byte-comparable with the native reference;
//! - a hold inside the request's write transaction (`x-norn-conformance-hold`):
//!   the cell thread stops right after the n-th SQL statement, or at the end
//!   of the transaction's body before it commits, after printing the runner's
//!   marker to stderr (the host log the runner reads);
//! - the verified ledger head (`/__conformance/head`) and per-request
//!   statement counts (`/__conformance/stats`);
//! - the probe actor: the Rivet platform properties the cell relies on, one
//!   route each, over the same primitives the cell host uses.
//!
//! Nothing here changes what the cell decides: requests reach the same dispatch.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use async_trait::async_trait;
use norn_api::scope::CellScope;
use norn_cell::CellError;
use norn_cell::engine::{SqlEngine, SqlValue};
use rivetkit::action::Raw;
use rivetkit::{Actor, BindParam, ColumnValue, Ctx, Handler, Request, Response};
use serde_json::{Value, json};

use crate::actor::{CellActor, WakeNow, json_response, now_ms};
use crate::alarm::Alarm;
use crate::core::Call;
use crate::engine::{RivetEngine, StatementHook};

mod upgrade;

/// The probe actor's name in the registry.
pub const PROBE_ACTOR: &str = "probe";
const NOW_HEADER: &str = "x-norn-conformance-now";
const HOLD_HEADER: &str = "x-norn-conformance-hold";
const MARKER_HEADER: &str = "x-norn-conformance-marker";
const MARKER_LINE: &str = "norn-conformance-hold-reached";

/// This process, for the identity probes.
static ISOLATE: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| uuid::Uuid::new_v4().to_string());

/// Where a request asked to be held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hold {
    /// Right after the n-th statement inside the transaction.
    Statement { n: u64, ms: Option<u64> },
    /// At the end of the outermost transaction's body, before it commits.
    TurnEnd { ms: Option<u64> },
}

fn parse_hold(value: &str) -> Option<Hold> {
    let mut parts = value.split(':');
    let at = parts.next()?;
    let first = parts.next().map(str::parse::<u64>);
    let second = parts.next().map(str::parse::<u64>);
    match (at, first, second) {
        ("stmt", Some(Ok(n)), None) => Some(Hold::Statement { n, ms: None }),
        ("stmt", Some(Ok(n)), Some(Ok(ms))) => Some(Hold::Statement { n, ms: Some(ms) }),
        ("turn-end", None, None) => Some(Hold::TurnEnd { ms: None }),
        ("turn-end", Some(Ok(ms)), None) => Some(Hold::TurnEnd { ms: Some(ms) }),
        _ => None,
    }
}

fn marker_ok(marker: &str) -> bool {
    (16..=64).contains(&marker.len())
        && marker
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The per-request counts and hold, on the cell thread.
#[derive(Default)]
pub struct Seams {
    statements: Cell<u64>,
    in_transaction: Cell<u64>,
    transactions: Cell<u64>,
    at_begin: Cell<u64>,
    hold: RefCell<Option<Hold>>,
    marker: RefCell<Option<String>>,
    reached: Cell<bool>,
    last: RefCell<Value>,
}

impl Seams {
    fn reset(&self, hold: Option<Hold>, marker: Option<String>) {
        self.statements.set(0);
        self.in_transaction.set(0);
        self.transactions.set(0);
        self.reached.set(false);
        *self.hold.borrow_mut() = hold;
        *self.marker.borrow_mut() = marker;
    }

    fn finish(&self) -> (u64, Option<bool>) {
        let counts = (
            self.in_transaction.get(),
            self.hold.borrow().is_some().then(|| self.reached.get()),
        );
        *self.last.borrow_mut() = json!({"statements": self.statements.get(), "inTransaction": self.in_transaction.get(), "transactions": self.transactions.get()});
        *self.hold.borrow_mut() = None;
        *self.marker.borrow_mut() = None;
        counts
    }

    /// Print the runner's marker (before the hold starts), then hold.
    fn mark_and_hold(&self, ms: Option<u64>) {
        self.reached.set(true);
        if let Some(marker) = self.marker.borrow().as_deref() {
            let mut stderr = std::io::stderr().lock();
            // The marker is the runner's only evidence; a failed write leaves
            // the boundary untested, never killed.
            let _written = writeln!(stderr, "{MARKER_LINE} {marker}").and_then(|()| stderr.flush());
        }
        // Spin, as the Workers entry spins its runtime thread: the runner
        // finds the process that holds the cell by its CPU time.
        let started = std::time::Instant::now();
        while ms.is_none_or(|ms| started.elapsed() < Duration::from_millis(ms)) {
            std::hint::spin_loop();
        }
    }
}

impl StatementHook for Seams {
    fn begin(&self) {
        self.transactions
            .set(self.transactions.get().saturating_add(1));
        self.at_begin.set(self.in_transaction.get());
    }

    fn ran(&self, in_transaction: bool) {
        self.statements.set(self.statements.get().saturating_add(1));
        if !in_transaction {
            return;
        }
        let count = self.in_transaction.get().saturating_add(1);
        self.in_transaction.set(count);
        let hold = *self.hold.borrow();
        if let Some(Hold::Statement { n, ms }) = hold
            && n == count
            && !self.reached.get()
        {
            self.mark_and_hold(ms);
        }
    }

    fn before_commit(&self) {
        let hold = *self.hold.borrow();
        if let Some(Hold::TurnEnd { ms }) = hold
            && self.in_transaction.get() > self.at_begin.get()
            && !self.reached.get()
        {
            self.mark_and_hold(ms);
        }
    }
}

fn refuse(code: &str, message: &str) -> Response {
    crate::actor::refusal(
        403,
        code,
        "permission",
        message,
        "a conformance build serves only an opted-in test stage and its synthetic scope",
        false,
    )
}

fn header<'a>(req: &'a Request, name: &str) -> Option<&'a str> {
    req.headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
}

fn with_headers(response: &Response, extra: &[(&str, String)]) -> Response {
    let (status, mut headers, body) = response.to_parts();
    for (name, value) in extra {
        headers.insert((*name).to_owned(), value.clone());
    }
    Response::from_parts(status, headers, body).unwrap_or_else(|_error| Response::new(Vec::new()))
}

/// A cell request on a conformance host.
pub async fn serve_cell(
    actor: &Arc<CellActor>,
    ctx: &Ctx<CellActor>,
    req: &Request,
    scope: CellScope,
    mut call: Call,
) -> Response {
    if !crate::conformance_scope(&scope) {
        return refuse(
            "conformance.scope",
            "this conformance entry serves only its synthetic scope",
        );
    }
    let identity = json!({"instance": actor.instance, "isolate": *ISOLATE, "pid": std::process::id(), "constructedAt": actor.constructed_at});
    if call.path == "/__conformance/head" {
        if let Err(error) = actor.ready(ctx, &scope).await {
            return json_response(503, json!({"error": format!("{error:#}")}).to_string());
        }
        let head = actor
            .cell()
            .call(|slot| {
                slot.core.as_ref().map_or_else(
                    || Err("the cell is closed".to_owned()),
                    crate::core::CellCore::ledger_head,
                )
            })
            .await;
        return match head {
            Ok(Ok(head_json)) => {
                let mut document = identity;
                if let Some(fields) = document.as_object_mut() {
                    fields.insert(
                        "head".to_owned(),
                        serde_json::from_str(&head_json).unwrap_or(Value::Null),
                    );
                    fields.insert("headJson".to_owned(), Value::String(head_json));
                }
                json_response(200, document.to_string())
            }
            Ok(Err(error)) => json_response(500, json!({"error": error}).to_string()),
            Err(error) => json_response(500, json!({"error": error.to_string()}).to_string()),
        };
    }
    if call.path == "/__conformance/stats" {
        let last = actor
            .cell()
            .call(|slot| slot.seams.as_ref().map(|seams| seams.last.borrow().clone()))
            .await;
        let mut document = identity;
        if let (Ok(Some(Value::Object(counts))), Some(fields)) = (last, document.as_object_mut()) {
            fields.extend(counts);
        }
        return json_response(200, document.to_string());
    }
    if let Some(now) = header(req, NOW_HEADER).and_then(|value| value.parse::<u64>().ok()) {
        call.now_ms = now;
    }
    let hold = header(req, HOLD_HEADER).and_then(parse_hold);
    let marker = header(req, MARKER_HEADER)
        .filter(|marker| marker_ok(marker))
        .map(str::to_owned);
    let armed = actor
        .cell()
        .call(move |slot| {
            slot.seams
                .as_ref()
                .map(|seams| seams.reset(hold, marker))
                .is_some()
        })
        .await;
    if !matches!(armed, Ok(true)) {
        return json_response(
            500,
            json!({"error": "the conformance seams are not installed"}).to_string(),
        );
    }
    let response = actor.fetch(ctx, scope, call).await;
    let counts = actor
        .cell()
        .call(|slot| slot.seams.as_ref().map(|seams| seams.finish()))
        .await;
    let mut extra = vec![("x-norn-conformance-instance", actor.instance.clone())];
    if let Ok(Some((statements, reached))) = counts {
        extra.push(("x-norn-conformance-statements", statements.to_string()));
        if let Some(reached) = reached {
            extra.push(("x-norn-conformance-hold-reached", reached.to_string()));
        }
    }
    with_headers(&response, &extra)
}

// ------------------------------------------------------------------ probes

/// The probe actor: the platform properties the cell relies on.
pub struct ProbeActor {
    instance: String,
    alarm: Alarm,
    created: AtomicBool,
}

#[async_trait]
impl Actor for ProbeActor {
    type Input = ();
    type Actions = (WakeNow,);
    type Events = ();
    type ConnParams = ();
    type Action = Raw;

    const CONCURRENT_HTTP_CALLBACKS: bool = true;

    async fn create(_ctx: &Ctx<Self>) -> Result<Self> {
        Ok(Self {
            instance: uuid::Uuid::new_v4().to_string(),
            alarm: Alarm::default(),
            created: AtomicBool::new(false),
        })
    }

    async fn on_start(self: Arc<Self>, ctx: Ctx<Self>) -> Result<()> {
        if let Err(error) = self.alarm.resync(&ctx).await {
            tracing::error!(error = %format!("{error:#}"), "probe alarm resync failed");
        }
        Ok(())
    }

    async fn on_fetch(self: Arc<Self>, ctx: Ctx<Self>, req: Request) -> Result<Response> {
        if !crate::config::get().conformance {
            return Ok(refuse(
                "conformance.disabled",
                "this conformance entry is not enabled",
            ));
        }
        let route = req
            .uri()
            .path()
            .strip_prefix("/__probe/")
            .unwrap_or_default()
            .to_owned();
        let params: HashMap<String, String> = req
            .uri()
            .query()
            .map(|query| {
                query
                    .split('&')
                    .filter_map(|pair| pair.split_once('='))
                    .map(|(name, value)| (name.to_owned(), value.to_owned()))
                    .collect()
            })
            .unwrap_or_default();
        // The upgrade probe loads a fixture cell into a fresh actor database,
        // so it runs before the probe's own tables exist.
        if route == "engine-upgrade" {
            return Ok(engine_upgrade(&ctx, &params).await);
        }
        let answered = match self.ensure(&ctx).await {
            Ok(()) => self.route(&ctx, &route, &params).await,
            Err(error) => Err(error),
        };
        Ok(match answered {
            Ok(Some(mut value)) => {
                if let Some(fields) = value.as_object_mut() {
                    fields.insert("instance".to_owned(), Value::String(self.instance.clone()));
                }
                json_response(200, value.to_string())
            }
            Ok(None) => json_response(404, json!({"instance": self.instance, "error": format!("no probe {route}")}).to_string()),
            Err(error) => json_response(500, json!({"instance": self.instance, "error": format!("{error:#}").chars().take(500).collect::<String>()}).to_string()),
        })
    }
}

impl Handler<WakeNow> for ProbeActor {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, _action: WakeNow) -> Result<()> {
        let now = i64::try_from(now_ms()).unwrap_or(i64::MAX);
        let fired = ctx.clone();
        self.alarm
            .fire(&ctx, now, |delivery| async move {
                let fail = one_integer(&fired, "SELECT v FROM kv WHERE k = 'fail'", vec![]).await?.unwrap_or(0);
                if delivery.retry_count < fail {
                    log(&fired, "alarm-failed", Some(json!({"retryCount": delivery.retry_count}))).await?;
                    return Err(anyhow!("conformance: alarm handler failure"));
                }
                log(&fired, "alarm", Some(json!({"retryCount": delivery.retry_count, "isRetry": delivery.retry_count > 0}))).await
            })
            .await
    }
}

fn json_value(value: &ColumnValue) -> Value {
    match value {
        ColumnValue::Null => Value::Null,
        ColumnValue::Integer(value) => json!(value),
        ColumnValue::Float(value) => json!(value),
        ColumnValue::Text(value) => json!(value),
        ColumnValue::Blob(value) => json!(format!("blob:{}", value.len())),
    }
}

async fn rows<A: Actor>(
    ctx: &Ctx<A>,
    sql: &str,
    params: Vec<BindParam>,
) -> Result<Vec<Vec<Value>>> {
    let result = ctx.sql().execute(sql, Some(params)).await?;
    Ok(result
        .rows
        .iter()
        .map(|row| row.iter().map(json_value).collect())
        .collect())
}

async fn one_integer<A: Actor>(
    ctx: &Ctx<A>,
    sql: &str,
    params: Vec<BindParam>,
) -> Result<Option<i64>> {
    let result = ctx.sql().execute(sql, Some(params)).await?;
    Ok(match result.rows.first().and_then(|row| row.first()) {
        Some(ColumnValue::Integer(value)) => Some(*value),
        Some(
            ColumnValue::Null | ColumnValue::Float(_) | ColumnValue::Text(_) | ColumnValue::Blob(_),
        )
        | None => None,
    })
}

async fn count<A: Actor>(ctx: &Ctx<A>, kind: &str) -> Result<i64> {
    Ok(one_integer(
        ctx,
        "SELECT count(*) FROM log WHERE kind = ?",
        vec![BindParam::Text(kind.to_owned())],
    )
    .await?
    .unwrap_or(0))
}

async fn log<A: Actor>(ctx: &Ctx<A>, kind: &str, detail: Option<Value>) -> Result<()> {
    ctx.sql()
        .run(
            "INSERT INTO log (kind, at, detail) VALUES (?, ?, ?)",
            Some(vec![
                BindParam::Text(kind.to_owned()),
                BindParam::Integer(i64::try_from(now_ms()).unwrap_or(i64::MAX)),
                detail.map_or(BindParam::Null, |detail| {
                    BindParam::Text(detail.to_string())
                }),
            ]),
        )
        .await?;
    Ok(())
}

fn log_params(kind: &str, detail: &Value) -> Vec<BindParam> {
    vec![
        BindParam::Text(kind.to_owned()),
        BindParam::Integer(i64::try_from(now_ms()).unwrap_or(i64::MAX)),
        BindParam::Text(detail.to_string()),
    ]
}

const LOG_INSERT: &str = "INSERT INTO log (kind, at, detail) VALUES (?, ?, ?)";

/// Run `f` over a fresh [`RivetEngine`] on its own thread (never a runtime
/// worker, which the engine blocks on).
async fn on_engine<T: Send + 'static>(
    ctx: &Ctx<ProbeActor>,
    f: impl FnOnce(&RivetEngine) -> T + Send + 'static,
) -> Result<T> {
    let db = ctx.sql().clone();
    let runtime = tokio::runtime::Handle::current();
    let (reply, answer) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("norn-probe".to_owned())
        .spawn(move || {
            let engine = RivetEngine::new(db, runtime);
            let _sent = reply.send(f(&engine));
        })?;
    answer.await.context("the probe thread panicked")
}

/// ENGINE-UPGRADE-1 on this actor's database ([`upgrade`]); `case` is `v1`,
/// `v2`, `downgrade` or `unknown`, on a fresh `object`.
async fn engine_upgrade(ctx: &Ctx<ProbeActor>, params: &HashMap<String, String>) -> Response {
    let Some(case) = params
        .get("case")
        .and_then(|name| upgrade::Case::parse(name))
    else {
        return json_response(
            400,
            json!({"error": "case is v1, v2, downgrade or unknown"}).to_string(),
        );
    };
    let db = ctx.sql().clone();
    let runtime = tokio::runtime::Handle::current();
    let (reply, answer) = tokio::sync::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("norn-probe-upgrade".to_owned())
        .spawn(move || {
            let fresh = || RivetEngine::new(db.clone(), runtime.clone());
            let _sent = reply.send(upgrade::run(case, &fresh));
        });
    let answered = match spawned {
        Ok(_) => answer
            .await
            .map_err(|_| "the probe thread panicked".to_owned()),
        Err(error) => Err(error.to_string()),
    };
    match answered {
        Ok(Ok(value)) => json_response(200, value.to_string()),
        Ok(Err(error)) => json_response(500, json!({"error": error.to_string()}).to_string()),
        Err(error) => json_response(500, json!({"error": error}).to_string()),
    }
}

/// `crates/norn-cell/examples/workers_sql.rs`'s adapter laws over the Rivet
/// engine: commit, a typed refusal rolled back, nesting and SQL transaction
/// control refused, exact values, blobs, duplicate columns.
fn exercise(engine: &RivetEngine) -> Result<String, CellError> {
    engine.execute_batch(
        "CREATE TABLE IF NOT EXISTS probe (id INTEGER PRIMARY KEY, exact TEXT, bytes BLOB)",
    )?;
    engine.transaction(|| {
        engine.execute(
            "INSERT OR IGNORE INTO probe VALUES (?1,?2,?3)",
            &[
                SqlValue::Integer(1),
                SqlValue::Text("9007199254740993.0001".into()),
                SqlValue::Blob(vec![0, 255]),
            ],
        )
    })?;
    let refusal = engine.transaction(|| {
        engine.execute("INSERT INTO probe VALUES (2,'refused',NULL)", &[])?;
        Err::<(), _>(CellError::Engine("typed refusal".into()))
    });
    if !matches!(refusal, Err(CellError::Engine(ref e)) if e == "typed refusal") {
        return Err(CellError::Engine("typed refusal lost".into()));
    }
    let nested = engine.transaction(|| {
        engine.execute("INSERT INTO probe VALUES (3,'nested',NULL)", &[])?;
        engine.transaction(|| Ok::<_, CellError>(()))
    });
    if nested.is_ok() || engine.execute("COMMIT", &[]).is_ok() {
        return Err(CellError::Engine("transaction control accepted".into()));
    }
    let found = engine.query("SELECT id,exact,bytes FROM probe ORDER BY id", &[])?;
    let expected = vec![vec![
        SqlValue::Integer(1),
        SqlValue::Text("9007199254740993.0001".into()),
        SqlValue::Blob(vec![0, 255]),
    ]];
    if found != expected {
        return Err(CellError::Engine(format!(
            "rollback/state mismatch: {found:?}"
        )));
    }
    if engine.query("SELECT 1 AS x,2 AS x", &[])?
        != vec![vec![SqlValue::Integer(1), SqlValue::Integer(2)]]
    {
        return Err(CellError::Engine("column order lost".into()));
    }
    Ok("commit,typed-refusal,nested,sql-control,exact,blob,columns:pass".into())
}

impl ProbeActor {
    async fn ensure(&self, ctx: &Ctx<Self>) -> Result<()> {
        if !self.created.load(Ordering::Acquire) {
            ctx.sql()
                .exec(
                    "CREATE TABLE IF NOT EXISTS log (seq INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT NOT NULL, at INTEGER, detail TEXT);
                     CREATE TABLE IF NOT EXISTS kv (k TEXT PRIMARY KEY, v)",
                )
                .await?;
            self.created.store(true, Ordering::Release);
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one route per platform property, as the JS probe object"
    )]
    async fn route(
        &self,
        ctx: &Ctx<Self>,
        route: &str,
        params: &HashMap<String, String>,
    ) -> Result<Option<Value>> {
        let param = |name: &str, fallback: &str| {
            params
                .get(name)
                .cloned()
                .unwrap_or_else(|| fallback.to_owned())
        };
        let number = |name: &str, fallback: i64| {
            params
                .get(name)
                .and_then(|value| value.parse::<i64>().ok())
                .unwrap_or(fallback)
        };
        Ok(Some(match route {
            "adapter" => {
                let result = on_engine(ctx, |engine| {
                    exercise(engine).unwrap_or_else(|error| format!("fail: {error}"))
                })
                .await?;
                json!({"result": result})
            }
            "tx-throw" => {
                // A typed error from the body rolls back and comes back unchanged.
                let before = count(ctx, "tx-throw").await?;
                let thrown = on_engine(ctx, |engine| {
                    engine.transaction(|| {
                        engine.execute(
                            LOG_INSERT,
                            &[
                                SqlValue::from("tx-throw"),
                                SqlValue::Integer(0),
                                SqlValue::Null,
                            ],
                        )?;
                        Err::<(), _>(CellError::Engine("norn:rollback".into()))
                    })
                })
                .await?;
                let after = count(ctx, "tx-throw").await?;
                json!({"rolledBack": after == before, "rethrown": matches!(thrown, Err(CellError::Engine(ref e)) if e == "norn:rollback"), "thrownType": "CellError"})
            }
            "tx-nested" => {
                // Norn refuses nesting (the adapter probe); this measures what
                // the platform does with a second manual transaction.
                let before = count(ctx, "outer").await?;
                let tx = ctx
                    .sql()
                    .begin_transaction(Some(Duration::from_secs(30)))
                    .await?;
                tx.execute(LOG_INSERT, Some(log_params("outer", &Value::Null)))
                    .await?;
                // RivetKit's transaction coordinator queues a second manual
                // transaction behind the open one, so a nested begin would
                // wait out the outer transaction's own timeout; a begin that
                // is given up on still poisons the queue. Norn refuses nesting
                // before the platform (the adapter probe), so it is not tried.
                let inner = "not attempted: RivetKit queues a second manual transaction behind the open one".to_owned();
                tx.commit().await?;
                let added = count(ctx, "outer").await?.saturating_sub(before);
                json!({"inner": inner, "outerKept": added == 1, "added": added})
            }
            "turn" => {
                let n = number("n", 50);
                let tag = param("tag", "turn");
                let ms = u64::try_from(number("ms", 300)).unwrap_or(0);
                let tx = ctx
                    .sql()
                    .begin_transaction(Some(Duration::from_secs(60)))
                    .await?;
                for i in 0..n {
                    tx.execute(LOG_INSERT, Some(log_params(&tag, &json!(i))))
                        .await?;
                    if i == n / 2 {
                        tokio::time::sleep(Duration::from_millis(ms)).await;
                    }
                }
                tx.commit().await?;
                json!({"wrote": n})
            }
            "count" => json!({"count": count(ctx, &param("tag", "turn")).await?}),
            "clock" => {
                let first = now_ms();
                let started = std::time::Instant::now();
                tokio::time::sleep(Duration::from_millis(
                    u64::try_from(number("ms", 50)).unwrap_or(50),
                ))
                .await;
                json!({"frozen": now_ms() == first, "dateAdvancedMs": now_ms().saturating_sub(first), "perfAdvancedMs": started.elapsed().as_secs_f64() * 1000.0})
            }
            "sqlite" => self.sqlite(ctx).await?,
            "size" => {
                let bytes = number("bytes", 1_048_576);
                let stored = ctx
                    .sql()
                    .run(
                        "INSERT OR REPLACE INTO kv VALUES ('big', zeroblob(?))",
                        Some(vec![BindParam::Integer(bytes)]),
                    )
                    .await;
                match stored {
                    Ok(_) => {
                        let length =
                            one_integer(ctx, "SELECT length(v) FROM kv WHERE k = 'big'", vec![])
                                .await?;
                        ctx.sql()
                            .run("DELETE FROM kv WHERE k = 'big'", None)
                            .await?;
                        json!({"bytes": bytes, "accepted": true, "stored": length})
                    }
                    Err(error) => {
                        json!({"bytes": bytes, "accepted": false, "error": format!("{error:#}").chars().take(300).collect::<String>()})
                    }
                }
            }
            "alarm-set" => {
                let at = i64::try_from(now_ms())
                    .unwrap_or(i64::MAX)
                    .saturating_add(number("in", 2000));
                self.alarm.set(ctx, at).await?;
                log(ctx, "alarm-set", Some(json!({"at": at}))).await?;
                ctx.sql()
                    .run(
                        "INSERT OR REPLACE INTO kv VALUES ('fail', ?)",
                        Some(vec![BindParam::Integer(number("fail", 0))]),
                    )
                    .await?;
                ctx.sql()
                    .run("DELETE FROM log WHERE kind = 'alarm-failed'", None)
                    .await?;
                json!({"at": at})
            }
            "alarm-rollback" => {
                // An alarm set inside a transaction that then fails must not survive.
                self.alarm.prepare(ctx.sql()).await?;
                let tx = ctx
                    .sql()
                    .begin_transaction(Some(Duration::from_secs(30)))
                    .await?;
                tx.execute(
                    LOG_INSERT,
                    Some(log_params("alarm-rolled-back", &Value::Null)),
                )
                .await?;
                self.alarm
                    .set_in(
                        &tx,
                        i64::try_from(now_ms())
                            .unwrap_or(i64::MAX)
                            .saturating_add(3_600_000),
                    )
                    .await?;
                tx.rollback().await?;
                json!({"alarm": self.alarm.get(ctx).await?, "logged": count(ctx, "alarm-rolled-back").await?})
            }
            "alarm-state" => json!({
                "alarm": self.alarm.get(ctx).await?,
                "fired": rows(ctx, "SELECT at, detail FROM log WHERE kind = 'alarm' ORDER BY seq", vec![]).await?,
                "failed": rows(ctx, "SELECT at, detail FROM log WHERE kind = 'alarm-failed' ORDER BY seq", vec![]).await?,
            }),
            "wait-until" => {
                let n = number("n", 15);
                let every = u64::try_from(number("every", 1000)).unwrap_or(1000);
                let tag = param("tag", "bg");
                let worker = ctx.clone();
                let background = tag.clone();
                ctx.wait_until(async move {
                    for i in 0..n {
                        tokio::time::sleep(Duration::from_millis(every)).await;
                        let turn = async {
                            let tx = worker.sql().begin_transaction(Some(Duration::from_secs(30))).await?;
                            tx.execute(LOG_INSERT, Some(log_params(&background, &json!(i)))).await?;
                            tx.execute(LOG_INSERT, Some(log_params(&format!("{background}-pair"), &json!(i)))).await?;
                            tx.commit().await
                        };
                        if let Err(error) = turn.await {
                            tracing::error!(error = %format!("{error:#}"), "background turn failed");
                        }
                    }
                });
                json!({"started": n, "written": count(ctx, &tag).await?})
            }
            "pairs" => {
                let tag = param("tag", "bg");
                json!({"count": count(ctx, &tag).await?, "pairs": count(ctx, &format!("{tag}-pair")).await?})
            }
            "identity" => {
                log(ctx, "identity", Some(json!(self.instance))).await?;
                json!({"isolate": *ISOLATE, "pid": std::process::id(), "writes": count(ctx, "identity").await?})
            }
            _ => return Ok(None),
        }))
    }

    async fn sqlite(&self, ctx: &Ctx<Self>) -> Result<Value> {
        let first = |rows: Vec<Vec<Value>>| rows.into_iter().next().unwrap_or_default();
        let try_one = |sql: String| async move {
            match rows(ctx, &sql, vec![]).await {
                Ok(found) => found
                    .into_iter()
                    .next()
                    .and_then(|row| row.into_iter().next())
                    .unwrap_or(Value::Null),
                Err(error) => {
                    Value::String(format!("error: {error:#}").chars().take(120).collect())
                }
            }
        };
        ctx.sql().exec("CREATE TABLE IF NOT EXISTS aff (i INTEGER, t TEXT, r REAL, n NUMERIC, b BLOB, x); DELETE FROM aff").await?;
        ctx.sql()
            .run(
                "INSERT INTO aff VALUES ('42', 42, '1.5', '9007199254740993', x'00ff', '7')",
                None,
            )
            .await?;
        let types = first(
            rows(
                ctx,
                "SELECT typeof(i), typeof(t), typeof(r), typeof(n), typeof(b), typeof(x) FROM aff",
                vec![],
            )
            .await?,
        );
        let n = first(rows(ctx, "SELECT CAST(n AS TEXT) FROM aff", vec![]).await?)
            .into_iter()
            .next()
            .unwrap_or(Value::Null);
        let max_int = first(rows(ctx, "SELECT CAST(9223372036854775807 AS TEXT)", vec![]).await?)
            .into_iter()
            .next()
            .unwrap_or(Value::Null);
        let big = match ctx
            .sql()
            .execute(
                "SELECT ?",
                Some(vec![BindParam::Integer(9_007_199_254_740_993)]),
            )
            .await
        {
            Ok(result) => {
                result
                    .rows
                    .first()
                    .and_then(|row| row.first())
                    .map_or(Value::Null, |value| match value {
                        ColumnValue::Integer(value) => Value::String(value.to_string()),
                        other @ (ColumnValue::Null
                        | ColumnValue::Float(_)
                        | ColumnValue::Text(_)
                        | ColumnValue::Blob(_)) => json_value(other),
                    })
            }
            Err(error) => Value::String(format!("refused: {error:#}").chars().take(120).collect()),
        };
        let variables = |count: usize| async move {
            let sql = format!("SELECT {}", vec!["?"; count].join(","));
            let bindings = (0..count)
                .map(|i| BindParam::Integer(i64::try_from(i).unwrap_or(0)))
                .collect();
            ctx.sql().execute(sql, Some(bindings)).await.is_ok()
        };
        let statement = ctx
            .sql()
            .execute(format!("SELECT '{}'", "x".repeat(100_000)), None)
            .await
            .is_ok();
        let size = one_integer(
            ctx,
            "SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()",
            vec![],
        )
        .await
        .ok()
        .flatten();
        let strict =
            try_one("CREATE TABLE IF NOT EXISTS strict_probe (a INTEGER) STRICT".to_owned()).await;
        Ok(json!({
            "version": try_one("SELECT sqlite_version()".to_owned()).await,
            "affinity": {"types": types, "n": n, "maxInt": max_int, "bigBinding": big},
            "variables100": variables(100).await,
            "variables1000": variables(1000).await,
            "statement100k": statement,
            "json": try_one("SELECT json_extract('{\"a\":[1,2]}', '$.a[1]')".to_owned()).await,
            "strict": if strict.is_null() { Value::String("ok".to_owned()) } else { strict },
            "foreignKeys": try_one("PRAGMA foreign_keys".to_owned()).await,
            "databaseSize": size,
        }))
    }
}
