# norn-builtins

Ring 0 (pure, sans-IO, wasm32): Norn-owned host built-in Functions and their compiled-in registry (FUNCTION-HOST-1, Core 07; #761). Each built-in's manifest pins this crate by name, version and HOST-TREE-1 source-tree digest. The pin lives in `registry/builtins/norn-builtins.json`, and `build.rs` fails the build whenever the sources drift from it (`tests/pin.rs` checks the same walk). After a reviewed change, re-pin with `norn source-digest crates/norn-builtins`.

- `norn.decision@1` (`decision`): ask a person. Only the cell resolves it, by issuing the receipt for a verified decider. `IssuedOnly` makes reconciliation refuse every other receipt for this manifest.
