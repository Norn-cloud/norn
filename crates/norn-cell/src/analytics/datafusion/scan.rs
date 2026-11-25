//! D137: how the adapter runs `DataFusion`. A plan reads exactly the files a
//! catalog snapshot (or a landing attempt) names, each confined to the
//! tenant's private directory and never a link, and streams its batches
//! (`execute_stream`) to a consumer one at a time: each batch is checked
//! against the scan's byte budget and deadline before it is used, so no plan
//! materializes more than its share of memory. Operators that need memory
//! (sorts, joins) reserve it from the engine's fair spilling pool and spill
//! to the tenant's capped spill directory. Arrow stays inside this module and
//! its siblings.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use datafusion::arrow::record_batch::RecordBatch;
use datafusion::common::Column;
use datafusion::execution::runtime_env::RuntimeEnv;
use datafusion::logical_expr::{Expr, lit};
use datafusion::prelude::{DataFrame, ParquetReadOptions, SessionConfig, SessionContext};
use futures::StreamExt as _;

use super::catalog::{self, Catalog};
use super::{EXHAUSTED, Outcome, engine_error, refusal};

/// Rows per batch a plan yields: small enough that one batch of the widest
/// admitted row stays far under any memory bound.
const BATCH_ROWS: usize = 1024;
/// The longest a scan may run.
pub(super) const SCAN_DEADLINE: Duration = Duration::from_secs(300);

/// One tenant's directory, catalog and bounded `DataFusion` runtime.
pub(super) struct Tenant {
    /// `t/<tenant>/`.
    pub dir: PathBuf,
    /// The catalog, the one publication point.
    pub catalog: Catalog,
    pub(super) runtime: Arc<RuntimeEnv>,
    /// Objects whose digest this process has verified (N8): immutable,
    /// sealed and private, so a verified object stays verified.
    pub(super) verified: Mutex<BTreeSet<(PathBuf, String)>>,
}

impl Tenant {
    /// The tenant's immutable objects.
    pub(super) fn objects(&self) -> PathBuf {
        self.dir.join("objects")
    }

    /// The tenant's private staging directory for files not yet installed.
    pub(super) fn staging(&self) -> PathBuf {
        self.dir.join("staging")
    }

    /// Fail closed on a tampered or truncated object: its digest is checked
    /// once per process before it is read.
    pub(super) fn verify(&self, path: &std::path::Path, sha256: &str) -> Outcome<()> {
        let key = (path.to_path_buf(), sha256.to_owned());
        let mut verified = self.verified.lock().unwrap_or_else(PoisonError::into_inner);
        if verified.contains(&key) {
            return Ok(());
        }
        catalog::verify_object(path, sha256)?;
        verified.insert(key);
        Ok(())
    }
}

/// A column by its exact name: dotted, mixed-case or reserved names included.
pub(super) fn col(name: &str) -> Expr {
    Expr::Column(Column::new_unqualified(name))
}

/// `name` is one of `rows`.
pub(super) fn row_in(name: &str, rows: &[u64]) -> Expr {
    col(name).in_list(rows.iter().map(|row| lit(*row)).collect(), false)
}

/// What a plan reads and in what order.
pub(super) enum Plan {
    /// Some files, filtered, projected, ordered by the keys (ascending; the
    /// flag puts nulls first).
    Scan {
        files: Vec<PathBuf>,
        filter: Option<Expr>,
        columns: Option<Vec<String>>,
        order: Vec<(String, bool)>,
    },
}

fn context(tenant: &Tenant, threads: usize) -> SessionContext {
    let mut config = SessionConfig::new()
        .with_target_partitions(threads.max(1))
        .with_batch_size(BATCH_ROWS)
        .with_information_schema(false);
    config
        .options_mut()
        .execution
        .parquet
        .schema_force_view_types = false;
    config.options_mut().execution.parquet.skip_metadata = true;
    SessionContext::new_with_config_rt(config, Arc::clone(&tenant.runtime))
}

fn paths(tenant: &Tenant, files: &[PathBuf]) -> Outcome<Vec<String>> {
    files
        .iter()
        .map(|file| catalog::confined(file, &tenant.dir).map(|p| p.to_string_lossy().into_owned()))
        .collect()
}

/// `plan` with every file confined to the tenant's private directory.
fn resolve(tenant: &Tenant, plan: Plan) -> Outcome<Plan> {
    let confine = |files: &[PathBuf]| -> Outcome<Vec<PathBuf>> {
        Ok(paths(tenant, files)?
            .into_iter()
            .map(PathBuf::from)
            .collect())
    };
    Ok(match plan {
        Plan::Scan {
            files,
            filter,
            columns,
            order,
        } => Plan::Scan {
            files: confine(&files)?,
            filter,
            columns,
            order,
        },
    })
}

fn names(files: &[PathBuf]) -> Vec<String> {
    files
        .iter()
        .map(|file| file.to_string_lossy().into_owned())
        .collect()
}

async fn frame(ctx: &SessionContext, plan: Plan) -> Outcome<Option<DataFrame>> {
    let read = |files: Vec<String>| ctx.read_parquet(files, ParquetReadOptions::default());
    Ok(match plan {
        Plan::Scan {
            files,
            filter,
            columns,
            order,
        } => {
            if files.is_empty() {
                return Ok(None);
            }
            let mut frame = super::df(read(names(&files)).await)?;
            if let Some(filter) = filter {
                frame = super::df(frame.filter(filter))?;
            }
            if !order.is_empty() {
                let keys = order
                    .iter()
                    .map(|(name, nulls_first)| col(name).sort(true, *nulls_first))
                    .collect();
                frame = super::df(frame.sort(keys))?;
            }
            if let Some(columns) = columns {
                frame =
                    super::df(frame.select(columns.iter().map(|c| col(c)).collect::<Vec<Expr>>()))?;
            }
            Some(frame)
        }
    })
}

/// Run `plan`, handing each batch to `each` after checking it against the
/// byte budget (`memory`, cumulative: the consumer keeps what it is given)
/// and the deadline.
pub(super) fn stream(
    tenant: &Tenant,
    threads: usize,
    memory: usize,
    plan: Plan,
    each: &mut (dyn FnMut(&RecordBatch) -> Outcome<()> + Send),
) -> Outcome<()> {
    let ctx = context(tenant, threads);
    let plan = resolve(tenant, plan)?;
    let started = Instant::now();
    block_on(async move {
        let Some(frame) = frame(&ctx, plan).await? else {
            return Ok(());
        };
        let mut stream = super::df(frame.execute_stream().await)?;
        let mut kept = 0_usize;
        while let Some(batch) = stream.next().await {
            let batch = super::df(batch)?;
            let size = batch.get_array_memory_size();
            kept = kept.saturating_add(size);
            if kept > memory {
                return Err(refusal(
                    EXHAUSTED,
                    "a scan decodes more than the memory bound",
                ));
            }
            if started.elapsed() > SCAN_DEADLINE {
                return Err(refusal(EXHAUSTED, "a scan ran past its deadline"));
            }
            each(&batch)?;
        }
        Ok(())
    })?
}

/// Drive `future` on a current-thread runtime of its own, on a thread of its
/// own (so a caller inside another runtime never nests one); a thread or a
/// runtime that cannot start is a refusal, never a panic.
fn block_on<F>(future: F) -> Outcome<F::Output>
where
    F: std::future::Future + Send,
    F::Output: Send,
{
    std::thread::scope(|scope| {
        let handle = std::thread::Builder::new()
            .name("norn-datafusion".to_owned())
            .spawn_scoped(scope, || {
                tokio::runtime::Builder::new_current_thread()
                    .build()
                    .map(|runtime| runtime.block_on(future))
                    .map_err(|error| engine_error(error.to_string()))
            })
            .map_err(|error| engine_error(error.to_string()))?;
        handle
            .join()
            .map_err(|_panicked| engine_error("a scan failed"))?
    })
}
