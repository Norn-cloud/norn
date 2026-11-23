# Norn cells on Rivet

The runner image packages the native Rust cell actor and the Norn HTTP
router. It connects to the Rivet control plane at `RIVET_ENDPOINT`.
The control plane owns the actor database. Replacing a runner does not
replace the cell's SQLite data.

## Hosted Actor Nodes

Use the repository root as the build context and select
`apps/norn-cell-rivet/Dockerfile`. The image listens on the platform's
`PORT` (8787 for a plain Docker run). Rivet Cloud supplies
`RIVET_ENDPOINT`, `RIVET_TOKEN` and `RIVET_POOL`. Set the Norn variables
listed in the [runner reference](../../crates/norn-cell-rivet/README.md#running).
Keep the engine connection on HTTPS and use the production image, which
builds without the `conformance` feature.

Confirm the cluster's engine is compatible with RivetKit 3.1.1, then
rehearse cell reads, writes, and data persistence across a runner replacement
before routing live traffic to it. See the
[Rivet Cloud Actor Node guide](https://rivet.dev/docs/deploy/cloud/compute/).

## Local rehearsal

The Compose stack runs three pinned Rivet Engine 3.1.1 instances with
separate log volumes, HAProxy, two Norn runners and a test object store.
The runner and engine ingress bind to loopback. Engine peer traffic and
the plaintext runner connection stay on the Compose bridge.

```sh
cp apps/norn-cell-rivet/env.example apps/norn-cell-rivet/.env
# Fill in the local test credentials.
docker compose -f apps/norn-cell-rivet/compose.yaml --profile s3 up -d --build
```

The repository's `.dockerignore` excludes local credentials and build
output from the image build context.

For the rehearsal's three engine instances, a commit is acknowledged after
the majority holding the cell volume persists the write. The object store
receives cold data asynchronously. The harness samples process kills and
loss of one log volume. All three instances share the same machine and the
results do not establish availability across failure domains. The one-node
Rivet Cloud Free plan also cannot demonstrate a three-node quorum.

## Durability and cost evidence

`durability/durability.py` runs on a Boat VM. It brings this stack up with an
S3 request meter in front of the bucket (`durability/compose.meter.yaml`, an
HAProxy that logs every request and estimates R2 operation counts) and
runs the `norn-cell-rivet` runners (a `conformance` build) on the machine:

```sh
NORN_CELL_RIVET=crates/norn-cell-rivet/target/release/norn-cell-rivet \
  python3 apps/norn-cell-rivet/durability/durability.py \
  --reference "$P/reference.json" --phase kill,disk,load,latency --out results.json
NORN_CELL_RIVET=crates/norn-cell-rivet/target/release/norn-cell-rivet \
  python3 apps/norn-cell-rivet/durability/durability.py \
  --reference "$P/reference.json" --phase idle --project norn-idle --lb-port 6520
```

- `kill`: writers commit governed installs, each acknowledged id fsynced to
  a file; every round SIGKILLs a runner, an engine, both, or everything.
- `disk`: one engine's disk is lost and it restarts empty, while writers run.
- `load` and `idle`: S3 requests for 10,000 commits, and for an idle hour.
- `latency`: warm read and commit p50/p99, and a one-row commit.

The oracle is every acknowledged id present and no id that was never sent,
with each cell's hash chain verified from genesis on reopen. The results are
on #1130.
