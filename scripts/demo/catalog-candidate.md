# Catalog read candidate: ontology knows itself

Pin #467 at `03d02276e75f6c310328b397845370d450d43dd7`, on #455/#457.
This is a prepared candidate; assign its drop number after it lands.

Run `bash scripts/demo/catalog-candidate.sh` on clean, fetched main after the
seam and this wrapper merge. Install the frozen `tools/norn-ts` dependencies
with Bun's `--backend copyfile`; Rust is also required.

Compiles the existing Norn jewelry fragment, obtains a real checker receipt,
and installs those definitions in an ephemeral local cell. It calls
`CatalogSnapshot::read_active` then queries all eleven `norn.catalog.*` virtual
relations through `SliceQuery`/`SliceResult`. Output preserves the returned
MemberRef keys and edge endpoints, including empty relations and completeness.

The local host supplies ordinary `norn.read`, kernel PDP and a fixed physical
org/environment scope. Virtual descriptors enter Cedar projections only;
schema fragments compose through the kernel facade. The wrapper also checks
missing-capability denial, missing-scope refusal and a refused catalog write,
with no cell-head change. It creates no production principal or deployment.

This is a bounded read of a small admitted definition set. It is not analytical
QuestionIR, UI browsing, subscriptions or a classification demonstration.
Lane G owns analytical evaluation. No membership effects are introduced.

The stacked seam branch predates #359/#366/#399. The main-only script also
requires those existing demo files; candidate qualification feeds Norn output
from a separately pinned main fixture. No candidate run is a delivered drop.
