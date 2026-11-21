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

The runner image builds from the repository root. The root `.dockerignore`
keeps `.env` files, keys and other credential files, `.git`, `target/` and
`node_modules/` out of the build context, so the filled-in `.env` never reaches
a layer (checked on Boat: a build with a dummy `.env` beside the compose file
has no such file in its context or image).

The stack has these services:

- `engine-1..3`: zones `zone-1..3`. Each advertises `engine-N:6421` to its
  peers.
- `lb`: HAProxy in front of the engines on port 6420. It health-checks
  `/health`, and it listens on loopback only. It finds the engines through
  Docker's DNS at run time, so a recreated engine with a new address rejoins
  without an HAProxy restart.
- `pool`: creates the `default` pool before any runner connects.
- `runner-1`, `runner-2`: `runner-1` publishes `NORN_PORT` (default 8787).
  They reach `lb` over plain http on the compose network, so the token stays
  on this machine; they say so with `NORN_RIVET_PLAINTEXT_ENGINE=single-host`.
  Without it the runner refuses a plaintext endpoint to anything but loopback.
  More than one machine means TLS ("Transport security").
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
| 6420 | The engines (API, actor traffic, node connections) | Private; loopback in compose; TLS on every hop between machines |
| 6421 | Instance to instance | Plaintext and unauthenticated: private network only, never exposed |
| 6430 | Engine metrics | Private |

## Three machines (production)

`systemd/` has the units and configurations:

- `rivet-engine.service`, with `engine.json.example` as
  `/etc/rivet/config.json`;
- `haproxy-engine.cfg`: TLS in front of each engine, on the engine's machine;
- `haproxy-lb.cfg`: the private load balancer, TLS on both sides;
- `norn-cell-rivet.service`, with `/etc/norn/cell-rivet.env`.

The machines share one private network. The addresses below are examples;
use your own and keep them consistent:

| Machine | Private address | Listens on |
|---|---|---|
| engine-1, engine-2, engine-3 | 10.0.0.11, .12, .13 | 6420 (TLS, in front of the engine on 127.0.0.1:6420), 6421 (peers), 6430 (metrics), private address only |
| load balancer (private only, no public interface) | 10.0.0.2 | 6420 |
| runners | 10.0.0.21, .22 | 8787, behind the public TLS proxy |
| monitoring (Prometheus) | 10.0.0.30 | |
| operator bastion (or WireGuard peer) | 10.0.0.40 | |

What may reach each engine port:

- **6420** (API, actor traffic, node connections): the private load balancer
  and the operator bastion only, over TLS with a client certificate (see
  "Transport security"). Runners reach it through the load balancer, never
  directly. Every route needs the admin token; the Boat probe below shows the
  refusals.
- **6421** (instance to instance): the other two engines only. It is
  plaintext and unauthenticated, so the network is its only protection.
- **6430** (metrics): the monitoring host and the operator bastion only.
- Nothing on the public interface. Outbound HTTPS (443) to the bucket stays
  open.

On each of the three engine machines:

1. Put the log directory on a dedicated block disk (NFS is not supported), at
   least 100 GiB. Rivet refuses to start with less than 10% of the disk, or
   less than 4 GiB, free.
2. Write `/etc/rivet/config.json`. The admin token and the bucket are the same
   on every machine. Each machine sets its own `storage.zone`. The guard
   (6420) binds loopback only (`guard.host: 127.0.0.1`): the token reaches it
   only through the TLS proxy on the same machine. The other listeners bind
   the machine's own private address: `metrics.host` (6430), `api_peer.host`
   (6421) and `api_peer.advertise_address` (`<private ip>:6421`). Left unset,
   `guard.host` and `metrics.host` bind `[::]`, every interface, including
   the public one.
3. Issue the engine's certificate and install `haproxy-engine.cfg` as
   `/etc/haproxy/haproxy.cfg`, with this machine's private address on the
   `bind` line ("Transport security" below). Install the firewall below
   before starting the engine.
4. Start all three within about a minute of each other. The first storage
   placement then spans all three; the instances find each other through the
   bucket.
5. Check the bindings and the health:

   ```sh
   ss -ltnp '( sport = :6420 or sport = :6421 or sport = :6430 )'
   # rivet-engine on 127.0.0.1:6420, 10.0.0.1N:6421 and 10.0.0.1N:6430; haproxy on 10.0.0.1N:6420
   curl --cacert /etc/rivet/tls/ca.pem --cert /etc/rivet/tls/bastion.pem https://10.0.0.1N:6420/health   # from the bastion
   # {"architecture":"v3",…,"status":"ok","version":"3.1.1"}
   ```

   Without the client certificate the handshake fails, and plain `http://`
   gets no answer from the engine.

   From a machine that is not allowed (a runner, say), the connections must
   time out: `nc -zv -w 3 10.0.0.11 6421` and `nc -zv -w 3 10.0.0.11 6430`.

### Engine firewall

nftables, `/etc/nftables.conf` on engine-1 (on engine-2 and engine-3 the
`peers` set holds the other two addresses). `priv` is the private network's
interface: on Hetzner Cloud it is `enp7s0` on current server types (`ens10` on
older ones); check with `ip -br addr` and set it to the interface holding
10.0.0.1N. Every exception is on that interface only, so the same source
address arriving on the public interface is dropped. The table is `inet`, so
IPv6 is filtered by the same policy: no IPv6 exception exists, and every IPv6
packet but ICMPv6 and replies is dropped. Load it with
`systemctl enable --now nftables`:

```nft
#!/usr/sbin/nft -f
flush ruleset

define priv = "enp7s0"

table inet rivet {
  set lb         { type ipv4_addr; elements = { 10.0.0.2 } }
  set operators  { type ipv4_addr; elements = { 10.0.0.40 } }
  set peers      { type ipv4_addr; elements = { 10.0.0.12, 10.0.0.13 } }
  set monitoring { type ipv4_addr; elements = { 10.0.0.30 } }

  chain input {
    type filter hook input priority filter; policy drop;
    ct state established,related accept
    ct state invalid drop
    iif lo accept
    meta l4proto { icmp, ipv6-icmp } accept
    iifname $priv tcp dport 22   ip saddr @operators  accept
    iifname $priv tcp dport 6420 ip saddr @lb         accept
    iifname $priv tcp dport 6420 ip saddr @operators  accept
    iifname $priv tcp dport 6421 ip saddr @peers      accept
    iifname $priv tcp dport 6430 ip saddr @monitoring accept
    iifname $priv tcp dport 6430 ip saddr @operators  accept
  }
  chain forward { type filter hook forward priority filter; policy drop; }
  chain output  { type filter hook output priority filter; policy accept; }
}
```

The same rules with ufw, on engine-1 (`enp7s0` as above):

```sh
grep -qx 'IPV6=yes' /etc/default/ufw || { echo 'set IPV6=yes in /etc/default/ufw first'; exit 1; }
ufw default deny incoming
ufw default deny routed
ufw default allow outgoing
ufw allow in on enp7s0 from 10.0.0.40 to 10.0.0.11 port 22   proto tcp
ufw allow in on enp7s0 from 10.0.0.2  to 10.0.0.11 port 6420 proto tcp
ufw allow in on enp7s0 from 10.0.0.40 to 10.0.0.11 port 6420 proto tcp
ufw allow in on enp7s0 from 10.0.0.12 to 10.0.0.11 port 6421 proto tcp
ufw allow in on enp7s0 from 10.0.0.13 to 10.0.0.11 port 6421 proto tcp
ufw allow in on enp7s0 from 10.0.0.30 to 10.0.0.11 port 6430 proto tcp
ufw allow in on enp7s0 from 10.0.0.40 to 10.0.0.11 port 6430 proto tcp
ufw enable
ufw status verbose | grep -q 'deny (incoming), allow (outgoing), deny (routed)'   # all three defaults
ip6tables -S ufw6-user-input >/dev/null                                           # IPv6 filtering is on
```

With `IPV6=no`, ufw loads no IPv6 rules at all, and every IPv6 port is open;
the first line refuses to go on in that case.

These rules hold because the engines run under systemd. A process in Docker
with published ports bypasses both nftables input rules and ufw, so never
publish 6420, 6421 or 6430 from a container on these machines. Add the same
allowlist as a Hetzner Cloud Firewall on the public interface (inbound: none,
or SSH from the bastion only).

Then:

6. Put the private load balancer in front of port 6420 of every engine:
   `haproxy-lb.cfg` as `/etc/haproxy/haproxy.cfg` on 10.0.0.2, with its server
   certificate and its client certificate ("Transport security"). It checks
   `GET /health` on each engine over TLS and allows idle connections for
   3600 s; runners hold one long-lived connection to it. The load balancer's
   firewall accepts 6420 on its private interface from the runners and the
   bastion only.
7. Create the pool from the bastion:

   ```sh
   curl --cacert /etc/rivet/tls/ca.pem -X PUT "https://10.0.0.2:6420/pools/default?namespace=default" \
     -H "authorization: Bearer $TOKEN" -H 'content-type: application/json' -d '{}'
   ```

8. Start the runners with
   `RIVET_ENDPOINT=https://default:<token>@10.0.0.2:6420` and
   `SSL_CERT_FILE=/etc/norn/rivet-ca.pem` (the private CA), plus the Norn
   variables in `env.example`. The runner refuses a plaintext `http://`
   endpoint to anything but loopback. The runners' firewall accepts 8787 from
   the TLS proxy only.

### Transport security

The admin token travels on three hops between machines, and each is TLS from
the cluster's own private CA:

| Hop | Client | Server | Client proves itself with |
|---|---|---|---|
| bastion → load balancer | `curl --cacert ca.pem` | `lb.pem` (IP 10.0.0.2) | the admin token |
| runner → load balancer | the runner (`https://`, `SSL_CERT_FILE`) | `lb.pem` | the admin token (the node socket's handshake) |
| load balancer → engine | HAProxy, `verify required`, `verifyhost engine-N.rivet.internal` | `engine.pem` on the engine's machine | `lb-client.pem` and the token |
| bastion → engine (rebuild check) | `curl --cacert ca.pem --cert bastion.pem` | `engine.pem` | `bastion.pem` and the token |

On the engine's machine the proxy forwards to the guard on 127.0.0.1, so the
only cleartext copy of the token is on loopback. 6421 and 6430 carry no token
(6421 is unauthenticated, 6430 is metrics); the firewall is their protection.
The runner's TLS client reads trust roots from the system store and from
`SSL_CERT_FILE` (rustls-native-certs, in RivetKit's socket and HTTP clients),
so `SSL_CERT_FILE=/etc/norn/rivet-ca.pem` trusts the private CA.

**Rehearsed on Boat** (2026-11-21, `haproxy-engine.cfg` and `haproxy-lb.cfg`
as committed, the certificates made with the commands below). Each machine
was a container at its README address on one private bridge: three engines
with their TLS proxies, the load balancer, two runners and the bastion.
`tcpdump` ran on that bridge for the whole run, which covered pool creation,
`/namespaces` through the load balancer and on each engine, both runners
joining, and 10 actor commits and reads through each runner:

- 19,479 packets, 266 TLS handshakes to 6420, no cleartext HTTP request line
  to 6420, and the 64-character token **0 times** in the capture.
- Control: one request with the token sent in cleartext to 6421 put it in a
  second capture once, so a cleartext token would have been seen.
- Each engine refused a client without a certificate and one with a
  certificate from another CA (TLS handshake failure, curl exit 56). Plain
  `http://` to 6420 got no HTTP answer (exit 52).
- A runner given `RIVET_ENDPOINT=http://…@10.0.0.2:6420` refused to start.
- Both runners served every actor through the load balancer, and the same
  actor instance answered from either runner.

The firewall recipes were rehearsed the same day in network namespaces: an
engine with a private (`enp7s0`) and a public interface, and clients using the
load balancer's source address on each. With the nftables ruleset or the ufw
commands above, 6420 answered only on the private interface; 22, 6421 and 6430
were dropped for that source; and every IPv6 SYN was dropped, on both
interfaces. With no firewall, all of them answered. With `IPV6=no`, the ufw
recipe stopped at its first line.

**Issuing certificates.** The CA key lives in Infisical
(`/ops/rivet/RIVET_TLS_CA_KEY`, with the certificate at
`/ops/rivet/RIVET_TLS_CA_CERT`) and is only ever written to a tmpfs on the
bastion while signing. Every other key is made on the machine that uses it and
never leaves it; only certificate requests and certificates move.

```sh
# Once: the CA (EC P-256, 2 years), on the bastion in a tmpfs; store both files in Infisical, then wipe them.
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 730 \
  -subj '/CN=norn-rivet private CA' \
  -addext 'basicConstraints=critical,CA:TRUE,pathlen:0' -addext 'keyUsage=critical,keyCertSign,cRLSign' \
  -keyout ca.key -out ca.pem

# On the machine that will hold the key (engine-1 here):
openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -subj '/CN=engine-1.rivet.internal' \
  -keyout /etc/rivet/tls/engine.key -out engine-1.csr
chmod 600 /etc/rivet/tls/engine.key

# On the bastion, sign it for 90 days. The SAN and usage come from here, never from the request:
openssl x509 -req -in engine-1.csr -CA ca.pem -CAkey ca.key -days 90 -out engine-1.crt -extfile <(printf '%s\n' \
  'subjectAltName=DNS:engine-1.rivet.internal,IP:10.0.0.11' 'extendedKeyUsage=serverAuth' \
  'basicConstraints=critical,CA:FALSE' 'keyUsage=critical,digitalSignature')

# Back on engine-1: HAProxy takes the certificate and key in one file.
cat engine-1.crt /etc/rivet/tls/engine.key > /etc/rivet/tls/engine.pem && chmod 600 /etc/rivet/tls/engine.pem
```

The same with these names and extensions:

| Certificate | On | `subjectAltName` | `extendedKeyUsage` |
|---|---|---|---|
| `engine.pem` | engine-N | `DNS:engine-N.rivet.internal,IP:10.0.0.1N` | `serverAuth` |
| `lb.pem` | load balancer | `DNS:lb.rivet.internal,IP:10.0.0.2` | `serverAuth` |
| `lb-client.pem` | load balancer | `DNS:lb.rivet.internal` | `clientAuth` |
| `bastion.pem` | bastion | `DNS:bastion.rivet.internal` | `clientAuth` |

`ca.pem` (the certificate only) goes to every engine, the load balancer, the
bastion and every runner (`/etc/norn/rivet-ca.pem`).

**Rotation.**

- Leaves last 90 days and are renewed at 60: a new key and request on the
  machine, signed as above, then `systemctl reload haproxy`. A reload keeps
  open connections on the old process until they close, so runners are not
  cut off. Alert at 21 days left:
  `openssl x509 -checkend 1814400 -noout -in <cert>` on every machine, from the
  monitoring host.
- The CA is replaced a year before it expires, and at once if its key may have
  leaked:
  1. Make the new CA.
  2. Put a bundle of both CA certificates as `ca.pem` everywhere (engines,
     load balancer, bastion, runners), and reload HAProxy and restart the
     runners.
  3. Re-issue every leaf from the new CA and reload.
  4. Remove the old CA from every bundle and reload.
- If any private key or the admin token may have leaked, rotate the admin
  token too (it is the same on every engine and runner).

### Admin and fork routes refuse missing or wrong credentials

Probed on Boat against the compose stack (engine 3.1.1, through `lb`).
Requests were sent with no `authorization` header, with a wrong bearer token,
and with the right one:

| Request | No `authorization` | Wrong bearer | Admin token |
|---|---|---|---|
| `GET /health` | 200 | 200 | 200 (health is public) |
| `GET /namespaces` | 401 `invalid_token` | 401 `invalid_token` | 200 |
| `PUT /pools/default?namespace=default` | 401 `invalid_token` | 401 `invalid_token` | 200 |
| `GET /actors?namespace=default&name=probe` | 401 `invalid_token` | 401 `invalid_token` | 200 |
| `POST /actors/{id}/fork?namespace=default` | 401 `invalid_token` | 401 `invalid_token` | 422 (array key) / 500 (unknown actor id) |

- The same refusals (401) came straight from `engine-1:6420`, bypassing
  `lb`, for the fork and pool routes.
- 6421 and 6430 answered with no credentials at all (404 on `/`, 200 on
  `/metrics`). That is why the firewall above is their only protection.
- The 500 for a fork of an actor id that does not exist comes after the token
  check; it is the engine's answer, not an authorization gap.

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

## Losing a second disk before the first one is rebuilt

One lost disk loses no acknowledged commit. Two lost disks in quick succession
can stop the cluster, and the cause is not known:

- In the #1130 rehearsal, a second disk lost 120 s after the first stopped the
  cluster from serving (`depot request_deadline_exceeded`). Either the first
  replica rebuild had not finished, or the engine hit the 1024-file limit
  (since raised). The logs that would tell them apart were lost.
- The pass with 600 s between the two losses (0 lost of 16,043) is a single
  rehearsal. It does not establish that 600 s is always enough.

So no fixed wait is safe. Before any planned work on another engine (a restart,
an upgrade, a disk or machine replacement), check that the replica rebuild is
complete:

```sh
# From the operator bastion (3 machines). In compose, run the same against
# engine-1..3 over http (no TLS on one machine) from:
# docker run --rm --network norn-rivet_default curlimages/curl:8.17.0 ...
tls='--cacert /etc/rivet/tls/ca.pem --cert /etc/rivet/tls/bastion.pem'
for e in 10.0.0.11 10.0.0.12 10.0.0.13; do
  printf '%s ' "$e"; curl -sf $tls "https://$e:6420/health" >/dev/null && printf 'health=ok ' || printf 'health=FAIL '
  curl -s "http://$e:6430/metrics" | awk '
    /^rivet_depot_tablets\{worker_role="replica"\}/                      {printf "replicas=%s ", $2}
    /^rivet_depot_coordinator_placement\{kind="zone_degraded"\}/         {printf "zone_degraded=%s ", $2}
    /^rivet_depot_coordinator_placement\{kind="pending_replacements"\}/  {printf "pending=%s ", $2}
    /^rivet_depot_coordinator_placement\{kind="(desired|projected)"\}/   {printf "%s ", $0}'
  echo
done
```

One engine at a time is the placement coordinator; only it reports
`rivet_depot_coordinator_placement`. The rebuild is complete only when, on two
reads at least 60 s apart:

- every engine answers `health=ok`;
- on the coordinator, `zone_degraded` and `pending` are 0, and `projected`
  equals `desired`;
- `replicas` (`rivet_depot_tablets{worker_role="replica"}`) is the same on all
  three engines.

Observed on Boat (compose, idle cluster with 24 tablets): after engine-3's
disk was deleted and the engine restarted empty, the coordinator reported
`zone_degraded=24` and engine-3 `replicas=0`. At about 75 s `pending` rose to
24, and at about 96 s every gauge was back (`zone_degraded=0`, `pending=0`,
`replicas=24` on all three), and stayed so for the next 5 minutes. A cluster
holding data takes longer; that is why the check reads the gauges rather than
waiting a fixed time.

If a second disk fails before the first rebuild is complete:

1. Stop replacing anything. Do not wipe, reformat or replace another disk, and
   do not restart the engines one by one.
2. Preserve what survives. Leave the surviving log disks attached and
   untouched. Snapshot them (a Hetzner Volume snapshot, or `dd` to a spare
   disk) before any further step.
3. Keep the logs: `journalctl -u rivet-engine` (or `docker logs`) from every
   engine since the first failure, plus a `/metrics` capture from each engine
   still running. Copy them off the machines.
4. Escalate to the owner with the timeline (first failure, second failure,
   which disks survive) before any further replacement or restart. The
   bucket's replica and the surviving disks are the only copies of the
   commits since the last offload.

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
