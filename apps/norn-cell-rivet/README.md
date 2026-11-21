# Norn cells on self-hosted Rivet

This is the deployment recipe for the Rivet host (`crates/norn-cell-rivet`,
D131). It has three parts:

- **Control plane:** three Rivet Engine 3.1.1 instances, each with its own log
  disk, sharing one S3-compatible bucket.
- **Load balancer:** one endpoint in front of the engines.
- **Runners:** two or more `norn-cell-rivet` processes. Each one hosts cell
  actors and serves the public router.

## What an acknowledged commit means

A cell's SQLite runs inside the runner process, on Rivet's VFS. A commit returns
only after it is fsynced on a majority of the instances that hold the cell's
volume. With three instances, losing one instance, or its disk, loses no
acknowledged commit; the kill-loop and disk-loss evidence is on #1130.

The bucket is not on the write path. Data moves to the bucket in the
background, 60 s after a write by default (`storage.write_to_cold_after_ms`).

With one instance, the majority is that single disk. Losing it loses every
commit not yet offloaded. Run at least three instances, in three failure
domains (`storage.zone`).

Rivet's own crash tests do not model power loss or torn writes. The evidence
here is process kills (`kill -9`) and deleted disks.

## Compose (one machine, for trials and tests)

```sh
cp apps/norn-cell-rivet/env.example apps/norn-cell-rivet/.env    # fill it in; never commit it
docker compose -f apps/norn-cell-rivet/compose.yaml --profile s3 up -d --build
```

The stack has these services:

- `engine-1..3`: zones `zone-1..3`. Each advertises `engine-N:6421` to its
  peers.
- `lb`: HAProxy in front of the engines on port 6420. It health-checks
  `/health`, and it listens on loopback only.
- `pool`: creates the `default` pool before any runner connects.
- `runner-1`, `runner-2`: `runner-1` publishes `NORN_PORT` (default 8787).
- `s3`, `s3-bucket` (profile `s3`): a local test bucket, RustFS 1.0.6
  (Apache-2.0).
  - MinIO no longer publishes community images or binaries. Its Docker Hub
    repository is gone, and `dl.min.io` answers 410.
  - The engines reach this bucket over plain http on the compose network, so
    `S3_ALLOW_HTTP=true` is set for it. Never set it for a remote bucket.

Ports:

| Port | What | Exposure |
|---|---|---|
| 8787 | The runners' router: the cell API, MCP and live sockets | Public, behind TLS |
| 6420 | The engines (API, actor traffic, node connections) | Private; loopback in compose |
| 6421 | Instance to instance | Plaintext and unauthenticated: private network only, never exposed |
| 6430 | Engine metrics | Private |

## Three machines (production)

`systemd/` has the units:

- `rivet-engine.service`, with `engine.json.example` as
  `/etc/rivet/config.json`;
- `norn-cell-rivet.service`, with `/etc/norn/cell-rivet.env`.

On each of the three engine machines:

1. Put the log directory on a dedicated block disk (NFS is not supported), at
   least 100 GiB. Rivet refuses to start with less than 10% of the disk, or
   less than 4 GiB, free.
2. Write `/etc/rivet/config.json`. The admin token and the bucket are the same
   on every machine. Each machine sets its own `api_peer.host`,
   `api_peer.advertise_address` (`<private ip>:6421`) and `storage.zone`.
3. Start all three within about a minute of each other. The first storage
   placement then spans all three; the instances find each other through the
   bucket.
4. Check `curl http://<ip>:6420/health`. It returns
   `{"architecture":"v3",…,"status":"ok","version":"3.1.1"}`.

Then:

5. Put a load balancer on port 6420 of every engine, with an idle timeout of
   3600 s. Runners hold one long-lived connection to it.
6. Create the pool:

   ```sh
   curl -X PUT "https://<lb>/pools/default?namespace=default" \
     -H "authorization: Bearer $TOKEN" -H 'content-type: application/json' -d '{}'
   ```

7. Start the runners with
   `RIVET_ENDPOINT=https://default:<token>@<lb>`, plus the Norn variables in
   `env.example`.

### Hetzner Object Storage

Use the `s3` backend with these settings:

- `endpoint: https://fsn1.your-objectstorage.com` (or another location);
- `region: fsn1`;
- the project's access key.

Leave bucket versioning off. Rivet's control plane relies on conditional
writes, and Hetzner does not support them on versioned buckets. Protect the
bucket with replication to a second bucket instead.

Keep the original log disks too. Rivet says that restoring a bucket into a
fresh cluster "has not yet been validated for this release". Never start a
second cluster against a bucket that a running cluster uses.

## Does the open-source engine tier to S3?

Yes. Rivet's Depot docs say the open-source edition lacks S3 cold storage and
offload, and their blog says it has them. The pinned v3.1.1 Apache-2.0 tree
and a run against this compose stack show that offload is in it:

- **The S3 backend is in the open-source tree.**
  - `object_storage.primary` takes `s3`, `gcs`, `azure`, `file_system` or
    `in_memory` (`engine/packages/config/src/config/object_storage.rs`).
  - The engine builds it with `object_store`'s `AmazonS3Builder::from_env()`
    (`engine/packages/engine-context/src/production/object_storage.rs:178-205`).
- **The cold-store tier is compiled in, with no feature gate.**
  - `storage.write_to_cold_after_ms` defaults to 60 s
    (`engine/packages/config/src/config/depot.rs:33-46`, `:224`).
  - The SlateDB cold store, with `object_store`'s `aws` feature, is a
    non-optional native dependency of `depot-worker`
    (`engine/packages/depot-worker/Cargo.toml:47-55`,
    `src/tablet_maintainer_machine/executor/cold_store.rs`).
- **It ran.**
  - The engines kept their cluster configuration, partition manifests,
    compactions and cold SSTs in the bucket.
  - The durability runs on #1130 put an S3 request meter in front of the
    bucket and counted every request the engines made.
- **What the open-source edition lacks:**
  - Rivet's Enterprise page lists "SQLite backup", "SQLite PITR" and
    "Forking" as Enterprise features.
  - Backups here are bucket replication plus the log disks.
  - There is no consistent snapshot or export command.

## Operating limits found in the rehearsal (#1130)

- **Open files:** the engine needs `nofile` far above Docker's default of 1024.
  At 1024, a sustained write load failed the log shard with `EMFILE` and the
  engine fail-stopped. The compose file sets 1048576, as Rivet's systemd unit
  does (`LimitNOFILE`).
- **A dead runner's cells move after `pegboard.node_lost_threshold`** (default
  40 s; measured 44-51 s end to end). A restarted engine is back in 3-10 s.
  Lowering the threshold shortens runner-crash outages, but it has not been
  measured here, and a lower value moves cells off a runner sooner on a long
  pause (the thawed runner is fenced either way).
- **Bulk writes into one actor are rate-limited by the engine's cold-store
  compaction.** On a 4-vCPU machine, sustained 8-64 MiB commits at 20-66 MB/s
  were refused (`write_refused`), and the actor's volume was fenced
  (`staged_write_abort_failed`) and reopened in a new generation. No
  acknowledged write was lost. Past about 4 GiB in one actor, every further
  256 MiB load was fenced. Large dataset loads need a bounded rate (or a
  bigger engine machine) until this is understood upstream.

## Durability and cost evidence

`durability/durability.py` runs on a Boat VM. It brings this stack up with an
S3 request meter in front of the bucket (`durability/compose.meter.yaml`, an
HAProxy that logs every request, counted the way S3 and R2 bill them) and
runs the norn-cell-rivet runners (a `conformance` build) on the machine:

```sh
NORN_CELL_RIVET=crates/norn-cell-rivet/target/release/norn-cell-rivet \
  python3 apps/norn-cell-rivet/durability/durability.py \
  --reference "$P/reference.json" --phase kill,disk,load,latency --out results.json
python3 apps/norn-cell-rivet/durability/durability.py ... --phase idle --project norn-idle --lb-port 6520
```

- `kill`: writers commit governed installs, each acknowledged id fsynced to
  a file; every round SIGKILLs a runner, an engine, both, or everything.
- `disk`: one engine's disk is lost and it restarts empty, while writers run.
- `load` and `idle`: S3 requests for 10,000 commits, and for an idle hour.
- `latency`: warm read and commit p50/p99, and a one-row commit.

The oracle is every acknowledged id present and no id that was never sent,
with each cell's hash chain verified from genesis on reopen. The results are
on #1130.
