# norn-cell-do

Ring 1. One tenant cell per Workers Durable Object, over `norn-api`'s shared
authorized read services and `norn-cell`'s portable store on
`norn_cell::workers::WorkersEngine`. The Durable Object binding is `wasm`
(wasm32 only); the Worker shim and smoke test are in `apps/norn-cell-do`.

- `principal`: `PrincipalResolver` is the one seam from transport credential
  to Norn principal (fixture principals until #817), plus the fixture policy.
- `seed`: provisions the public three-systems demo cell idempotently.
- `base`: the trusted base this host compiles in (`registry/core/artifacts`'
  reserved-namespace artifacts).
- `tenant`: one scope's cell, vacant until `POST /v1/cell:provision`
  (HOST-PROVISION-1) creates it from a tenant's submission, then served and
  reopened on restart; see `apps/norn-cell-do/README.md`.
- `server`: fetch dispatch (reads, ad-hoc questions, the retained definition
  routes for dashboards and agents, written only by admins and owners, and the
  governed action route `POST /v1/actions/{actionId}`); native tests run the
  identical code.
