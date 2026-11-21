//! ENGINE-UPGRADE-1 (#1494): the engine version a cell's data was written
//! with, the ordered migrations that bring an older cell up to this engine,
//! and the refusals for a cell this engine cannot serve.
//!
//! A cell records its **storage version** in the `_norn_meta` row
//! [`STAMP_KEY`]. The storage version is the compatibility contract between
//! a cell and an engine: every change to what an engine writes that an
//! older engine could misread bumps it, and every bump comes with one
//! migration step. Releases that share a storage version are
//! interchangeable for a cell, so a host may roll back between them; across
//! a bump it may not. The release channel is described in
//! `docs/platform/engine-upgrades.md`.
//!
//! [`STORE_FORMAT`](crate::system::STORE_FORMAT) stays the format family:
//! ledger events name it, so it never changes for an upgradable cell.
//! Storage version 1 is every `norn.cell.store/8` cell written before the
//! stamp existed; it carries no stamp.
//!
//! The check runs before any schema statement, definition decoding, host
//! model selection or history verification touches the file, so a refused
//! open leaves the cell unchanged. The migrations run inside the open
//! transaction, before this engine's schema statements, so an open that
//! fails later (a schema mismatch, a ledger that does not verify) rolls the
//! upgrade back with it. It lives below the host seam: the native host, the
//! Durable Object host and the Rivet host all open through
//! [`CellStore`](crate::CellStore), which calls it.

use serde::{Deserialize, Serialize};

use crate::engine::{SqlEngine, SqlValue, params};
use crate::error::CellError;
use crate::system::META;

/// `_norn_meta` key holding the engine stamp.
pub const STAMP_KEY: &str = "engine";

/// Encoding of the stamp this engine reads and writes. A stamp whose
/// `format` differs is from a release this engine cannot order against
/// itself, and refuses [`CellError::EngineUnknownVersion`]. Fields may be
/// added to `/1` (older engines ignore them); the tag changes only when an
/// older engine must not read the stamp at all.
pub const STAMP_FORMAT: &str = "norn.cell.engine/1";

/// One storage version of the release channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorageVersion {
    /// The version.
    pub version: u32,
    /// What changed in what an engine writes.
    pub change: &'static str,
}

/// Every storage version this engine knows, oldest first. The last is the
/// one it writes ([`STORAGE_VERSION`]); a new version appends an entry here
/// and a step in `migrate_to`.
pub const CHANNEL: &[StorageVersion] = &[
    StorageVersion {
        version: 1,
        change: "norn.cell.store/8 before the engine stamp: no `engine` row in _norn_meta",
    },
    StorageVersion {
        version: 2,
        change: "the engine stamp: _norn_meta `engine` records the storage version and the release that wrote it",
    },
    StorageVersion {
        version: 3,
        change: "APPROVAL-RECOVERY-1 (#1483): the ledger events norn.approval.refused, norn.approval.consumed and norn.step.call_owned, and their ledger-derived tables _norn_approved_call and _norn_step_call",
    },
    StorageVersion {
        version: 4,
        change: "MAP-DERIVATION-1 (#1488): a mapping bundle records the runtime derivation it was captured under (`derivation`) and the quarantined bundle it reprocesses (`reprocesses`)",
    },
    StorageVersion {
        version: 5,
        change: "OPEN-CHECKPOINT-1 (#1533): _norn_open_checkpoint holds the last verified ledger head and its receipt root; an open verifies only the history after it",
    },
];

/// The storage version this engine writes.
pub const STORAGE_VERSION: u32 = 5;

/// The oldest storage version this engine upgrades in place. Raising it
/// retires the steps below it; an older cell must first open on a bridge
/// release (one whose floor admits it).
pub const UPGRADES_FROM: u32 = 1;

/// The release label this engine writes into a stamp: a release build sets
/// `NORN_RELEASE` (for example `drop-30`); any other build is `dev`. It is
/// informational, named in a downgrade refusal; admission orders storage
/// versions only.
pub const WRITER: &str = match option_env!("NORN_RELEASE") {
    Some(release) => release,
    None => "dev",
};

/// An engine's storage window: what it writes and the oldest it upgrades.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineVersion {
    /// The storage version it writes.
    pub storage: u32,
    /// The oldest storage version it upgrades in place.
    pub upgrades_from: u32,
}

/// This engine.
pub const THIS_ENGINE: EngineVersion = EngineVersion {
    storage: STORAGE_VERSION,
    upgrades_from: UPGRADES_FROM,
};

/// The stamp a cell carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineStamp {
    /// [`STAMP_FORMAT`].
    pub format: String,
    /// The storage version of the cell's data.
    pub storage: u32,
    /// The release label that wrote this storage version ([`WRITER`]).
    pub writer: String,
}

impl EngineStamp {
    /// The stamp this engine writes.
    #[must_use]
    pub fn current() -> Self {
        Self {
            format: STAMP_FORMAT.to_owned(),
            storage: STORAGE_VERSION,
            writer: WRITER.to_owned(),
        }
    }
}

/// What a file's metadata says about the engine that wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoredVersion {
    /// No store yet: the open creates one at this engine's version.
    Fresh,
    /// A `norn.cell.store/8` store from before the stamp: storage version 1.
    Unstamped,
    /// The stored stamp, verbatim.
    Stamped(String),
}

/// What an open does about the cell's version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// No store yet; the open stamps it.
    Fresh,
    /// The cell is at the engine's storage version; nothing to do.
    Current,
    /// Run the steps `from + 1 ..= engine.storage` in order, then restamp.
    Upgrade {
        /// The cell's storage version.
        from: u32,
    },
}

const SHOWN_STAMP_CHARS: usize = 128;

fn unknown(found: &str) -> CellError {
    CellError::EngineUnknownVersion {
        found: found.chars().take(SHOWN_STAMP_CHARS).collect(),
    }
}

/// Decide what `engine` does with a cell whose metadata says `stored`.
/// Pure, so an older or newer engine's window can be checked against a cell
/// without opening it.
///
/// # Errors
/// [`CellError::EngineUnknownVersion`] for a stamp it cannot read,
/// [`CellError::EngineDowngrade`] for a cell newer than `engine`, and
/// [`CellError::EngineUpgradeUnsupported`] for one older than its floor.
pub fn admit(stored: &StoredVersion, engine: EngineVersion) -> Result<Admission, CellError> {
    let (cell, writer) = match stored {
        StoredVersion::Fresh => return Ok(Admission::Fresh),
        StoredVersion::Unstamped => (1, "a release before the engine stamp".to_owned()),
        StoredVersion::Stamped(text) => {
            let stamp: EngineStamp =
                serde_json::from_str(text).map_err(|_unreadable| unknown(text))?;
            if stamp.format != STAMP_FORMAT || stamp.storage == 0 {
                return Err(unknown(text));
            }
            (stamp.storage, stamp.writer)
        }
    };
    order(cell, writer, engine)
}

/// Decide what `engine` does with an export whose newest record says it was
/// written at storage version `storage`. A record without one predates the
/// field and counts as storage version 1
/// (`docs/platform/engine-upgrades.md`). [`Admission::Upgrade`] means the
/// restore rebuilds an older export under this engine; the rebuilt file is
/// written at this engine's version.
///
/// # Errors
/// As [`admit`]: version 0 is unknown, a newer one a downgrade.
pub fn admit_export(storage: Option<u32>, engine: EngineVersion) -> Result<Admission, CellError> {
    match storage {
        None => order(
            1,
            "a release before export storage versions".to_owned(),
            engine,
        ),
        Some(0) => Err(unknown("export storageVersion 0")),
        Some(version) => order(version, "the exporting engine".to_owned(), engine),
    }
}

fn order(cell: u32, writer: String, engine: EngineVersion) -> Result<Admission, CellError> {
    if cell > engine.storage {
        return Err(CellError::EngineDowngrade {
            cell,
            writer,
            engine: engine.storage,
        });
    }
    if cell < engine.upgrades_from {
        return Err(CellError::EngineUpgradeUnsupported {
            cell,
            engine: engine.storage,
            reason: format!(
                "this engine upgrades storage versions {} to {}; open the cell first on a release whose floor admits version {cell}",
                engine.upgrades_from, engine.storage
            ),
        });
    }
    Ok(if cell == engine.storage {
        Admission::Current
    } else {
        Admission::Upgrade { from: cell }
    })
}

/// Read what `engine`'s metadata says about the engine that wrote it,
/// without writing.
///
/// # Errors
/// Engine failures, or [`CellError::EngineUnknownVersion`] for a stamp on a
/// store that records no format, or a stamp that is not text.
pub fn stored_version<E: SqlEngine>(engine: &E) -> Result<StoredVersion, CellError> {
    let has_meta = engine
        .query_optional(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![META],
            |row| row.get::<i64>(0),
        )?
        .is_some();
    if !has_meta {
        return Ok(StoredVersion::Fresh);
    }
    let meta = |key: &str| {
        engine.query_optional(
            &format!("SELECT \"value\" FROM \"{META}\" WHERE \"key\" = ?1"),
            params![key],
            |row| row.get_ref(0).cloned(),
        )
    };
    // The column is TEXT but not STRICT, so it can hold a non-text value: a
    // stamp that is not text is unreadable, as malformed JSON is.
    let text = |stamp: SqlValue| match stamp {
        SqlValue::Text(stamp) => Ok(stamp),
        SqlValue::Null | SqlValue::Integer(_) | SqlValue::Real(_) | SqlValue::Blob(_) => {
            Err(unknown("a stamp that is not text"))
        }
    };
    match (meta("format")?, meta(STAMP_KEY)?) {
        (None, None) => Ok(StoredVersion::Fresh),
        (Some(_), None) => Ok(StoredVersion::Unstamped),
        (Some(_), Some(stamp)) => Ok(StoredVersion::Stamped(text(stamp)?)),
        (None, Some(stamp)) => Err(unknown(&text(stamp)?)),
    }
}

/// This engine's admission of `engine`'s cell, read-only.
///
/// # Errors
/// As [`stored_version`] and [`admit`].
pub fn admission<E: SqlEngine>(engine: &E) -> Result<Admission, CellError> {
    admit(&stored_version(engine)?, THIS_ENGINE)
}

/// Refuse unless `engine`'s cell is at this engine's storage version, without
/// writing: a copy of a cell (a fork) is never migrated, so it must already
/// be one this engine serves as it is.
///
/// # Errors
/// As [`admission`]; [`CellError::EngineUpgradeUnsupported`] for an older
/// cell, [`CellError::Engine`] for a file with no store.
pub(crate) fn require_current<E: SqlEngine>(engine: &E) -> Result<(), CellError> {
    match admission(engine)? {
        Admission::Current => Ok(()),
        Admission::Upgrade { from } => Err(CellError::EngineUpgradeUnsupported {
            cell: from,
            engine: STORAGE_VERSION,
            reason: "a copy is never migrated; open the cell to upgrade it first".to_owned(),
        }),
        Admission::Fresh => Err(CellError::Engine("the copy holds no cell store".to_owned())),
    }
}

/// Run `step(to)` for every storage version after `from` up to `to`, in
/// order, stopping at the first refusal.
fn run_chain(
    from: u32,
    to: u32,
    mut step: impl FnMut(u32) -> Result<(), CellError>,
) -> Result<(), CellError> {
    let mut version = from;
    while version < to {
        version = version
            .checked_add(1)
            .ok_or(CellError::OutOfRange("storage version"))?;
        step(version)?;
    }
    Ok(())
}

/// The one migration step that brings a cell from `to - 1` to `to`.
fn migrate_to<E: SqlEngine>(_engine: &E, to: u32) -> Result<(), CellError> {
    match to {
        // Version 2 adds the stamp alone, which `upgrade` writes after the chain.
        // Version 3 adds the approval recovery tables. They derive from the
        // ledger alone: the schema statements that follow the chain create
        // them, and `BACKFILL_APPROVED_CALLS` fills `_norn_approved_call`
        // from the cell's own approving decisions. No row is rewritten.
        // Version 4 rewrites nothing: a bundle without `derivation` is
        // derivation 1 and still verifies under it (MAP-DERIVATION-1).
        // Version 5 adds `_norn_open_checkpoint`, created empty by the
        // schema statements that follow the chain; the upgrading open then
        // verifies the whole history and records its head there
        // (OPEN-CHECKPOINT-1). No row is rewritten.
        2..=5 => Ok(()),
        _ => Err(CellError::EngineUpgradeUnsupported {
            cell: to.saturating_sub(1),
            engine: STORAGE_VERSION,
            reason: format!("no migration to storage version {to}"),
        }),
    }
}

/// Bring `engine`'s cell to this engine's storage version: the ordered
/// migrations, then the stamp. Call inside the open transaction, before the
/// schema statements, so a later failure of the open rolls it back.
///
/// # Errors
/// As [`admission`], a migration's refusal, or engine failures.
pub(crate) fn upgrade<E: SqlEngine>(engine: &E) -> Result<(), CellError> {
    match admission(engine)? {
        Admission::Fresh | Admission::Current => Ok(()),
        Admission::Upgrade { from } => {
            run_chain(from, STORAGE_VERSION, |to| migrate_to(engine, to))?;
            engine.execute(
                &format!(
                    "INSERT INTO \"{META}\" (\"key\", \"value\") VALUES (?1, ?2) \
                     ON CONFLICT (\"key\") DO UPDATE SET \"value\" = excluded.\"value\""
                ),
                params![STAMP_KEY, stamp_text()?],
            )
        }
    }
}

/// Stamp a store this open just created.
///
/// # Errors
/// Engine failures.
pub(crate) fn stamp_fresh<E: SqlEngine>(engine: &E) -> Result<(), CellError> {
    engine.execute(
        &format!("INSERT INTO \"{META}\" (\"key\", \"value\") VALUES (?1, ?2)"),
        params![STAMP_KEY, stamp_text()?],
    )
}

fn stamp_text() -> Result<String, CellError> {
    serde_json::to_string(&EngineStamp::current())
        .map_err(|error| CellError::Encoding(format!("engine stamp: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENGINE: EngineVersion = EngineVersion {
        storage: 4,
        upgrades_from: 2,
    };

    fn stamped(storage: u32) -> StoredVersion {
        StoredVersion::Stamped(format!(
            r#"{{"format":"{STAMP_FORMAT}","storage":{storage},"writer":"drop-40"}}"#
        ))
    }

    #[test]
    fn the_channel_is_contiguous_and_ends_at_this_engine() {
        let versions: Vec<u32> = CHANNEL.iter().map(|entry| entry.version).collect();
        let expected: Vec<u32> = (1..=STORAGE_VERSION).collect();
        assert_eq!(versions, expected);
        assert!((1..=STORAGE_VERSION).contains(&UPGRADES_FROM));
    }

    #[cfg(feature = "native")]
    #[test]
    fn every_version_after_the_floor_has_a_migration() {
        let engine = crate::engine::NativeEngine::open_in_memory().expect("in-memory engine");
        let mut ran = Vec::new();
        let chain = run_chain(UPGRADES_FROM, STORAGE_VERSION, |to| {
            ran.push(to);
            migrate_to(&engine, to)
        });
        assert!(chain.is_ok(), "{chain:?}");
        let expected: Vec<u32> = (UPGRADES_FROM.saturating_add(1)..=STORAGE_VERSION).collect();
        assert_eq!(ran, expected);
        assert!(matches!(
            migrate_to(&engine, STORAGE_VERSION.saturating_add(1)),
            Err(CellError::EngineUpgradeUnsupported { .. })
        ));
    }

    #[test]
    fn the_chain_runs_in_order_and_stops_at_the_first_refusal() {
        let mut ran = Vec::new();
        let result = run_chain(1, 5, |to| {
            ran.push(to);
            if to == 3 {
                Err(CellError::EngineUpgradeUnsupported {
                    cell: 2,
                    engine: 5,
                    reason: "refused".to_owned(),
                })
            } else {
                Ok(())
            }
        });
        assert_eq!(ran, [2, 3]);
        assert!(matches!(
            result,
            Err(CellError::EngineUpgradeUnsupported { cell: 2, .. })
        ));
    }

    #[test]
    fn admission_orders_the_cell_against_the_engine_window() {
        assert_eq!(
            admit(&StoredVersion::Fresh, ENGINE).ok(),
            Some(Admission::Fresh)
        );
        assert_eq!(admit(&stamped(4), ENGINE).ok(), Some(Admission::Current));
        assert_eq!(
            admit(&stamped(3), ENGINE).ok(),
            Some(Admission::Upgrade { from: 3 })
        );
        assert_eq!(
            admit(&stamped(2), ENGINE).ok(),
            Some(Admission::Upgrade { from: 2 })
        );
        assert!(matches!(
            admit(&stamped(5), ENGINE),
            Err(CellError::EngineDowngrade { cell: 5, engine: 4, ref writer }) if writer == "drop-40"
        ));
        assert!(matches!(
            admit(&stamped(1), ENGINE),
            Err(CellError::EngineUpgradeUnsupported {
                cell: 1,
                engine: 4,
                ..
            })
        ));
        assert!(matches!(
            admit(&StoredVersion::Unstamped, ENGINE),
            Err(CellError::EngineUpgradeUnsupported { cell: 1, .. })
        ));
        assert_eq!(
            admit(&StoredVersion::Unstamped, THIS_ENGINE).ok(),
            Some(Admission::Upgrade { from: 1 })
        );
    }

    #[test]
    fn an_export_without_a_storage_version_is_version_1() {
        assert_eq!(
            admit_export(None, THIS_ENGINE).ok(),
            Some(Admission::Upgrade { from: 1 })
        );
        assert!(matches!(
            admit_export(None, ENGINE),
            Err(CellError::EngineUpgradeUnsupported { cell: 1, .. })
        ));
        assert_eq!(admit_export(Some(4), ENGINE).ok(), Some(Admission::Current));
        assert!(matches!(
            admit_export(Some(5), ENGINE),
            Err(CellError::EngineDowngrade {
                cell: 5,
                engine: 4,
                ..
            })
        ));
        assert!(matches!(
            admit_export(Some(0), ENGINE),
            Err(CellError::EngineUnknownVersion { .. })
        ));
    }

    #[test]
    fn an_unreadable_stamp_is_an_unknown_version() {
        for text in [
            "",
            "2",
            "{}",
            r#"{"format":"norn.cell.engine/2","storage":3,"writer":"drop-90"}"#,
            r#"{"format":"norn.cell.engine/1","storage":0,"writer":"dev"}"#,
            r#"{"format":"norn.cell.engine/1","storage":-1,"writer":"dev"}"#,
            r#"{"format":"norn.cell.engine/1","storage":"3","writer":"dev"}"#,
        ] {
            assert!(
                matches!(
                    admit(&StoredVersion::Stamped(text.to_owned()), THIS_ENGINE),
                    Err(CellError::EngineUnknownVersion { .. })
                ),
                "{text}"
            );
        }
        let long = "x".repeat(4_096);
        let Err(CellError::EngineUnknownVersion { found }) =
            admit(&StoredVersion::Stamped(long), THIS_ENGINE)
        else {
            panic!("a long stamp is unknown");
        };
        assert_eq!(found.chars().count(), SHOWN_STAMP_CHARS);
    }

    #[test]
    fn a_stamp_keeps_fields_a_newer_release_adds() {
        let text = format!(
            r#"{{"format":"{STAMP_FORMAT}","storage":{STORAGE_VERSION},"writer":"drop-41","channel":"stable"}}"#
        );
        assert_eq!(
            admit(&StoredVersion::Stamped(text), THIS_ENGINE).ok(),
            Some(Admission::Current)
        );
    }
}
