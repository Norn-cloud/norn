# Connector credentials: vault, OAuth and egress

D126, first slice. Code: `crates/norn-cell-do/src/connections/`. Tests:
`crates/norn-cell-do/tests/connections.rs`.

**Where it runs.** On the native host: `norn dev` today, and any native
host built on `norn-cell-do`. The Workers Durable Object host doesn't serve these
routes yet. Its outbound `fetch` belongs to the runtime, which this
client can't pin. The hosted wiring builds on the same types.

## What it does

- **Vault.** Each cell has one data key (DEK). Credentials are sealed with
  AES-256-GCM under that DEK. The DEK is stored only wrapped by the host's
  key-encryption key (KEK), together with the KEK's id. The cell's storage holds
  ciphertext, key ids and non-secret status, nothing else. Every ciphertext is
  bound to its cell, connection and revision, so a copied row doesn't open.
- **Write-only API.** Credentials can be set and used, never read back. The
  status shows `last4`, scopes, expiry, last use and last error.
- **OAuth 2.0 authorization code with PKCE (S256),** through the `oauth2`
  crate. The `state` is random and bound to the starting principal, the
  connection and the cell. It lives for ten minutes and is consumed once. The
  PKCE verifier is sealed in the vault.
- **Refresh.** Refreshes of one connection are single-flight: concurrent
  callers make one token-endpoint call. The result is written only if the
  connection's revision hasn't changed since the refresh read it. A rotated
  refresh token is saved in that same write. If the provider refuses the
  refresh (an OAuth error), the connection is marked `reconnect_needed`.
- **Audit.** `_norn_connection_audit` gets one row per event: `credential.set`,
  `credential.use`, `credential.refresh`, `credential.revoke`,
  `credential.failure`, `oauth.start`, `oauth.connected` and `oauth.refused`.
  A row holds who, when and a short code, never a value.
- **Egress.** Every outbound connector call goes through one client
  (`connections::egress`, over the workspace's `ureq`). It does four things:
  1. It requires https to a public host. Plain http, or an internal host, is
     allowed only when the operator lists the host (see below).
  2. It resolves each name once and checks every address against the shared
     rules in `norn_api::net`, *after* resolution: no loopback, private,
     link-local or metadata, CGNAT, ULA, IPv4-mapped or NAT64 addresses. It
     then connects only to the addresses it checked, so DNS rebinding can't
     redirect the connection.
  3. It follows redirects itself (at most 10) and checks every hop.
  4. On a redirect that changes origin, it drops every header except a small
     safe list (deny by default).

  Idempotent requests are retried up to 3 times with exponential backoff, or
  `Retry-After` (at most 30 s). A POST is sent once.

## Routes

All routes are scoped by `orgId` and `environmentId`, like every other route.
Each is decided as `Operation::ManageConnection(id)` by the cell's own policy
before its body is read. Admins and owners hold this permission (the
definition-writer roles). Anyone else gets exactly the answer a scope with no
cell gives.

| Route | Body | Answer |
|---|---|---|
| `GET /v1/connections/{id}/credentials` | – | status |
| `PUT /v1/connections/{id}/credentials` | `{provider, accessToken, refreshToken?, expiresAtMs?, scopes?, params?}` | status |
| `DELETE /v1/connections/{id}/credentials` | – | status (`not_connected`) |
| `POST /v1/connections/{id}/oauth:start` | `{provider, scopes?, params?}` | `{authorizeUrl, expiresAtMs}` |
| `GET /v1/connections/oauth:callback?state=…&code=…` | – | status |

`DELETE` first asks the provider's RFC 7009 revocation endpoint, when the
provider has one, to end the grant. `oauth2` sends that request over https
only. The outcome is audited, and the credential is deleted either way.

The callback must carry the same user's own credential (the platform domain's
session, forwarded as the `Authorization` header). A state started by someone
else is refused and spent.

## Host configuration

| Variable | Meaning |
|---|---|
| `NORN_VAULT_KEK` | The current KEK: 32 bytes, standard base64. Unset means the host serves no connection route. `dev-insecure` selects a fixed, public development key, and only `norn dev` accepts it. |
| `NORN_VAULT_KEK_ID` | The current KEK's id: `<env>-<region>-v<N>`, for example `prod-eu-v2`. |
| `NORN_VAULT_KEKS_PREVIOUS` | Other active KEKs, used to unwrap only: `prod-eu-v1=<base64>,…`. All must share the current id's environment and region. |
| `NORN_OAUTH_REDIRECT_URL` | The callback URL registered with every provider, on the platform domain. |
| `NORN_OAUTH_<PROVIDER>_CLIENT_ID` | The provider's OAuth client id. `<PROVIDER>` is the provider id in upper case, with `.` written as `_`: `GITHUB`, `GOOGLE_GMAIL`, `SHOPIFY`. |
| `NORN_OAUTH_<PROVIDER>_CLIENT_SECRET` | Its client secret. |
| `NORN_SOURCE_ALLOWED_HOSTS` | Hosts allowed although internal, or reached over plain http. This is the same list that source base URLs use (SYNC-URL-1). |
| `NORN_EGRESS_PROXY` | Optional. `http://host:port`, an HTTP CONNECT proxy for every connector call. |

The secrets manager (Infisical) provides these to the host's environment.
Only the connections module reads the KEK variables. A test fails if any
other source file names them.

### Rotating the KEK

1. Add the new key as current, and keep the old one listed:
   ```
   NORN_VAULT_KEK_ID=prod-eu-v2
   NORN_VAULT_KEK=<new>
   NORN_VAULT_KEKS_PREVIOUS=prod-eu-v1=<old>
   ```
2. Restart the host. When a cell's connections open, every DEK not under
   `prod-eu-v2` is rewrapped. Credential ciphertext isn't read or rewritten.
3. Once every cell has opened, remove `prod-eu-v1` from
   `NORN_VAULT_KEKS_PREVIOUS`.

## Egress addresses

Static egress is a deployment concern. Without a proxy, calls leave from the
host's own address. To use fixed egress IPs, set `NORN_EGRESS_PROXY` to an HTTP
CONNECT proxy that runs on those addresses.

The client still checks the target URL, and every address the target resolves
to, before it sends anything. The proxy then resolves the name again, so the
proxy must enforce the same address policy. Ambient `HTTP_PROXY` and
`HTTPS_PROXY` variables are ignored.

## Provider metadata

`registry/connections/providers.json` is generated by
`scripts/connections/extract_openconnector.py`. Each entry's `auth` is a
`ConnectorAuth::Oauth2` value: the connection facet of #1430.

The script reads OpenConnector at tag v1.9.4 only (commit
`d89dc9f24f5a057bb500272d7dbf333837c77c68`, Apache-2.0). Every input is
fetched by its commit-pinned URL and must match a pinned SHA-256. The licence
is checked, and the upstream NOTICE is kept in `registry/connections/NOTICE`.

The first providers are GitHub, Google (Gmail) and Shopify. Default scopes are
the options OpenConnector selects by default (least privilege); the rest are
optional scopes. To check that the file is current, run:

```
python3 scripts/connections/extract_openconnector.py --check
```
