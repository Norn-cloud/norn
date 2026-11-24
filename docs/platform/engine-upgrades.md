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
- The history is verified from the cell's open checkpoint (storage version 6, OPEN-CHECKPOINT-1, #1533), or from genesis when it has none. See [Checkpointed open](#checkpointed-open).
- Provisioning an existing cell admits it again and runs the migrations at the start of its write transaction, before any other statement, so a second opener that changed the version after the first check is caught.
- A fork admits the copied snapshot and refuses unless it is at this engine's storage version, before it reads the copy's head or writes its pin. A copy is never migrated. A fork is also refused while a checkpointed open's history scrub is incomplete (`cell.verifying`) or while the cell is fenced (`cell.fenced`), so a fork only copies scrub-verified history.

Hosts already turn an open refusal into `cell.unopenable`, and the reason reaches only a definition writer.

## Checkpointed open

From storage version 6 an open does not re-verify every commit from the first one. `_norn_open_checkpoint` holds one row: a ledger head this engine verified (commit count, last event sequence and hash), the RFC 6962 root of the receipt digests of those commits, the quantity boundary in force at it, a digest of the `(commit, receipt digest)` list of the quantity receipts it covers, the commit and stored digest of the last export record it covers, a digest of every retained definition version it covers, and a MAC. The code is `crates/norn-cell/src/open_checkpoint.rs` (the row, its MAC, durable fence and the genesis-parity table), `crates/norn-cell/src/open_checkpoint/verify.rs` (verification and checkpoint advancement) and `crates/norn-cell/src/scrub.rs` (the history scrub).

### Claim and threat model

- **Who we defend against:** anyone who can rewrite the cell file (its rows, digests and the checkpoint row together) but does not hold the host's checkpoint key.
- **The claim:** a checkpointed cell detects every edit a genesis open detects. Edits to live state are detected at open. Edits to history are detected within one scrub, before any read that depends on that history being complete is served.
- **Forging a checkpoint needs the key.** The row is authenticated by HMAC-SHA256 under a key held outside the cell file: the host's secret store (OpenBao or a hosted secret store in alpha, `NORN_CHECKPOINT_KEY` on the Durable Object host). The dev host makes a random key on first use and keeps it beside the cell, in `.norn/checkpoint.key` (owner-only), never in the cell file. Tests draw a random key per process. No key is written into the source. The MAC covers every column and the cell's identity binding, so a row copied from another cell does not verify either.
- **Fail-safe:** a missing key, a cell with no identity, a malformed or unparseable checkpoint row, or a row whose MAC does not verify gives a genesis open, never a refusal. The same holds for a checkpoint past the end of the file, as after a restore that trimmed history. A lost or rotated key costs open time, not the cell.
- **Not claimed:** a rollback of the whole file to an older consistent state, which a genesis open does not detect either. An attacker who holds the key. Proving history complete without reading it; a per-row authenticated index (a Merkle index over value versions and selectors) is a post-beta option.

### What an open checks

An open from an authentic checkpoint verifies all live state, and the history after the checkpoint, exactly as a genesis open does:

- **The anchor.** The ledger entry at the checkpoint's sequence carries its hash, and its last commit's receipt names that head and verifies. The receipt digests of the commits it covers rebuild its root.
- **The suffix.** Everything after the checkpoint: the chain, receipts and their logical time order, the definition history, the value history and each export record's cell identity.
- **Live state, in full:**
  - The object identities, over the current state.
  - The cell identity binding against the last export record the checkpoint pins.
  - The definitions: every retained version it covers folds to its definitions digest, and the version active at it is proven again against its typed export record.
  - The quantity projections, replayed from every quantity receipt and compared row for row. The receipt table's quantity indexes list those receipts, and the list must fold to the checkpoint's quantity digest.
  - The schema, storage version, markings and fork pin.

### The history scrub

The history before the checkpoint is verified after the open, by the scrub. It runs the checks a genesis open runs, from genesis to the head the open verified, in bounded steps between commits:

1. **Receipts:** each body against its digest, its lookup columns against its body, each digest against the one the open loaded, and the rebuilt root against the checkpoint's.
2. **History:** the ledger chain, every export record's identity, every definition install (superseded ones included), and every value version against its receipt.
3. **Seed:** the checkpoint at that head, built only from what the scrub verified, MAC'd and recorded.

Placement:

- The scrub runs on the cell's own connection, one step per wake of the host's due-work clock (`norn_api::timers`): the Durable Object's alarm, and the dev host's timer loop.
- A step reads at most 512 receipts or verifies 64 commits, each step in its own transaction, so a commit waits for at most one step.
- A separate read connection over a WAL snapshot was considered and not used. A Durable Object has one storage connection and no threads, and an in-memory cell cannot be opened twice, so one stepped scrub serves every host.
- On a Durable Object the scrub's state lives in memory. An evicted object scrubs again on its next open, and its alarm keeps it awake while the scrub runs.

Until the scrub ends, reads that depend on history being complete are refused with the retryable `cell.verifying`. Live reads and writes are served.

| Held back until | Reads |
|---|---|
| the history phase ends | as-of reads (`version_as_of`, knowledge-cutoff evidence), historical frames and analytics/compute queries over history (gated before population selection), version lists (object history), `ledger()`, ledger and finding-ledger pages that reach an entry the checkpoint covers, export and transparency reads covering checkpointed history, and `fork()` |
| the receipts phase ends | a commit or `receipt_by_idempotency_key` lookup whose idempotency key finds no receipt: the miss cannot be dated, and a changed selector could hide a covered receipt, so the miss is refused as retryable rather than treated as absent or committed twice |

Each scrub step stages its progress and applies it to in-memory state only after its transaction commits, so a transient SQLite failure never loses or corrupts scrub state. A defect (or exhausted transient retries) fences the cell durably: the checkpoint row is dropped and `_norn_meta` records `open_checkpoint:fenced` in one transaction, so every commit, read, fork, sibling handle and subsequent reopen fails closed with `cell.fenced`. The finding is logged and returned by the host's due-work report.

### When a checkpoint is written

- **By a commit.** After every 128th commit, and only from the checkpoint this handle validated, once the scrub has verified the history it covers. Inside the checkpoint's own transaction, the commit re-checks the tail anchor against concurrent writes, re-runs the pins an open runs on that checkpoint, verifies everything it newly covers, and builds the new root only from digests equal to the ones on disk. A commit verifies at most 256 commits. A longer gap is handed to the scrub, which closes it off the commit path and seeds the next checkpoint. A commit never runs a catch-up from genesis. A history of 256 commits or fewer is checkpointed from genesis by its first due commit.
- **By the scrub.** At the end of every scrub, at the head it verified. That covers after a checkpointed open, after a genesis open of a history over 256 commits (where the scrub only rebuilds what the seed records), and after a restore.
- **By an upgrading open,** which writes anyway.
- **Never otherwise.** A same-version open writes nothing. The row is metadata: it is never part of a receipt, a ledger event or an export. A failed write keeps the previous checkpoint.

### Restores

- **Whole-file snapshots** (a file copy, a Durable Object point-in-time bookmark, a Rivet actor snapshot) restore the checkpoint with the history it covers, and are consistent.
- **A restore that trims history** (an LTX-cut point-in-time restore) may leave a checkpoint past its end. The open then verifies from genesis, and the scrub re-seeds the checkpoint at the restored head.
- **The earlier runbook step** `DELETE FROM "_norn_open_checkpoint" WHERE "commits" > ?cut` is no longer needed, and stays harmless.
- **This repository's restore** (`restore.rs`) replays an export into a new file, so it never carries a checkpoint over.

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
2. **An engine refuses cells stamped by a different RivetKit version unless that version is in its allowed-upgrade list.** The list is `RIVETKIT_CHANNEL` in `crates/norn-cell-rivet/src/config.rs`: each RivetKit release and the first storage version written under it. A cell written under a newer RivetKit carries a newer storage version, so it refuses `core.engine.downgrade` before any Norn code touches it. The Rivet host admits cells from `ADMISSION_FLOOR`, the lowest `from_storage` of the retained list (or `UPGRADES_FROM`, if higher), up to `STORAGE_VERSION`. Dropping the oldest releases retires them: the floor rises past their storage versions, and a cell stamped below it fails its start with `core.engine.upgrade_unsupported`, even though the shared engine alone would upgrade it.
3. **Rollback across a RivetKit version is unsupported.** The older RivetKit may already have been run over by the newer one's migrations. The way back is a backup or a point-in-time restore taken before the upgrade, never a re-pin.

Two checks in `scripts/ci/core.sh rivet` enforce this:

- **Against the base:** `scripts/ci/rivetkit_channel.py` (its cases are in `scripts/ci/test_rivetkit_channel.py`) compares the working tree with the merge-base of `NORN_CI_BASE` (default `origin/main`). It fails closed when that base cannot be resolved, and it requires:
  - exactly one `Cargo.lock` entry each for `rivetkit` and `rivetkit-core`, from the crates.io registry, with a checksum;
  - a channel that is append-only: the base's, with only its oldest entries dropped and new ones appended;
  - when either crate's locked version, source or checksum differs from the base's, exactly one new entry for `RIVETKIT_VERSION` at the head's `STORAGE_VERSION`, which must be above the base's;
  - no new entry when neither crate changed.
- **In the tree:** the unit test `config::tests::rivetkit_pin_matches_the_lock_and_the_storage_version` fails when `Cargo.lock`'s `rivetkit` or `rivetkit-core` differs from `RIVETKIT_VERSION`, when the last `RIVETKIT_CHANNEL` entry is not that version, and when an entry does not start at a storage version above the previous one.

The real fix, a RivetKit hook that admits the cell before RivetKit's own migrations plus a conformance case with a pending Rivet schema migration, is post-beta work (#1550).

## Release channel

Releases are the `drop-N` tags. Packaging (installers, `cargo-dist`, `NORN_RELEASE` set in release builds, the storage window in release notes) is post-beta work, tracked in #1482.

- A host pins one drop. Each drop has a storage window: the version it writes and the oldest it upgrades.
- To upgrade, the host moves its pin forward. The first open on the new drop runs the migrations. The host can skip drops as long as the target drop's floor is at or below the cell's storage version. If not, the refusal says to open the cell first on a bridge drop.
- Before moving the pin across a storage version bump, the host keeps a snapshot it can return to: a copy of the file for the native host, a point-in-time bookmark for the Durable Object host, the actor's SQLite snapshot for Rivet.
- Rolling back between drops with the same storage version is allowed, because they are interchangeable for a cell. Rolling back across a bump refuses `core.engine.downgrade`. The way back is the snapshot. Once the newer drop has committed, the cell's export holds records at the newer storage version, and restoring it into the older drop refuses as well.

Limit: storage version 1 engines (every drop before the first one with the stamp) don't read the stamp, so they cannot refuse a newer cell. Storage version 2 lays out data exactly as version 1 does, so such an engine reads a version 2 cell correctly. Once version 3 exists, a host must not pin back past the first stamped drop. From the first stamped drop on, every engine refuses a cell newer than itself.
