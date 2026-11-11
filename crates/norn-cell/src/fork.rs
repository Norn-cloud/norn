//! FORK-PIN-1 and WORLD-ISOLATE-1: the store fork contract (Core 08, fork
//! lifecycle and isolation).
//!
//! A fork is a separate store instance, never rows in its parent's database.
//! [`CellStore::fork`] pins it to the parent's verified head and the parent's
//! world, and the fork inherits the parent's cell identity and bindings
//! unchanged: they are part of the copied contents. The pin is durable in the
//! fork's own metadata, so reopening a fork keeps it a fork.
//!
//! A fork never performs: claiming a command and recording a performed result
//! refuse `core.world.perform_forbidden` before anything is written. A fork
//! records simulated results instead ([`CellStore::record_simulated`]); main
//! refuses them (`core.world.simulation_forbidden`).
//!
//! Inside a fork, the row `world` label and receipts keep the label of the
//! timeline the fork inherited: relabelling the inherited rows would copy them.
//! The pin carries the fork's world, so every world-boundary decision reads
//! [`CellStore::world`], never a row label. Rows and receipts a fork mints are
//! not yet bound to its pin, so a fork refuses to export them or its receipt
//! log as evidence (FORK-EVIDENCE-1, `core.world.fork_evidence_forbidden`)
//! until a versioned store format binds the pin into what a fork writes.
//!
//! [`ForkEngine`] is the storage seam. The native engine implements it by
//! copying the whole database (SQLite's online backup). That copy is the
//! reference oracle: a copy-on-write engine must produce a fork whose
//! [`StoreImage`] equals the copy's, and it is not a production fork path.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::engine::{SqlEngine, SqlRow, SqlValue, params};
use crate::error::CellError;
use crate::read::LedgerHead;
use crate::store::{CellStore, MAIN_WORLD};
use crate::system::META;

const WORLD_KEY: &str = "fork:world";
const PARENT_KEY: &str = "fork:parent";
const BASE_KEY: &str = "fork:base";
const HORIZON_KEY: &str = "fork:main_horizon";

/// An engine that can fork: produce an independent engine whose committed
/// contents equal its own at the moment of the call.
///
/// After the call, writes to either engine are invisible to the other (L2),
/// and every read of the new engine returns what reading a full copy would
/// (L4). The native engine implements this by copying; that copy is the
/// oracle other engines are checked against.
pub trait ForkEngine: SqlEngine + Sized {
    /// An independent engine with this engine's committed contents.
    ///
    /// # Errors
    /// Refuses inside an open transaction, so uncommitted state is never
    /// forked; backend errors otherwise.
    fn fork_engine(&self) -> Result<Self, CellError>;
}

/// FORK-PIN-1: what a fork is pinned to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ForkPin {
    /// The fork's world id: never `main`, never its parent's world.
    pub world: String,
    /// The parent's world: `main` or another fork.
    pub parent: String,
    /// The parent's verified ledger head when the fork was created.
    pub base: LedgerHead,
    /// FORK-EVIDENCE-1: the commit count at which this fork's lineage left
    /// main. For a fork of main it is `base.commits`; a fork of a fork
    /// inherits its parent's unchanged, because commits its parent minted
    /// are fork-minted too even though they precede this fork's base.
    pub main_horizon: u64,
}

/// A store's complete committed contents, table by table, for comparing
/// stores. Each table's rows are in rowid order, rowid first; the schema
/// catalog omits physical page numbers. Tables the host platform keeps in the
/// same database ([`SqlEngine::platform_table_globs`]) are not cell state and
/// are omitted.
#[derive(Debug, Clone, PartialEq)]
pub struct StoreImage {
    /// Rows by table name; the catalog is under `sqlite_master`.
    pub tables: BTreeMap<String, Vec<Vec<SqlValue>>>,
}

impl StoreImage {
    /// This image without the fork pin, for comparing a fork with a store
    /// that reached the same state without forking.
    #[must_use]
    pub fn without_fork_pin(mut self) -> Self {
        if let Some(rows) = self.tables.get_mut(META) {
            rows.retain(
                |row| !matches!(row.get(1), Some(SqlValue::Text(key)) if key.starts_with("fork:")),
            );
        }
        self
    }
}

pub(crate) fn load_pin<E: SqlEngine>(conn: &E) -> Result<Option<ForkPin>, CellError> {
    let meta = |key: &str| {
        conn.query_optional(
            &format!("SELECT \"value\" FROM \"{META}\" WHERE \"key\" = ?1"),
            params![key],
            |row| row.get::<String>(0),
        )
    };
    let found = [
        meta(WORLD_KEY)?,
        meta(PARENT_KEY)?,
        meta(BASE_KEY)?,
        meta(HORIZON_KEY)?,
    ];
    if found.iter().all(Option::is_none) {
        return Ok(None);
    }
    let [Some(world), Some(parent), Some(base), Some(main_horizon)] = found else {
        return Err(CellError::Encoding("incomplete fork pin".to_owned()));
    };
    let base = serde_json::from_str(&base)
        .map_err(|error| CellError::Encoding(format!("fork base: {error}")))?;
    let main_horizon = main_horizon
        .parse()
        .map_err(|error| CellError::Encoding(format!("fork main horizon: {error}")))?;
    let pin = ForkPin {
        world,
        parent,
        base,
        main_horizon,
    };
    if pin.main_horizon > pin.base.commits {
        return Err(CellError::Encoding(
            "fork pin: the main horizon is past the base".to_owned(),
        ));
    }
    Ok(Some(pin))
}

pub(crate) fn write_pin<E: SqlEngine>(conn: &E, pin: &ForkPin) -> Result<(), CellError> {
    let base = serde_json::to_string(&pin.base)
        .map_err(|error| CellError::Encoding(format!("fork base: {error}")))?;
    // A fork of a fork replaces the copied pin: its parent is that fork.
    conn.execute(
        &format!("DELETE FROM \"{META}\" WHERE \"key\" IN (?1, ?2, ?3, ?4)"),
        params![WORLD_KEY, PARENT_KEY, BASE_KEY, HORIZON_KEY],
    )?;
    let insert = format!("INSERT INTO \"{META}\" (\"key\", \"value\") VALUES (?1, ?2)");
    conn.execute(&insert, params![WORLD_KEY, pin.world.as_str()])?;
    conn.execute(&insert, params![PARENT_KEY, pin.parent.as_str()])?;
    conn.execute(&insert, params![BASE_KEY, base])?;
    conn.execute(&insert, params![HORIZON_KEY, pin.main_horizon.to_string()])?;
    Ok(())
}

impl<E: ForkEngine> CellStore<E> {
    /// FORK-PIN-1: fork this store as world `world`, pinned at `base`.
    ///
    /// `base` must be this store's verified head, so a caller forks exactly
    /// the state it observed. The fork is a separate store over
    /// [`ForkEngine::fork_engine`]: it shares this store's model, inherits its
    /// identity and bindings, and records [`ForkPin`] in its own metadata.
    /// The fork's verification state is inherited, not recomputed, because
    /// the engine guarantees equal contents. Nothing is written to this
    /// store.
    ///
    /// # Errors
    ///
    /// `core.world.fork_invalid_world` for `main`, an empty id or this store's
    /// own world; `core.world.fork_stale_base` when `base` is not the verified
    /// head; ledger verification or engine errors otherwise.
    pub fn fork(&self, world: &str, base: &LedgerHead) -> Result<Self, CellError> {
        let parent = self
            .fork_pin()
            .map_or(MAIN_WORLD, |pin| pin.world.as_str())
            .to_owned();
        let inherited_horizon = self.fork_pin().map(|pin| pin.main_horizon);
        if world.is_empty() || world == MAIN_WORLD || world == parent {
            return Err(CellError::ForkInvalidWorld);
        }
        let head = self.verified_head()?;
        if &head != base {
            return Err(CellError::ForkStaleBase {
                expected: base.commits,
                head: head.commits,
            });
        }
        let engine = self.conn.fork_engine()?;
        // Another handle may commit between the head check and the copy: the
        // copy itself must hold exactly `base`, or the fork refuses.
        let copied = copied_head(&engine, self.model())?;
        if copied != (head.commits, head.hash.clone()) {
            return Err(CellError::ForkStaleBase {
                expected: base.commits,
                head: copied.0,
            });
        }
        let pin = ForkPin {
            world: world.to_owned(),
            parent,
            main_horizon: inherited_horizon.unwrap_or(head.commits),
            base: head,
        };
        engine.transaction(|| write_pin(&engine, &pin))?;
        Ok(self.forked(engine, pin))
    }
}

/// The newest commit of `engine` and its ledger head hash: one indexed read
/// of the newest receipt, not a verification pass (the copy's contents are
/// the parent's, which this store verified).
fn copied_head<E: SqlEngine>(engine: &E, model: &crate::Model) -> Result<(u64, String), CellError> {
    let reader = crate::StoreReader::new(engine, model);
    let commits = reader.next_commit_seq()?.saturating_sub(1);
    if commits == 0 {
        return Ok((0, crate::system::GENESIS_HASH.to_owned()));
    }
    let newest = reader.receipts_after(commits.saturating_sub(1), 1)?;
    let hash = newest
        .first()
        .filter(|receipt| receipt.body.commit_seq == commits)
        .map(|receipt| receipt.body.ledger_head.clone())
        .ok_or(CellError::LedgerCorrupt {
            seq: commits,
            reason: "the fork copy has no newest receipt".to_owned(),
        })?;
    Ok((commits, hash))
}

impl<E: SqlEngine> CellStore<E> {
    /// The pin of a forked store; `None` for a store that is not a fork.
    #[must_use]
    pub const fn fork_pin(&self) -> Option<&ForkPin> {
        self.fork.as_ref()
    }

    /// The world this store serves, from its pin and never from row labels:
    /// [`norn_kernel::World::Main`] unless the store is a fork. Every
    /// world-boundary decision (perform, result recording, kernel turns,
    /// evidence) reads this; a fork's inherited rows still say `main`.
    #[must_use]
    pub fn world(&self) -> norn_kernel::World {
        self.fork
            .as_ref()
            .map_or(norn_kernel::World::Main, |pin| norn_kernel::World::Fork {
                id: norn_kernel::WorldId(pin.world.clone()),
            })
    }

    /// FORK-EVIDENCE-1: a fork's receipt log is never presented as evidence.
    pub(crate) fn refuse_fork_evidence(&self) -> Result<(), CellError> {
        if self.fork.is_some() {
            return Err(CellError::ForkEvidenceForbidden);
        }
        Ok(())
    }

    /// WORLD-ISOLATE-1: refuse anything that would reach the outside world.
    pub(crate) fn refuse_perform_in_fork(&self) -> Result<(), CellError> {
        if self.fork.is_some() {
            return Err(CellError::WorldPerformForbidden);
        }
        Ok(())
    }

    /// FORK-SIMULATE-1: record `outcome` as the simulated result of the
    /// outbox head command `command_id` at `generation`, without a claim or
    /// a dispatch. The fork pin labels it simulated; it is delivered and
    /// consumed exactly like a performed result. Returns `false` (and writes
    /// nothing) when that command is not the head or already has a result.
    ///
    /// # Errors
    ///
    /// `core.world.simulation_forbidden` in a store that is not a fork; SQL
    /// engine errors otherwise.
    pub fn record_simulated(
        &mut self,
        command_id: &str,
        generation: u64,
        outcome: &norn_ir::RuntimeValue,
    ) -> Result<bool, CellError> {
        if self.fork.is_none() {
            return Err(CellError::WorldSimulationForbidden);
        }
        self.record_head_result(command_id, generation, outcome)
    }

    /// The store's complete committed contents ([`StoreImage`]).
    ///
    /// # Errors
    ///
    /// SQL engine errors.
    pub fn image(&self) -> Result<StoreImage, CellError> {
        let platform = self.conn.platform_table_globs();
        let filter = " AND name NOT GLOB ?".repeat(platform.len());
        let globs: Vec<SqlValue> = platform.iter().map(|glob| SqlValue::from(*glob)).collect();
        self.conn.transaction(|| {
            let catalog = self.conn.query(
                &format!(
                    "SELECT type, name, tbl_name, sql FROM sqlite_master WHERE 1{filter} ORDER BY type, name"
                ),
                &globs,
            )?;
            let names = self.conn.query(
                &format!(
                    "SELECT name FROM sqlite_master WHERE type = 'table'{filter} ORDER BY name"
                ),
                &globs,
            )?;
            let mut tables = BTreeMap::from([("sqlite_master".to_owned(), catalog)]);
            for name in names {
                let name: String = SqlRow(name).get(0)?;
                let rows = self.conn.query(
                    &format!(
                        "SELECT rowid, * FROM {} ORDER BY rowid",
                        norn_projections::sqlite_ddl::quote_identifier(&name)
                    ),
                    &[],
                )?;
                tables.insert(name, rows);
            }
            Ok(StoreImage { tables })
        })
    }
}

#[cfg(all(test, feature = "native"))]
mod tests {
    use norn_projections::SqliteDdlInput;

    use std::path::PathBuf;

    use super::{ForkEngine, ForkPin};
    use crate::engine::{NativeEngine, ReadIsolation, SqlEngine, SqlValue};
    use crate::{CellError, CellStore, CommitBatch, MAIN_WORLD, Model, StoreOptions};

    /// A file engine whose fork first lets another handle commit: the race
    /// between the parent's head check and the copy, made deterministic.
    #[derive(Debug)]
    struct Racing {
        native: NativeEngine,
        path: PathBuf,
        model: Model,
    }

    impl SqlEngine for Racing {
        fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<(), CellError> {
            self.native.execute(sql, params)
        }
        fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
            self.native.query(sql, params)
        }
        fn execute_batch(&self, sql: &str) -> Result<(), CellError> {
            self.native.execute_batch(sql)
        }
        fn transaction<T, X: From<CellError>>(
            &self,
            f: impl FnOnce() -> Result<T, X>,
        ) -> Result<T, X> {
            self.native.transaction(f)
        }
        fn read_isolation(&self) -> ReadIsolation {
            self.native.read_isolation()
        }
    }

    impl ForkEngine for Racing {
        fn fork_engine(&self) -> Result<Self, CellError> {
            let mut other =
                CellStore::open(&self.path, self.model.clone(), StoreOptions::default())?;
            other.commit(&CommitBatch {
                world: MAIN_WORLD.to_owned(),
                scope: "s".to_owned(),
                ..CommitBatch::default()
            })?;
            Ok(Self {
                native: self.native.fork_engine()?,
                path: self.path.clone(),
                model: self.model.clone(),
            })
        }
    }

    /// FORK-PIN-1 (Fable S3): a commit that lands between the head check and
    /// the copy refuses the fork; it never pins a base the copy does not hold.
    #[test]
    fn a_commit_racing_the_copy_refuses_the_fork() -> Result<(), CellError> {
        let model = Model::new(&SqliteDdlInput {
            object_types: &[],
            property_types: &[],
        })?;
        let dir = tempfile::tempdir().map_err(|error| CellError::Engine(error.to_string()))?;
        let path = dir.path().join("cell.sqlite");
        drop(CellStore::open(
            &path,
            model.clone(),
            StoreOptions::default(),
        )?);
        let store = CellStore::from_engine(
            Racing {
                native: NativeEngine::open(&path, StoreOptions::default())?,
                path: path.clone(),
                model: model.clone(),
            },
            model,
        )?;
        let base = store.verified_head()?;
        assert!(matches!(
            store.fork("raced", &base),
            Err(CellError::ForkStaleBase {
                expected: 0,
                head: 1
            })
        ));
        Ok(())
    }

    /// FORK-PIN-1, exhaustive to depth 4: every fork in a chain keeps its pin
    /// across reopen, names its parent, and still refuses to perform.
    #[test]
    fn a_fork_chain_keeps_its_pins_across_reopen() -> Result<(), CellError> {
        let model = Model::new(&SqliteDdlInput {
            object_types: &[],
            property_types: &[],
        })?;
        let mut store = CellStore::open_in_memory(model.clone())?;
        let mut parent = MAIN_WORLD.to_owned();
        let horizon = store.verified_head()?.commits;
        assert_eq!(store.world(), norn_kernel::World::Main);
        for level in 0..4 {
            let base = store.verified_head()?;
            let world = format!("w{level}");
            let fork = store.fork(&world, &base)?;
            let expected = ForkPin {
                world: world.clone(),
                parent: parent.clone(),
                base,
                main_horizon: horizon,
            };
            assert_eq!(fork.fork_pin(), Some(&expected));
            assert_eq!(
                fork.world(),
                norn_kernel::World::Fork {
                    id: norn_kernel::WorldId(world.clone()),
                }
            );
            let mut reopened = CellStore::from_engine(fork.conn, model.clone())?;
            assert_eq!(reopened.fork_pin(), Some(&expected));
            assert!(matches!(
                reopened.record_performed("c", 0, &norn_ir::RuntimeValue::Null),
                Err(CellError::WorldPerformForbidden)
            ));
            assert!(matches!(
                reopened.fork(&world, &reopened.verified_head()?),
                Err(CellError::ForkInvalidWorld)
            ));
            parent = world;
            store = reopened;
        }
        Ok(())
    }
    /// FORK-EVIDENCE-1 with CELL-ID-1: a fork carries its parent's cell
    /// binding. It exports main's commits up to the horizon under that
    /// binding, refuses its own commits before any binding check, still
    /// refuses a record bound to another cell, and its exportable prefix
    /// rebuilds a main store (no pin) under the parent's identity.
    #[test]
    fn a_fork_exports_under_its_parents_cell_binding() -> Result<(), CellError> {
        let model = Model::new(&SqliteDdlInput {
            object_types: &[],
            property_types: &[],
        })?;
        let empty = || CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: "s".to_owned(),
            ..CommitBatch::default()
        };
        let missing = || CellError::Engine("expected a record".to_owned());
        let mut main = CellStore::open_in_memory(model)?;
        main.commit(&empty())?;
        main.commit(&empty())?;
        let id = main.cell_id()?.ok_or_else(missing)?;
        let base = main.verified_head()?;
        let mut fork = main.fork("what-if", &base)?;
        assert_eq!(fork.cell_id()?, Some(id.clone()));
        fork.require_cell_id(&id)?;

        let mut records = Vec::new();
        for seq in 1..=base.commits {
            let record = fork.export_record(seq)?.ok_or_else(missing)?;
            assert_eq!(Some(&record), main.export_record(seq)?.as_ref());
            assert_eq!(
                crate::cell_identity::from_bindings(&record.bindings)?,
                Some(id.clone())
            );
            records.push(record);
        }

        // A fork-minted commit carries the same binding but is never
        // evidence: the fence refuses it before the binding is read.
        fork.commit(&empty())?;
        let minted = base.commits + 1;
        assert!(fork.local_export_record(minted)?.is_some());
        assert!(matches!(
            fork.export_record(minted),
            Err(CellError::ForkEvidenceForbidden)
        ));

        // Its exportable prefix rebuilds a main store under the same identity.
        let rebuilt = CellStore::rebuild_export_history(
            NativeEngine::open_in_memory()?,
            &records,
            &records.last().ok_or_else(missing)?.hash()?,
        )?;
        assert_eq!(rebuilt.fork_pin(), None);
        assert_eq!(rebuilt.cell_id()?, Some(id));
        // The binding check still applies inside the fork.
        fork.conn.execute(
            &format!(
                "UPDATE \"{}\" SET \"value\" = ?1 WHERE \"key\" = 'binding:cell_id'",
                crate::system::META
            ),
            &[SqlValue::Text("0".repeat(32))],
        )?;
        assert!(matches!(
            fork.export_record(1),
            Err(CellError::BindingMismatch { .. })
        ));
        Ok(())
    }
}
