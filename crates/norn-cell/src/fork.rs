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
    let found = [meta(WORLD_KEY)?, meta(PARENT_KEY)?, meta(BASE_KEY)?];
    if found.iter().all(Option::is_none) {
        return Ok(None);
    }
    let [Some(world), Some(parent), Some(base)] = found else {
        return Err(CellError::Encoding("incomplete fork pin".to_owned()));
    };
    let base = serde_json::from_str(&base)
        .map_err(|error| CellError::Encoding(format!("fork base: {error}")))?;
    Ok(Some(ForkPin {
        world,
        parent,
        base,
    }))
}

fn write_pin<E: SqlEngine>(conn: &E, pin: &ForkPin) -> Result<(), CellError> {
    let base = serde_json::to_string(&pin.base)
        .map_err(|error| CellError::Encoding(format!("fork base: {error}")))?;
    // A fork of a fork replaces the copied pin: its parent is that fork.
    conn.execute(
        &format!("DELETE FROM \"{META}\" WHERE \"key\" IN (?1, ?2, ?3)"),
        params![WORLD_KEY, PARENT_KEY, BASE_KEY],
    )?;
    let insert = format!("INSERT INTO \"{META}\" (\"key\", \"value\") VALUES (?1, ?2)");
    conn.execute(&insert, params![WORLD_KEY, pin.world.as_str()])?;
    conn.execute(&insert, params![PARENT_KEY, pin.parent.as_str()])?;
    conn.execute(&insert, params![BASE_KEY, base])?;
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
        let pin = ForkPin {
            world: world.to_owned(),
            parent,
            base: head,
        };
        engine.transaction(|| write_pin(&engine, &pin))?;
        Ok(self.forked(engine, pin))
    }
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

    use super::ForkPin;
    use crate::{CellError, CellStore, MAIN_WORLD, Model};

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
        assert_eq!(store.world(), norn_kernel::World::Main);
        for level in 0..4 {
            let base = store.verified_head()?;
            let world = format!("w{level}");
            let fork = store.fork(&world, &base)?;
            let expected = ForkPin {
                world: world.clone(),
                parent: parent.clone(),
                base,
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
}
