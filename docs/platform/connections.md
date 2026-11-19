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
- **Sealed bindings.** The provider and its URL parameters are sealed with the
  tokens, and a pending authorization's verifier is sealed with its actor,
  connection, generation and expiry. Calls use the sealed values. A plaintext
  row that disagrees fails as a vault integrity error, so editing the status
  row can't send a credential to another endpoint.
- **OAuth 2.0 authorization code with PKCE (S256),** through the `oauth2`
  crate. The `state` is random and bound to the starting principal, the
  connection and the cell. It lives for ten minutes and is consumed once. The
  PKCE verifier is sealed in the vault.
- **Generations.** Each connection id has a generation and a revision that
  never reset, even after a disconnect. Setting, connecting and disconnecting
  start a new generation; every credential write takes a new revision. Stale
  work loses:
  - a refresh writes only if both are still the ones it read;
  - an OAuth completion stores tokens only if the generation is still the one
    its start saw;
  - a disconnect deletes the credential and every pending authorization of the
    connection in one transaction; when it was asked to revoke the grant too,
    it calls the provider only after that, so a slow revocation can't touch a
    newer credential.
- **Refresh.** Refreshes of one connection are single-flight: concurrent
  callers make one token-endpoint call. A rotated refresh token is saved in
  the same write. If the provider refuses the refresh (an OAuth error), the
  connection is marked `reconnect_needed`.
- **Errors.** Answers, `last_error` and audit rows carry fixed codes only: an
  RFC 6749 error code (`invalid_grant`, …) or `provider_error`, and the egress
  kinds (`egress_redirect`, `egress_deadline`, …). No provider or peer text is
  kept. The `log` facade is capped at `info` for this crate, so ureq's debug
  and trace records, which include wire rows, are compiled out.
- **Audit.** `_norn_connection_audit` gets one row per event: `credential.set`,
  `credential.use`, `credential.refresh`, `credential.revoke`,
  `credential.provider_revoke`, `credential.failure`, `oauth.start`,
  `oauth.connected` and `oauth.refused`. A row holds who, when and a short
  code, never a value. A row recording a credential change commits in the
  same transaction as the change.
- **Egress.** Every outbound connector call goes through one client
  (`connections::egress`, over the workspace's `ureq`). It does four things:
  1. It requires https to a public host. Plain http, or an internal host, is
     allowed only when the operator lists the host (see below). OAuth token
     exchange, refresh and revocation go only to https, even to a listed
     host; the one exception is a listed loopback address literal
     (`127.0.0.1`, `::1`), which never leaves the host.
  2. It resolves each name once and checks every address against the shared
     rules in `norn_api::net`, *after* resolution: no loopback, private,
     link-local or metadata, CGNAT, ULA, IPv4-mapped or NAT64 addresses. It
     then connects only to the addresses it checked, so DNS rebinding can't
     redirect the connection.
  3. It follows redirects itself (at most 10) and checks every hop. It refuses
     a hop from https to plain http, even to a listed host, and a 307/308 that
     would resend a body to another origin. OAuth token exchange, refresh and
     revocation follow no redirect at all.
  4. On a redirect that changes origin, it drops every header except a small
     safe list (deny by default).

  Idempotent requests are retried up to 3 times with exponential backoff, or
  `Retry-After` (at most 30 s). A POST is sent once. A name resolves within
  5 s, and a whole call, with every redirect, retry and wait, ends within 60 s.
  A lookup runs on its own thread so a hung resolver can't hold a call past
  5 s; at most 16 lookups are outstanding per host, and one more is refused
  at once (`egress_failed`) rather than leaving another thread behind.

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
| `DELETE /v1/connections/{id}/credentials[?revokeGrant=true]` | – | status (`not_connected`) |
| `POST /v1/connections/{id}/oauth:start` | `{provider, scopes?, params?}` | `{authorizeUrl, expiresAtMs, completionKey}` |
| `GET /v1/connections/oauth:callback?state=…&code=…` | – | `connection.oauth_received` |
| `POST /v1/connections/{id}/oauth:complete` | `{state, completionKey}` | status |

`DELETE` disconnects (see Generations) and deletes locally only; it calls no
provider. A provider's revocation can end the whole grant, not just one
token: Google revokes every token the person gave Norn's client, which would
break other connections on the same Google account, and a replacement, while
their rows still say `connected`. So revoking at the provider is an explicit
request: `DELETE …?revokeGrant=true` disconnects, then asks the provider's RFC
7009 revocation endpoint, when the provider has one, to end the grant. Every
other connection on that grant then fails its next refresh and is marked
`reconnect_needed`. The outcome is audited as `credential.provider_revoke`.

The vault is opened only after a request is authorized; a denied caller never
causes a vault read.

### The OAuth callback handoff

The provider redirects the person's browser to the callback, and that
redirect carries no Norn credential. So the callback is a handoff, not a
completion:

1. `oauth:start`, with the user's credential, returns the authorize URL and a
   random `completionKey`; the client keeps the key and opens the URL. The
   start reserves its state in the host's memory, bound to the user, the
   connection and the key's digest, for ten minutes. At most 256
   authorizations are reserved at once, and at most 8 per user (a user's
   oldest is dropped when they start a ninth).
2. The provider redirects the browser to `oauth:callback?state=…&code=…`. The
   callback parks the code only for a reserved, unexpired state (the first
   code wins), and answers `connection.oauth_received`. An unknown or expired
   state is refused with `400 connection.oauth_state` and nothing is stored,
   so callbacks can't fill the store. It opens no vault and reads no row.
   Expired reservations are dropped on every connection request.
3. The client that started calls `oauth:complete` with
   `{state, completionKey}` and the same user's credential. It answers
   `409 connection.oauth_pending` until the redirect has arrived, so the
   client polls. Once the code is there, the state is consumed in one
   transaction that checks the sealed actor, connection, key, expiry and
   generation; only then is the code exchanged.

Another user, another session of the same user (one without the key), or a
call for another connection is refused with `400 connection.oauth_state` and
spends nothing: the flow stays completable by the session that started it.
A state whose connection was disconnected or replaced meanwhile is refused
and spent.

## Host configuration

| Variable | Meaning |
|---|---|
| `NORN_VAULT_KEK` | The current KEK: 32 bytes, standard base64. Unset means the host serves no connection route. `dev-insecure` selects a fixed, public development key, and only `norn dev` accepts it. |
| `NORN_VAULT_KEK_ID` | The current KEK's id: `<env>-<region>-v<N>`, for example `prod-eu-v2`. |
| `NORN_VAULT_KEKS_PREVIOUS` | Other active KEKs, used to unwrap only: `prod-eu-v1=<base64>,…`. All must share the current id's environment and region. |
| `NORN_OAUTH_REDIRECT_URL` | The callback URL registered with every provider, on the platform domain. |
| `NORN_OAUTH_<PROVIDER>_CLIENT_ID` | The provider's OAuth client id. `<PROVIDER>` is the provider id in upper case, with `.` written as `_`: `GITHUB`, `GOOGLE_GMAIL`. |
| `NORN_OAUTH_<PROVIDER>_CLIENT_SECRET` | Its client secret. |
| `NORN_SOURCE_ALLOWED_HOSTS` | Hosts allowed although internal, or reached over plain http. This is the same list that source base URLs use (SYNC-URL-1). |
| `NORN_EGRESS_PROXY` | Refused: the host doesn't start while it is set. See Egress addresses. |

The secrets manager (Infisical) provides these to the host's environment.
Only the connections module reads them, and only the vault module names the
KEK variables (a test fails if any other source file does). `norn dev`
removes every `NORN_VAULT_*` and `NORN_OAUTH_*` variable from its compile
worker's environment, since the worker runs the project's own code.

The development key (`NORN_VAULT_KEK=dev-insecure`) is public. Only `norn dev`
accepts it, and a normal keyring refuses its bytes under any key id.

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

Calls leave from the host's own address. Fixed egress IPs are a deployment
concern: run the host on those addresses.

An egress proxy isn't supported yet. A CONNECT proxy would resolve the name
again, outside the address checks above. Proxy support needs a CONNECT to the
checked address that keeps the TLS name and `Host`. Until then the host
refuses to start while `NORN_EGRESS_PROXY` is set. Ambient `HTTP_PROXY` and
`HTTPS_PROXY` variables are ignored.

## Provider metadata

`registry/connections/providers.json` is generated by
`scripts/connections/extract_openconnector.py`. Each entry's `auth` is a
`ConnectorAuth::Oauth2` value: the connection facet of #1430.

The script reads OpenConnector at tag v1.9.4 only (commit
`d89dc9f24f5a057bb500272d7dbf333837c77c68`, Apache-2.0). Every input is
fetched by its commit-pinned URL and must match a pinned SHA-256. The licence
is checked, and the upstream NOTICE is kept in `registry/connections/NOTICE`.

The first providers are GitHub and Google (Gmail). Shopify's entry is
generated but marked `withheld`: every call naming it is refused with
`connection.provider_unsupported` until a Shopify adapter exists (callback
HMAC and shop checks, its token response, comma-separated scopes). Default
scopes are the options OpenConnector selects by default (least privilege);
the rest are optional scopes.

The egress client's cross-origin header list is copied from OpenConnector's
`src/core/guarded-fetch.ts` at the same commit, under the same NOTICE. To check that the file is current, run:

```
python3 scripts/connections/extract_openconnector.py --check
```
