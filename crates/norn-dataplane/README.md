# norn-dataplane

`norn-dataplane` is a ring 1 crate (native driver and host perimeter): raw-observation contract, dlt profile, Iceberg writer glue. Its dependency boundary is in `AGENTS.md` and enforced by `norn-skeleton-guard`. It holds the `norn.raw_observation/v1` contract, its "latest observation wins" convergence view, and the Iceberg table specification (T12-DP-1, #2695). It is IO-free: HTTP clients, catalogs, and clocks live in host code.
