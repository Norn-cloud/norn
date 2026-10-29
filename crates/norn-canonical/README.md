# norn-canonical

`norn-canonical` is the small, dependency-light registry for Norn identity
profiles, integrated through `norn-ir`'s canonical dependency and re-exports.
Callers choose a profile explicitly and store
that profile next to every canonical identity.

## Profile registry

The registry currently contains exactly these profiles:

| Profile | Version | Canonical bytes | Hash preimage |
| --- | ---: | --- | --- |
| `Legacy0` | `0` | Historical compact Serde JSON after artifact null pruning and recursive key ordering | `canonical_bytes` |
| `Norn1` | `1` | The canonical JSON rules below | `NORN1_DOMAIN_TAG || canonical_bytes` |

`HashProfile::Legacy0` exists only for compatibility. The historical artifact
path serialized an artifact to
`serde_json::Value`, removed nulls only from `scope`, `provenance`, and
dependency objects, recursively sorted object keys, serialized compactly with
`serde_json::to_vec`, and applied an unprefixed SHA-256. The public
`legacy_canonicalize_value` function accepts an already parsed Serde value when
a caller needs that exact representation. The structured quantity
commitment helper is also available as `LegacyStableHasher`; it implements the
`SHA-256(label) || SHA-256(value)` part encoding from
`crates/norn-checker/src/quantity_ledger/boundary.rs:724-752` (moved from
`norn-ir` in T1-CHK-1).

The other current legacy hashes (moved from `norn-ir` to `norn-checker` in
T1-CHK-1) are raw, unprefixed SHA-256 over the exact
`serde_json::to_vec` output: accounting commands at
`crates/norn-checker/src/accounting.rs:1297-1301`, inventory transitions at
`crates/norn-checker/src/inventory.rs:1002-1006`, and inventory receipts at
`crates/norn-checker/src/inventory.rs:1008-1024`. Pass those already serialized bytes
to `HashProfile::Legacy0.hash_canonical_bytes`; that method deliberately does
not sort, prune, or otherwise alter its input.

The five golden vectors in `tests/legacy_fixtures.rs` preserve the historical
Legacy0 hashes independently of the removed v0 fixture tree. The product,
order, and process-shaped inputs are opaque hash test data only; they do not
represent retained schemas or runtime support. Their expected Legacy0 hashes
are:

```text
026f956e837ad090c704c1aca9211a2c4d859bdb14d1874c49f21d5ce648fca5
3a811374bb9c5b9e19d45001f42ea0fd275c7c265e8836707e466414645234b1
80da3e6475f7c3b58554f70815916a0d35ac734156ef949a5cba860b28934d7b
230bcd0973cf0d8f036675fd0169a6bfabcf72a52b16ae2b652c1c6635e8b1cc
bf52fd3d75fcb37eeb4c30e851e110e5b3b09ce04efc546b9a6b0719fd4e197e
```

These are compatibility records, not migration instructions. Existing hashes
are never recomputed under `Norn1` or any future profile. A stored identity
keeps the profile that produced it; a new profile produces a new identity only
when a governed artifact/version change explicitly opts into it.

## Norn1 canonical JSON

`parse` and `serialize` use a dedicated value model so duplicate object members
cannot disappear through a map conversion and decimal values do not pass
through an IEEE-754 float. The rules are:

- Object members are unique. Duplicate keys are rejected, including two keys
  that become equal after NFC normalization. Keys sort lexicographically by
  their normalized UTF-8 bytes. Array order is retained.
- Strings and object keys are Unicode NFC-normalized. UTF-8 is emitted directly
  except for JSON quoting, backslash, and all C0 control characters. The five
  conventional controls use `\\b`, `\\f`, `\\n`, `\\r`, and `\\t`; other C0
  controls use lower-case `\\u00xx` escapes. Unpaired UTF-16 surrogate escapes
  are rejected.
- Numbers are exact base-10 decimals. The serializer accepts ordinary JSON
  integer, fraction, and exponent spellings, removes insignificant leading and
  trailing zeroes, applies the exponent exactly, and emits one plain decimal
  spelling. Thus `1`, `1.0`, and `1e0` share bytes, while `-0`, `-0.0`, and
  `0e99` emit `0`. Non-finite values are rejected. Exponents outside the safe
  signed range or whose plain expansion cannot be represented return an error;
  one normalized number is capped at 1 MiB to prevent unbounded expansion.
- JSON nesting starts at depth zero for the root and permits at most
  `MAX_JSON_NESTING_DEPTH` (256) levels. Parsing and programmatic serialization
  return the typed `NestingTooDeep` error before descending further. The limit
  is intentionally broader than the IR schema guards while still bounding
  recursive stack use for generic JSON.

Plain decimal output is an intentional policy choice. It avoids precision loss
and cross-language float differences, makes numeric equality visible in the
wire bytes, and is straightforward for Rust, TypeScript, and Python contract
implementations to reproduce. The tradeoff is that a very large exponent can
produce a large byte string; callers should enforce their normal ingress size
limits.

Norn1 hashes with SHA-256 over the exact byte concatenation
`b"norn.canonical.json/norn1\\0" || canonical_bytes`. The domain tag is public
as `NORN1_DOMAIN_TAG` so other implementations do not invent a spelling.

## API sketch

```rust
use norn_canonical::{HashProfile, canonicalize_json};

let bytes = canonicalize_json(br#"{ "b": 1.0, "a": -0 }"#)?;
assert_eq!(bytes, br#"{"a":0,"b":1}"#);
let identity = HashProfile::Norn1.hash_canonical_bytes(&bytes);
```

Use `HashProfile::hash_json` when starting with source JSON. Use
`HashProfile::hash_canonical_bytes` only after canonicalization; it intentionally
does not parse a second time.
