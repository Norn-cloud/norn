# norn-cell-do

Ring 1. One tenant cell per Workers Durable Object, over `norn-api`'s shared
authorized read services and `norn-cell`'s portable store on
`norn_cell::workers::WorkersEngine`. The Durable Object binding and Worker follow in
`apps/norn-cell-do`.

- `principal`: `PrincipalResolver` is the one seam from transport credential
  to Norn principal (fixture principals until #817), plus the fixture policy.
- `seed`: provisions the public three-systems demo cell idempotently.
- `server`: read-only fetch dispatch; native tests run the identical code.
