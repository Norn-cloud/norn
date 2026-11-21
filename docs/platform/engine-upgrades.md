# Engine upgrades and the release channel

How a running cell moves to a newer engine, and how a host chooses which engine it runs (ENGINE-UPGRADE-1, #1494). The code is `crates/norn-cell/src/engine_release.rs`. Definition upgrades are a separate path: an additive definition change is checked against the cell's live data at install time (`migration.rs`).

## Storage version

Every cell records the **storage version** of its data in the `_norn_meta` row `engine`:

```json
{"format":"norn.cell.engine/1","storage":2,"writer":"drop-30"}
```

- `storage` is the compatibility contract between a cell and an engine. Any change to what an engine writes that an older engine could misread bumps it, and each bump comes with one migration step. Storage version 1 is every `norn.cell.store/8` cell written before the stamp existed. Those cells have no `engine` row.
- `writer` is the release that wrote this storage version: `NORN_RELEASE` at build time (for example `drop-30`), or `dev`. It is only a label. A downgrade refusal names it so the operator knows which release to pin. Admission never compares writers.
- `format` versions the stamp's own encoding. A later release may add fields to `/1`, and older engines ignore them. The tag changes only when older engines must not read the stamp at all.

`norn.cell.store/8` stays the format family and never changes for a cell that can be upgraded, because ledger events name it (the quantity boundary event, for example). Formats `/2` to `/7` still refuse with "recreate the cell".

The version metadata the engine checks is compiled in: `CHANNEL` lists each storage version and what changed, `STORAGE_VERSION` is the version this engine writes, and `UPGRADES_FROM` is the oldest version it upgrades in place.

## What an open does

`CellStore` reads the stamp before any schema statement, definition decoding, host model selection or history verification. `validate_store_format` makes this check, and every open path goes through it: `from_engine*`, `provision_definitions*`, `read_binding`, `read_active_definitions`, and the read-only `diagnose_history`.

| Cell | Result |
|---|---|
| no store | created and stamped with this engine's version |
| same storage version | opens with no migration and no write |
| older, at or above `UPGRADES_FROM` | runs the steps `from + 1 ..= STORAGE_VERSION` in order, then restamps |
| older than `UPGRADES_FROM` | refuses `core.engine.upgrade_unsupported` |
| newer storage version | refuses `core.engine.downgrade`, naming the cell's version and writer |
| stamp this engine cannot read (future `format`, a non-integer, 0, or a version too large to count) | refuses `core.engine.unknown_version` |

A refused or failed open leaves the file byte-identical. Nothing is committed until every check that can refuse the file has passed:

- The native engine (`NativeEngine::open`) admits the storage version before it sets the journal mode, which SQLite stores in the file.
- An open runs the schema step, the migrations, and the verification of the whole ledger, every receipt, the definition history, the fork pin and an expected cell identity in one transaction. The migrations run before this engine's schema statements, so each step sees the layout it was written for. If any later check fails (a model mismatch, a ledger that does not verify), the upgrade rolls back with it.
- Provisioning an existing cell admits it again and runs the migrations at the start of its write transaction, before any other statement, so a second opener that changed the version after the first check is caught.
- A fork admits the copied snapshot and refuses unless it is at this engine's storage version, before it reads the copy's head or writes its pin. A copy is never migrated.

Hosts already turn an open refusal into `cell.unopenable`, and the reason reaches only a definition writer.

## Exports and restore

Each export record carries `storageVersion`, the storage version of the engine that committed it. It is part of the record's content address, so it is covered by the same verification as the rest of the export. A replayed record keeps the version that first wrote it, so a restore reproduces the original export journal exactly.

A record with no `storageVersion` was written before the field existed and counts as storage version 1. A restore admits every record of the export against its engine's storage window before it stages anything: a record newer than the engine refuses `core.engine.downgrade`, one older than `UPGRADES_FROM` refuses `core.engine.upgrade_unsupported`, and `0` refuses `core.engine.unknown_version`. An admitted older export is rebuilt through this engine's commit path, so the restored file is written, and stamped, at this engine's storage version.

The refusal codes are registered in `crates/norn-checker/conformance/diagnostic-codes/` at tier T6.

The check lives in `norn-cell`, below the host seam. The native host, the Durable Object host and the Rivet host (`norn-cell-rivet`, #1130) all open through `CellStore`, so all three run the same check and need no host code for it.

## Adding a storage version

1. Before changing what the engine writes, record a fixture on the current engine: `tests/engine_fixture_record.rs` writes `crates/norn-cell/tests/fixtures/engine-storage-v<N>.json`.
2. Append `CHANNEL` entry `N + 1`, bump `STORAGE_VERSION`, and add the `N + 1` arm to `migrate_to`. The unit tests refuse a channel with gaps or a version with no step.
3. Make `tests/engine_upgrade.rs` open the new fixture as well: the ledger and receipts must be unchanged, every recorded commit must replay to its recorded receipt, and the test lists every row the step changes.
4. To retire old steps, raise `UPGRADES_FROM`. A cell older than the new floor must first be opened on a bridge release, one whose floor still admits it.

A step may only rewrite derived or physical state: projections, indexes, metadata. Ledger entries and receipts are evidence, and no step rewrites them. If a change would need to rewrite them, it is a new store format, not a storage version.

## RivetKit upgrades

The Rivet host runs a cell on RivetKit's own SQLite, and RivetKit may commit its internal schema migrations when an actor starts, before `on_start` reads the Norn stamp. A refused cell is then still written if the restart also brings a new RivetKit. RivetKit's public API has no hook that runs before those migrations, so the Rivet host rules out the combination by policy:

1. **A RivetKit version change ships only together with a Norn storage version bump.** The release that moves `rivetkit` also adds a `CHANNEL` entry and raises `STORAGE_VERSION`, even when Norn's own layout is unchanged.
2. **An engine refuses cells stamped by a different RivetKit version unless that version is in its allowed-upgrade list.** The list is `RIVETKIT_CHANNEL` in `crates/norn-cell-rivet/src/config.rs`: each RivetKit release and the first storage version written under it. A cell written under a newer RivetKit carries a newer storage version, so it refuses `core.engine.downgrade` before any Norn code touches it. A cell from an older RivetKit opens only if that release is still listed and its storage versions are at or above `UPGRADES_FROM`. Dropping a release from the list also raises `UPGRADES_FROM` past it.
3. **Rollback across a RivetKit version is unsupported.** The older RivetKit may already have been run over by the newer one's migrations. The way back is a backup or a point-in-time restore taken before the upgrade, never a re-pin.

The unit test `config::tests::rivetkit_pin_matches_the_lock_and_the_storage_version` (run by `scripts/ci/core.sh rivet`) enforces this. It fails when `Cargo.lock`'s `rivetkit` or `rivetkit-core` differs from `RIVETKIT_VERSION`, when the last `RIVETKIT_CHANNEL` entry is not that version, and when a new entry does not start at a storage version above the previous one. So a RivetKit bump cannot land without a storage version bump.

The real fix, a RivetKit hook that admits the cell before RivetKit's own migrations plus a conformance case with a pending Rivet schema migration, is post-beta work (#1550).

## Release channel

Releases are the `drop-N` tags. Packaging (installers, `cargo-dist`, `NORN_RELEASE` set in release builds, the storage window in release notes) is post-beta work, tracked in #1482.

- A host pins one drop. Each drop has a storage window: the version it writes and the oldest it upgrades.
- To upgrade, the host moves its pin forward. The first open on the new drop runs the migrations. The host can skip drops as long as the target drop's floor is at or below the cell's storage version. If not, the refusal says to open the cell first on a bridge drop.
- Before moving the pin across a storage version bump, the host keeps a snapshot it can return to: a copy of the file for the native host, a point-in-time bookmark for the Durable Object host, the actor's SQLite snapshot for Rivet.
- Rolling back between drops with the same storage version is allowed, because they are interchangeable for a cell. Rolling back across a bump refuses `core.engine.downgrade`. The way back is the snapshot. Once the newer drop has committed, the cell's export holds records at the newer storage version, and restoring it into the older drop refuses as well.

Limit: storage version 1 engines (every drop before the first one with the stamp) don't read the stamp, so they cannot refuse a newer cell. Storage version 2 lays out data exactly as version 1 does, so such an engine reads a version 2 cell correctly. Once version 3 exists, a host must not pin back past the first stamped drop. From the first stamped drop on, every engine refuses a cell newer than itself.
