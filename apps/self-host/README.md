# Self-hosted Norn

One command runs Norn on your own machine or server: Studio, the data cell behind it, and its
storage. Nothing in this path depends on a hosted service.

## Quick start (this machine)

You need Docker with Compose v2 and about 8 GB of free memory for the first build.

```sh
git clone https://github.com/Norn-cloud/norn.git
cd norn/apps/self-host
docker compose up
```

The first build compiles Norn and takes a while; later starts take seconds. When the log prints
`Norn is ready`, open <http://localhost:8080> and sign in as `owner@norn.localhost`. The password
was generated for you:

```sh
docker compose exec norn cat /data/secrets/owner-password
```

You own this install. Your workspace already has its data cell, so you can connect a source and
start asking questions. After the first account, sign-up is by invitation only, and Studio has no
invitation screen yet, so for now this install has one account.

On the first start the owner's workspace gets its data cell from `norn workspace create`, Norn's
command-line tool, which is in the image. The same command gives a workspace its cell from any
machine that can reach Studio:

```sh
NORN_PASSWORD='…' norn workspace create --studio https://norn.example.com \
  --email you@example.com --password-env NORN_PASSWORD
```

Only the workspace's owner may do this; running it again changes nothing.

To choose the owner yourself, set these before the first start:

```sh
NORN_OWNER_EMAIL=you@example.com NORN_OWNER_PASSWORD='a long passphrase' docker compose up
```

## On a server with a domain

Point the domain's DNS at the server, then:

```sh
export NORN_SITE=norn.example.com NORN_PUBLIC_URL=https://norn.example.com
docker compose -f compose.yaml -f compose.domain.yaml up -d
```

Caddy answers on ports 80 and 443 and obtains and renews the certificate itself.

## What runs

| service | what it is |
| --- | --- |
| `norn` | [celld](https://github.com/denoland/celld) v0.6.3 (pinned by digest), serving Studio and the tenant cell Worker. Each workspace's data lives in its own cell. |
| `s3` | [SeaweedFS](https://github.com/seaweedfs/seaweedfs), the bucket celld keeps everything in. It is reachable only on the compose network. |
| `proxy` | [Caddy](https://caddyserver.com), the only published port. It sets the client address Studio's sign-in rate limit counts. |

Storage is reached only through the S3 API, so any S3-compatible store can replace SeaweedFS: set
`CELLD_BUCKET`, `S3_ENDPOINT`, `AWS_REGION`, `AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY` on
the `norn` service and drop `s3` and `s3-bucket`.

## Data and secrets

- Everything lives in three Docker volumes: `norn-data` (secrets and celld's local working set),
  `s3-data` (the bucket: cells, Studio's database, deployments) and `caddy-data` (certificates).
  Back up `s3-data` and `norn-data` together.
- The secrets (Studio's signing secret, the lineage key, the generated owner password) are created
  once, on the first start, in `/data/secrets`, readable only by the container's user.
- `docker compose down` stops Norn and keeps the data; `docker compose down -v` deletes it.

## Upgrading

```sh
git pull && docker compose up --build -d
```

Each start redeploys the cell Worker and Studio from the image and applies Studio's pending
database migrations; your data stays.

## Settings

| variable | default | meaning |
| --- | --- | --- |
| `NORN_PUBLIC_URL` | `http://localhost:8080` | The address people open. Plain `http` works only for `localhost`. |
| `NORN_PORT` | `8080` | The published port on this machine. |
| `NORN_SITE` | `:8080` | Caddy's site: a domain name to serve it with a certificate. |
| `NORN_OWNER_EMAIL` | `owner@norn.localhost` | The first account, the install's owner. |
| `NORN_OWNER_PASSWORD` | generated | Its password, at least 12 characters. |
| `NORN_COMMIT` | from `.git` | The commit the image is built from, when the build has no `.git`. |

## Limits

- One node. celld can run as a fleet, but this setup runs one node that writes through to the
  bucket; scale up the machine before adding nodes.
- One account for now: Studio has no invitation screen yet, and sign-up after the owner is by
  invitation. No email either: magic-link sign-in needs a mail sender, which this setup does not
  configure.
- The image builds for `x86_64` Linux (celld's published binary).
