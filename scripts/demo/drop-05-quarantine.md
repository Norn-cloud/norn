# Drop 5: durable findings, quarantine and recovery

Run `bash scripts/demo/drop-05-quarantine.sh` from clean, fetched main with
Rust 1.98 and Python 3. It builds one native cell test binary, prints its
SHA-256, checks that every named scenario exists, and runs twelve scenarios.
The four generated-input properties each use 16 cases with recorded seed
21320261103; this is sampled runtime evidence, not exhaustive checking.

The existing synthetic Measurement fixture has a directly attached purity
law over the range zero to one. Native invalid writes refuse. Invalid external observations remain in the
durable raw store with a law-pinned finding. Replay and restart preserve one
finding; authoritative reads exclude the quarantined measurement. A later
0.80 correction resolves the finding and restores authoritative visibility.
Recovery checks retained definition versions rather than substituting the
current catalog. Other scenarios refuse missing/substituted laws, unsupported
inherited/effective scopes and unpinned legacy state without partial effects.

This is the normalized observation/cell seam. It does not capture signed
ERPNext/Shopify documents, run the authored jewelry feed pack, execute a
question or prove IVM equality. Those gaps print `not yet` lines. The source
fixture uses a normalized fraction, not the jewelry app's per-mille scale.
No generated application proof, production migration or external anchor is
claimed. Legacy law snapshots must be recreated; migration remains separate.

Publication requires passing these commands against a recorded main source.
An entrypoint rehearsed before its own PR lands must be pinned separately
from the main engine source in the evaluation note.
