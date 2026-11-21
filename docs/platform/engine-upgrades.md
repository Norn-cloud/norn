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
- An open runs the schema step, the migrations, and the verification of the history, the fork pin and an expected cell identity in one transaction. The migrations run before this engine's schema statements, so each step sees the layout it was written for. If any later check fails (a model mismatch, a ledger that does not verify), the upgrade rolls back with it.
- The history is verified from the cell's open checkpoint (storage version 5, OPEN-CHECKPOINT-1, #1533), or from genesis when it has none. See [Checkpointed open](#checkpointed-open).
- Provisioning an existing cell admits it again and runs the migrations at the start of its write transaction, before any other statement, so a second opener that changed the version after the first check is caught.
- A fork admits the copied snapshot and refuses unless it is at this engine's storage version, before it reads the copy's head or writes its pin. A copy is never migrated.

Hosts already turn an open refusal into `cell.unopenable`, and the reason reaches only a definition writer.

## Checkpointed open

From storage version 5 an open does not re-verify every commit from the first one. `_norn_open_checkpoint` holds one row: a ledger head this engine verified (commit count, last event sequence and hash), the RFC 6962 root of the receipt digests of those commits, and the quantity boundary in force at it, so the open never scans the receipts before it to find the boundary. The code is `crates/norn-cell/src/open_checkpoint.rs`.

- **When it is written.** After every 128th commit, once `verified_head` has verified the chain up to it, and at the end of an open that upgraded the cell (that open writes anyway). A same-version open still writes nothing. The row is metadata: it is never part of a receipt, a ledger event or an export, and a failed write keeps the previous checkpoint.
- **What an open checks.** The checkpoint must still anchor the file: the ledger entry at its sequence carries its hash, and its last commit's receipt names that head and verifies. The receipt digests of its commits must rebuild its root, so no receipt it covers was replaced, even one rewritten with its own hash. Everything after it is verified exactly as a genesis open verifies it: the chain and receipts, the definition history, the value history and each export record's cell identity. Object identities are checked over the current state, as before. With no checkpoint the open verifies from genesis; one that does not anchor the file refuses the open.
- **What it does not re-read.** The bodies of rows before the checkpoint. A consistent rewrite of that history changes the anchored head or the receipt root and refuses. An old row changed in place with its stored hashes kept is refused when it is read, because every historical read verifies its own proof (a value version against its receipt's digest, a receipt against its hash). A verification from genesis finds it: an operator's scrub opens a copy of the file with its checkpoint row deleted.
- **What still grows with history.** The open reads every receipt digest once to rebuild the transparency log in memory (about a microsecond per commit), and the definition history reads every installed version.

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

## Release channel

Releases are the `drop-N` tags. Packaging (installers, `cargo-dist`, `NORN_RELEASE` set in release builds, the storage window in release notes) is post-beta work, tracked in #1482.

- A host pins one drop. Each drop has a storage window: the version it writes and the oldest it upgrades.
- To upgrade, the host moves its pin forward. The first open on the new drop runs the migrations. The host can skip drops as long as the target drop's floor is at or below the cell's storage version. If not, the refusal says to open the cell first on a bridge drop.
- Before moving the pin across a storage version bump, the host keeps a snapshot it can return to: a copy of the file for the native host, a point-in-time bookmark for the Durable Object host, the actor's SQLite snapshot for Rivet.
- Rolling back between drops with the same storage version is allowed, because they are interchangeable for a cell. Rolling back across a bump refuses `core.engine.downgrade`. The way back is the snapshot. Once the newer drop has committed, the cell's export holds records at the newer storage version, and restoring it into the older drop refuses as well.

Limit: storage version 1 engines (every drop before the first one with the stamp) don't read the stamp, so they cannot refuse a newer cell. Storage version 2 lays out data exactly as version 1 does, so such an engine reads a version 2 cell correctly. Once version 3 exists, a host must not pin back past the first stamped drop. From the first stamped drop on, every engine refuses a cell newer than itself.
