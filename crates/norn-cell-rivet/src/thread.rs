//! The actor's cell thread: one OS thread per actor owns the opened cell and
//! runs every cell call in arrival order. It is the cell's writer lock
//! (`RivetKit` runs actions and HTTP callbacks concurrently; #1130 section 5),
//! and the synchronous `SqlEngine` blocks on Rivet's async `SQLite` from here,
//! never from a runtime worker.
//!
//! A panic inside a call is contained: the call answers [`CellGone::Panicked`],
//! any open transaction rolls back as the engine unwinds, and the cell is
//! dropped so the next call reopens it from storage.

use std::panic::AssertUnwindSafe;

use rivetkit::SqliteDb;
use tokio::runtime::Handle;
use tokio::sync::{mpsc, oneshot};

use crate::core::CellCore;
use crate::engine::RivetEngine;

/// What the cell thread holds between calls.
pub struct Slot {
    /// The opened cell, once a call opened it.
    pub core: Option<CellCore>,
    db: SqliteDb,
    runtime: Handle,
    /// The conformance seams' statement hook, when this host serves them.
    #[cfg(feature = "conformance")]
    pub seams: Option<std::rc::Rc<crate::conformance::Seams>>,
}

impl Slot {
    /// A fresh engine over this actor's database: what the cell opens on.
    pub fn engine(&self) -> impl Fn() -> RivetEngine + 'static {
        let db = self.db.clone();
        let runtime = self.runtime.clone();
        #[cfg(feature = "conformance")]
        let seams = self.seams.clone();
        move || {
            let engine = RivetEngine::new(db.clone(), runtime.clone());
            #[cfg(feature = "conformance")]
            if let Some(seams) = &seams {
                let seams: std::rc::Rc<crate::conformance::Seams> = std::rc::Rc::clone(seams);
                let hook: std::rc::Rc<dyn crate::engine::StatementHook> = seams;
                return engine.with_hook(hook);
            }
            engine
        }
    }
}

type Job = Box<dyn FnOnce(&mut Slot) + Send>;

/// Why a cell call did not answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellGone {
    /// The call panicked; the cell reopens on the next call.
    Panicked,
    /// The thread has ended (the actor is stopping).
    Stopped,
}

impl std::fmt::Display for CellGone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Panicked => "the cell call panicked",
            Self::Stopped => "the cell has stopped",
        })
    }
}

impl std::error::Error for CellGone {}

/// The handle the actor's tasks call the cell through.
#[derive(Clone)]
pub struct CellThread {
    jobs: mpsc::UnboundedSender<Job>,
}

impl std::fmt::Debug for CellThread {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CellThread").finish_non_exhaustive()
    }
}

impl CellThread {
    /// Start the thread for one actor's database. `conformance` installs the
    /// conformance seams (conformance builds that serve them only).
    ///
    /// # Errors
    /// The OS refused a thread.
    #[cfg_attr(
        not(feature = "conformance"),
        allow(
            unused_variables,
            reason = "the seams exist only in conformance builds"
        )
    )]
    pub fn spawn(
        name: &str,
        db: SqliteDb,
        runtime: Handle,
        conformance: bool,
    ) -> std::io::Result<Self> {
        let (jobs, mut queue) = mpsc::unbounded_channel::<Job>();
        std::thread::Builder::new()
            .name(format!("norn-cell {name}"))
            .spawn(move || {
                let mut slot = Slot {
                    core: None,
                    db,
                    runtime,
                    #[cfg(feature = "conformance")]
                    seams: conformance
                        .then(|| std::rc::Rc::new(crate::conformance::Seams::default())),
                };
                while let Some(job) = queue.blocking_recv() {
                    job(&mut slot);
                }
            })?;
        Ok(Self { jobs })
    }

    /// Run `f` on the cell thread after every call queued before it.
    ///
    /// # Errors
    /// The call panicked, or the thread has ended.
    pub async fn call<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Slot) -> T + Send + 'static,
    ) -> Result<T, CellGone> {
        let (reply, answer) = oneshot::channel();
        let job: Job = Box::new(move |slot: &mut Slot| {
            let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| f(slot)));
            let outcome = outcome.map_err(|_panic| {
                // The cell's in-memory state may be half-updated: reopen it.
                slot.core = None;
                CellGone::Panicked
            });
            // A caller that stopped waiting has nothing to receive.
            let _sent = reply.send(outcome);
        });
        self.jobs.send(job).map_err(|_closed| CellGone::Stopped)?;
        answer.await.map_err(|_dropped| CellGone::Stopped)?
    }
}
