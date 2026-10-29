# norn-graph

Pure, synchronous, deterministic graph algorithms for #3334 slice 1. This is
an off-commit-path library: no IO, clocks, unsafe code, hashing, runtime or Norn
crate dependencies. Native and `wasm32-unknown-unknown` builds share the same
implementation. Node identifiers are generic over `Ord + Clone`; all graph
storage uses standard-library `BTreeMap`, `BTreeSet` and `Vec`.

```rust
use norn_graph::{Budget, Graph, topological_order};

let graph = Graph {
    nodes: vec!["design", "build", "ship"],
    edges: vec![("design", "build"), ("build", "ship")],
};
let budget = Budget { max_nodes: 3, max_edges: 2, max_steps: 32 };
assert_eq!(topological_order(&graph, budget)?, vec!["design", "build", "ship"]);
# Ok::<(), norn_graph::Error<&str>>(())
```

## Shared contracts

Every algorithm requires an explicit `Budget { max_nodes, max_edges,
max_steps }`. Node/edge counts are checked before allocation or input traversal.
All subsequent passes share one meter: each input node/edge, seed, attribute,
demand, visited node, traversed edge, DFS frame and copied cycle node consumes a
step **before** its work. Map lookups, identifier comparisons/clones and fixed
width arithmetic belong to that work item; steps are not CPU instruction counts.
Exceeding a limit returns `Error::BudgetExceeded(BudgetExceeded { resource,
limit })` with no partial output. No clock deadlines or hidden unlimited default
exist. Repeated calls with identical inputs and budget have identical results,
including refusals. Excessive seed/attribute/demand inputs consume steps too.

Budget counts apply to the entire declared graph, including disconnected nodes
and parallel edges. Duplicate node declarations and undeclared edge endpoints
are errors. Invalid inputs can exhaust a budget before returning a structural
error. The library does not bound the caller's original input allocation or the
size/cost of generic identifier and quantity implementations. Traversals are
iterative, including cycle reconstruction; graph depth does not grow the call
stack. Working and output storage are `O(V + E)` (plus bounded seed processing).

Quantities are generic over `Quantity: Ord + Clone`, with an additive zero and
exact checked addition/subtraction/multiplication. This crate implements it for
the repo's existing `rust_decimal::Decimal`, with default features disabled.
Checked `i128` mantissa arithmetic with explicit decimal scale prevents silent
rounding. Results are normalized and constructed through rust_decimal's checked
constructor. Overflow (including a nonrepresentable intermediate) and results
requiring more than 28 fractional digits return typed `ArithmeticError`s. Some
operations whose final result could fit a Decimal can conservatively refuse if
an aligned/product `i128` intermediate does not fit. The caller selects common
units/currency; the algorithms do not perform conversion. Other exact quantity
types can implement the trait under the same no-rounding, bounded-work contract.

## Algorithms

Complexities below assume bounded-cost identifier comparisons/clones and
quantity arithmetic. `V` is declared nodes, `E` supplied edges, `S` supplied
seeds/demands/attributes. Balanced ordered-map lookups account for `log(V + 1)`.

| Algorithm | Contract | Time |
| --- | --- | --- |
| `topological_order` | Edges are prerequisite → successor. Return every node, including isolated nodes, choosing the smallest ready identifier first. On cyclic input return one concrete directed cycle with its first node repeated at the end. Cycle explanation uses the remaining budget. | `O((V + E) log(V + 1))` |
| `reachability` | Follow outgoing edges (`Downstream`) or incoming edges (`Upstream`) from the union of supplied seeds. Include seeds themselves, deduplicate output, tolerate cycles. Unknown seeds fail. Empty seed set returns empty. | `O((V + E + S) log(V + 1))` |
| `critical_path` | Precedence DAG with exactly one nonnegative duration per declared node. Return global longest-path duration plus earliest/latest start/finish and slack for every node. Start time is zero; all sinks share the global finish deadline, so disconnected shorter components have slack. Empty graph has length zero. No calendars, lags or resource constraints. | `O((V + E + S) log(V + 1))` |
| `explode` | BOM edges are parent → required child, weighted by nonnegative quantity per parent unit. Propagate demands in topological order, summing shared/parallel contributions into base-component requirements. Each part has intrinsic nonnegative per-unit cost and lead time. Unit cost is intrinsic cost plus summed quantity-weighted child costs; unit lead time is intrinsic time plus maximum positive-quantity child path. Total cost scales unit costs by requested quantities; overall lead time is the maximum among positive demands and does not scale with quantity. Return unit roll-ups for all parts. Validate the entire BOM, including cycles outside demand and zero-quantity edges. | `O((V + E + S) log(V + 1))` |
| `hierarchical_rollup` | A single rooted tree, parent → child, with exactly one value per node. Return sum/min/max over each node's own value and all descendants. Signed values are allowed. Reject wrong root, disconnected nodes, multiple parents and parallel edges; explain cycles. Empty hierarchy has no root and is rejected. | `O((V + E + S) log(V + 1))` |

For BOMs a base component has no outgoing composition lines. Zero demands and
zero-quantity lines contribute no requirements, cost or child lead time. A part
with only zero-quantity lines still counts as an assembly, retaining its intrinsic
cost/time but producing no base requirements. Intrinsic costs apply to assemblies
as well as leaves. Demanding both a parent and its child is additive. Empty BOM
and empty demand return empty requirements and zero totals.

## Reuse and validation

Reuse: standard ordered collections, the existing rust_decimal pin, and the
existing proptest pin (test-only). Petgraph from the dependency shortlist was
considered; this task forbids new external packages, and each traversal must
share explicit step metering, including validation and cycle explanation. No
external package/lockfile resolution changes are needed beyond this workspace
member. Definitions, registry wiring, resource leveling and product views are
outside this slice.

Validation runs on Boat, never on the shared agent host:

- `cargo fmt --all -- --check`
- `cargo clippy --locked -p norn-graph -p norn-skeleton-guard --all-targets -- -D warnings`
- `cargo test --locked -p norn-graph -p norn-skeleton-guard`
- `cargo build --locked -p norn-graph --target wasm32-unknown-unknown`

Unit and behavioral tests cover errors, exact fractions, diamonds, disconnected
components, trees, cycles, zero quantities, overflow and deep iterative traversal.
Proptest checks topological edge order, BOM requirements against a brute-force
path fold, critical length against independent longest-path enumeration, timing
invariants and deterministic budget exhaustion across all five algorithms.
