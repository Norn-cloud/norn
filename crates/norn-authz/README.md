# norn-authz

`norn-authz` is a ring 0 crate (pure, portable semantic core): AccessIR→Cedar compile/PDP. Its dependency boundary is in `boundary.json` and enforced by `norn-skeleton-guard`.

## What is here

- **Schema (T5-AZ-1, #2576).** `AuthzSchema` composes the Norn base Cedar fragment (`NORN_BASE_SCHEMA`: `Norn::Scope`, `Norn::Role`; the projection declares the principal type `Norn::Actor`) with the fragment `norn-projections` projects from EntityIR v2 + ActionIR (T1-PROJ-3, #2530; JSON or Cedar schema syntax). The projection contract is documented in `src/base.rs`.
- **Compile + T2 admission (T5-AZ-2, #2577).** `compile(&schema, &AccessIr)` lowers each `norn_ir::access::AccessRule` (`allOf`, mandatory structural `scope` conjunct, directional `rel`, roles, typed context comparisons) to one Cedar `permit`, then parses and strictly validates the set. Any finding rejects the document with a stable `AuthzCode` (`authz.schema.*`, `authz.policy.*`, `authz.access.*`). There is no entry point for hand-written Cedar.
- **PDP and the AND rule (T5-AZ-3, #2578).** `Pdp::new(validated_policies, &grants)` then `decide(&AccessRequest)` → `Allow` or `Deny(reason)`. The rule is capability (scoped `CapabilityGrant`s compiled to Cedar over the Norn-owned `CAPABILITY_SCHEMA`) ∧ the Biscuit slot (`Delegation::Direct` only; any token denies until Biscuit lands) ∧ the admitted AccessIR policies. Malformed input, schema mismatch, or any evaluation error denies. The kernel calls it through `norn_kernel::authz::authorize`.
- **Goldens** in `tests/goldens/`; semantic checks with Cedar's `Authorizer` in `src/tests.rs`.

## Guarantees

- **Default-deny.** Cedar denies unless a `permit` applies; the compiler emits no `forbid`, and an empty rule list denies everything.
- **Scope is a mandatory conjunct.** Every compiled policy starts with `principal.scope == context.scope && resource.scope == context.scope`, plus `context.scope has <dim>` for every listed optional dimension. Scope records compare exactly: an absent dimension matches only an absent dimension, never as a wildcard. `org` must be listed.
- **No expressions.** AccessIR has no expression node; ZEN Expression may only compute typed context values outside Cedar. IR strings are never spliced as Cedar syntax: entity references use `cedar-policy`'s printer and attribute names/strings are escaped literals.
- **Trusted principal attributes (#3110).** Resource comparisons accept a closed `{ "principalAttribute": "business_units" }` operand: `in` tests scalar membership in the principal's set, while `eq`/`ne` compare scalars. Types are inferred from projected resource metadata; decimals, links, conflicting declarations, and reserved principal fields are refused. Resolver values are versioned by authorization epoch and never taken from action input. The projection and host types are documented in `norn-projections` and `norn-kernel`.

## Where the PDP runs

In the kernel, on every call. The kernel is `wasm32-unknown-unknown` and synchronous, so this crate and `cedar-policy` (default features off) must build for that target; the core CI script builds `norn-authz` on every change. The PDP takes only Norn-typed inputs (`AccessRequest`, `CapabilityGrant`), so no Cedar type crosses into the kernel.
