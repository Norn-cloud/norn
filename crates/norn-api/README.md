# Core wire contracts

Projects the Rust IR and checker schemas into OpenAPI components for the TypeScript SDK.
Run `norn openapi` to regenerate the contract. Cell execution uses `norn-cell` and
`norn-kernel` directly; this crate supplies no server.

The host clock is the first native-host prerequisite: `Clock` supplies kernel
logical time; `ManualClock` supports deterministic replay and shared `Arc`
handles. The opt-in `native` feature exposes `SystemClock`, the wall-clock
capability. No routes, server, connector host or analytics executor ship yet.
The API crate is a ring-1 host perimeter; IR validation remains in ring 0.
