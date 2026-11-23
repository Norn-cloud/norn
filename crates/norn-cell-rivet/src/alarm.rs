//! The actor's one durable alarm, with Durable Object semantics, and the
//! scope a serving cell keeps for it.
//!
//! The alarm is a row in the actor's own database (`_norn_host_alarm`),
//! written with ordinary SQL: written inside a transaction, it commits or
//! rolls back with it. A `RivetKit` schedule event (`norn.wake`) is only the
//! trigger: when it fires the row decides whether anything is due, and every
//! actor start re-arms the trigger from the row, so a trigger lost between
//! the row's write and its arming (a crash) is restored by the restart that
//! crash causes.
//!
//! Delivery is at least once. Before the handler runs, the row moves to a
//! retry time with backoff and a retry count; a handler that fails leaves it
//! there, so it fires again. A handler that succeeds without setting a new
//! alarm deletes it. The cell's wake (the rules of `wake.mjs`, in [`crate::actor`]) adds its own recovery
//! wake on top, as it does on a Durable Object.

use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context as _, Result};
use rivetkit::{Actor, BindParam, ColumnValue, Ctx, SqliteDb, SqliteTransaction};
use tokio::sync::Mutex;

/// The scheduled action that fires the alarm.
pub const WAKE_ACTION: &str = "norn.wake";
/// A trigger that fires this early is still on time. Zero: an alarm never
/// runs before its time; an early trigger re-arms for the row's time.
const EARLY_MS: i64 = 0;
/// First retry delay after a failed handler; doubles per retry.
const RETRY_FIRST_MS: i64 = 2_000;
/// Longest retry delay.
const RETRY_MAX_MS: i64 = 60_000;

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS _norn_host_alarm (id INTEGER PRIMARY KEY CHECK (id = 1), at INTEGER NOT NULL, retries INTEGER NOT NULL, generation INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS _norn_host_scope (id INTEGER PRIMARY KEY CHECK (id = 1), scope TEXT NOT NULL)";

/// A delivery's retry information, as a Durable Object's `alarm(info)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Delivery {
    /// Failed deliveries before this one.
    pub retry_count: i64,
}

#[derive(Debug, Clone, Copy)]
struct Row {
    at: i64,
    retries: i64,
    generation: i64,
}

/// The alarm of one actor. Reads and writes of the row are serialized here.
#[derive(Debug, Default)]
pub struct Alarm {
    lock: Mutex<()>,
    created: AtomicBool,
}

fn integer(value: Option<&ColumnValue>) -> Option<i64> {
    match value {
        Some(ColumnValue::Integer(value)) => Some(*value),
        Some(
            ColumnValue::Null | ColumnValue::Float(_) | ColumnValue::Text(_) | ColumnValue::Blob(_),
        )
        | None => None,
    }
}

impl Alarm {
    async fn ensure(&self, db: &SqliteDb) -> Result<()> {
        if !self.created.load(Ordering::Acquire) {
            db.exec(SCHEMA)
                .await
                .context("create the host alarm tables")?;
            self.created.store(true, Ordering::Release);
        }
        Ok(())
    }

    /// Whether the tables exist; reads never create them, so an actor whose
    /// cell is vacant keeps no trace.
    async fn exists(&self, db: &SqliteDb) -> Result<bool> {
        if self.created.load(Ordering::Acquire) {
            return Ok(true);
        }
        let found = db
            .query(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name IN ('_norn_host_alarm', '_norn_host_scope')",
                None,
            )
            .await?;
        let both = integer(found.rows.first().and_then(|row| row.first())) == Some(2);
        if both {
            self.created.store(true, Ordering::Release);
        }
        Ok(both)
    }

    async fn row(&self, db: &SqliteDb) -> Result<Option<Row>> {
        if !self.exists(db).await? {
            return Ok(None);
        }
        let result = db
            .query(
                "SELECT at, retries, generation FROM _norn_host_alarm WHERE id = 1",
                None,
            )
            .await?;
        Ok(result.rows.first().and_then(|row| {
            Some(Row {
                at: integer(row.first())?,
                retries: integer(row.get(1))?,
                generation: integer(row.get(2))?,
            })
        }))
    }

    async fn write(db: &SqliteDb, at: i64, retries: i64) -> Result<i64> {
        let result = db
            .execute(
                "INSERT INTO _norn_host_alarm (id, at, retries, generation) VALUES (1, ?1, ?2, 1)
                 ON CONFLICT (id) DO UPDATE SET at = excluded.at, retries = excluded.retries, generation = generation + 1
                 RETURNING generation",
                Some(vec![BindParam::Integer(at), BindParam::Integer(retries)]),
            )
            .await?;
        integer(result.rows.first().and_then(|row| row.first())).context("the alarm generation")
    }

    /// When the alarm fires next (Unix milliseconds), if set.
    ///
    /// # Errors
    /// Storage errors.
    pub async fn get<A: Actor>(&self, ctx: &Ctx<A>) -> Result<Option<i64>> {
        let _held = self.lock.lock().await;
        Ok(self.row(ctx.sql()).await?.map(|row| row.at))
    }

    /// Set the alarm to `at`; a passed time fires at once.
    ///
    /// # Errors
    /// Storage or scheduling errors.
    pub async fn set<A: Actor>(&self, ctx: &Ctx<A>, at: i64) -> Result<()> {
        let _held = self.lock.lock().await;
        self.ensure(ctx.sql()).await?;
        Self::write(ctx.sql(), at, 0).await?;
        arm(ctx, at).await
    }

    /// Create the alarm's tables before a transaction that sets it: statements
    /// outside an open transaction wait for it to end.
    ///
    /// # Errors
    /// Storage errors.
    pub async fn prepare(&self, db: &SqliteDb) -> Result<()> {
        self.ensure(db).await
    }

    /// Set the alarm to `at` inside `tx` (after [`Self::prepare`]): it exists
    /// only if `tx` commits. The trigger is armed by [`Self::resync`] once it
    /// has.
    ///
    /// # Errors
    /// Storage errors.
    pub async fn set_in(&self, tx: &SqliteTransaction, at: i64) -> Result<()> {
        tx.execute(
            "INSERT INTO _norn_host_alarm (id, at, retries, generation) VALUES (1, ?1, 0, 1)
             ON CONFLICT (id) DO UPDATE SET at = excluded.at, retries = 0, generation = generation + 1",
            Some(vec![BindParam::Integer(at)]),
        )
        .await?;
        Ok(())
    }

    /// Remove the alarm.
    ///
    /// # Errors
    /// Storage or scheduling errors.
    pub async fn delete<A: Actor>(&self, ctx: &Ctx<A>) -> Result<()> {
        let _held = self.lock.lock().await;
        if !self.exists(ctx.sql()).await? {
            return Ok(());
        }
        ctx.sql()
            .run("DELETE FROM _norn_host_alarm WHERE id = 1", None)
            .await?;
        disarm(ctx, None).await
    }

    /// Re-arm the trigger from the row: on every actor start, and after a
    /// transaction that set the alarm committed.
    ///
    /// # Errors
    /// Storage or scheduling errors.
    pub async fn resync<A: Actor>(&self, ctx: &Ctx<A>) -> Result<()> {
        let _held = self.lock.lock().await;
        match self.row(ctx.sql()).await? {
            Some(row) => arm(ctx, row.at).await,
            None => Ok(()),
        }
    }

    /// The trigger fired at `now`: run `handler` when the alarm is due.
    ///
    /// # Errors
    /// Storage or scheduling errors, or the handler's (the alarm stays set
    /// at its retry time).
    pub async fn fire<A: Actor, F, Fut>(&self, ctx: &Ctx<A>, now: i64, handler: F) -> Result<()>
    where
        F: FnOnce(Delivery) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        let (claimed, delivery) = {
            let _held = self.lock.lock().await;
            let Some(row) = self.row(ctx.sql()).await? else {
                return Ok(());
            };
            if row.at > now.saturating_add(EARLY_MS) {
                // An early or stale trigger: the row's time stands.
                return arm(ctx, row.at).await;
            }
            let shift = u32::try_from(row.retries.clamp(0, 16)).unwrap_or(16);
            let backoff = RETRY_FIRST_MS
                .saturating_mul(2_i64.saturating_pow(shift))
                .min(RETRY_MAX_MS);
            let retry_at = now.saturating_add(backoff);
            let generation =
                Self::write(ctx.sql(), retry_at, row.retries.saturating_add(1)).await?;
            arm(ctx, retry_at).await?;
            (
                generation,
                Delivery {
                    retry_count: row.retries,
                },
            )
        };
        handler(delivery).await?;
        let _held = self.lock.lock().await;
        if self
            .row(ctx.sql())
            .await?
            .is_some_and(|row| row.generation == claimed)
        {
            ctx.sql()
                .run("DELETE FROM _norn_host_alarm WHERE id = 1", None)
                .await?;
            disarm(ctx, None).await?;
        }
        Ok(())
    }

    /// The scope a serving cell keeps for its alarm, if any.
    ///
    /// # Errors
    /// Storage errors.
    pub async fn scope<A: Actor>(&self, ctx: &Ctx<A>) -> Result<Option<String>> {
        if !self.exists(ctx.sql()).await? {
            return Ok(None);
        }
        let result = ctx
            .sql()
            .query("SELECT scope FROM _norn_host_scope WHERE id = 1", None)
            .await?;
        Ok(match result.rows.first().and_then(|row| row.first()) {
            Some(ColumnValue::Text(scope)) => Some(scope.clone()),
            Some(
                ColumnValue::Null
                | ColumnValue::Integer(_)
                | ColumnValue::Float(_)
                | ColumnValue::Blob(_),
            )
            | None => None,
        })
    }

    /// Keep `scope` (JSON) for alarms.
    ///
    /// # Errors
    /// Storage errors.
    pub async fn keep_scope<A: Actor>(&self, ctx: &Ctx<A>, scope: &str) -> Result<()> {
        self.ensure(ctx.sql()).await?;
        ctx.sql()
            .run(
                "INSERT INTO _norn_host_scope (id, scope) VALUES (1, ?1) ON CONFLICT (id) DO UPDATE SET scope = excluded.scope",
                Some(vec![BindParam::Text(scope.to_owned())]),
            )
            .await?;
        Ok(())
    }

    /// Forget a kept scope.
    ///
    /// # Errors
    /// Storage errors.
    pub async fn forget_scope<A: Actor>(&self, ctx: &Ctx<A>) -> Result<()> {
        if !self.exists(ctx.sql()).await? {
            return Ok(());
        }
        ctx.sql()
            .run("DELETE FROM _norn_host_scope WHERE id = 1", None)
            .await?;
        Ok(())
    }
}

/// Arm the trigger at `at`, then cancel every other one: an extra trigger
/// finds the row's time and re-arms, a missing one would lose the alarm.
async fn arm<A: Actor>(ctx: &Ctx<A>, at: i64) -> Result<()> {
    let id = ctx
        .schedule()
        .at(at, WAKE_ACTION, &[])
        .await
        .context("arm the alarm trigger")?;
    disarm(ctx, Some(&id)).await
}

/// Cancel every trigger but `keep`.
async fn disarm<A: Actor>(ctx: &Ctx<A>, keep: Option<&str>) -> Result<()> {
    for event in ctx.schedule().list().await? {
        if event.action == WAKE_ACTION && Some(event.id.as_str()) != keep {
            ctx.schedule().cancel(&event.id).await?;
        }
    }
    Ok(())
}
