# norn port

Files-only tooling for porting source-system code to Norn definitions: a
code-unit inventory, differential parity between an original function and its
Norn candidate, a scoreboard, and a deterministic code ontology (which code
unit reads, writes or triggers on which entity). It reads source as data and
never imports or executes the code it maps, apart from the reviewed original
function the parity runner calls. The parity report format is a **draft**, not
a ratified parity-artifact contract or an installation gate. App-specific
definitions and reports live in the application's repository, never here.

`norn port <subcommand> ...` forwards to this directory's tools; `norn-cli`
gains no dependency on them (see "Command" below).

`inventory.schema.json` types the source path and symbol, trigger, declared
reads/writes/external calls, inclusive source LOC, mapping target, parity
recipe, status, and artifact IDs. `access` is the cross-cutting authorization
target; `setup-only` is a classification outside the six runtime targets.
The classifier is a reviewed inventory, not a claimed automatic Python
business-code translator. A fallback reason is mandatory. `translated` must
only be set after review of its parity report and checker admission.

## Execution

Build and check on Boat or another disposable sandbox, using committed source:

```sh
cargo build --locked -p norn-cli
python3 -m venv /tmp/norn-port-venv
/tmp/norn-port-venv/bin/pip install -r tools/norn-port/requirements.txt
/tmp/norn-port-venv/bin/python tools/norn-port/selftest.py --norn /path/to/target/debug/norn
/tmp/norn-port-venv/bin/python tools/norn-port/port.py parity \
  --manifest /path/to/app/norn/<pack>/parity-manifest.json \
  --source-root /path/to/app --defs /path/to/app/norn/<pack>/defs \
  --norn /path/to/target/debug/norn --reports /path/to/app/norn/<pack>/parity
/tmp/norn-port-venv/bin/python tools/norn-port/port.py scoreboard \
  --manifest /path/to/app/norn/<pack>/parity-manifest.json \
  --source-root /path/to/app \
  --inventory /path/to/app/norn/<pack>/inventory.json \
  --defs /path/to/app/norn/<pack>/defs --authoring /path/to/app/norn/<pack>/authoring --reports /path/to/app/norn/<pack>/parity \
  --output /path/to/app/norn/<pack>/scoreboard.json
```

No dev server, cell,
credentials, live-site calls, or effect dispatch is involved.

`norn eval --defs DIR --org ORG --env ENV --rule ARTIFACT_ID` admits the entire
set with the existing checker, prepares one untriggered RuleIR v2 `decide` with its sub-decision closure,
and reads `{slice, event}` JSON lines on stdin. Each output is
`{ok:true, closureDigest, output:{facts,commands,timers}}` or
`{ok:false, closureDigest, error:REASON}`.
Admission failures and broken streams exit nonzero. Evaluation refusals are
per-input data. Exact decimal literals cross the existing RuntimeValue
boundary. Budgets are per input (`--units`, default 1,000,000). A closure
that does not resolve is refused before any input is read.

For parity, a rule emits one **candidate data record** in `commands`:
`{value, refusal: null | {type, reason}, input: POST_CALL_ARGUMENTS}`. It emits
no facts or timers and has no trigger, so it is not a hosted business effect.
The harness compares the result, typed refusal/reason, and input mutations.
Every value equality uses the shared type-preserving comparator, including
mutation detection and metadata comparisons. The harness self-test lints Python
AST equality operators, with exemptions only for the comparator itself and the
interpreter module-entrypoint check. A Norn evaluator/schema failure is an infrastructure failure; it cannot be
miscounted as agreement with a source refusal. Domain refusals must be modeled
in the candidate record. These draft comparison adapters are explicit in each
rule and should become sub-decisions when the public result seam is settled.

The original runner uses `python -I`, a fresh namespace for every case, and
the unchanged AST of the function and its module-global dependency closure.
It preserves the source module's future imports and eager annotation dependencies, without inheriting the
harness module's compiler flags. It skips unused module imports/setup. Only `frappe`, `frappe.throw`, the default
`ValidationError`, and the translation identity `_` are stubbed. A required
non-stdlib import is refused, never faked. Document adapters provide attribute
access; explicitly declared set/tuple arguments are wire adapters, not source
rewrites. Every source-file byte is hashed, including excluded setup, so any
source revision invalidates the report. Imported dependency files outside the
selected module are not supported in this first slice. This is trusted-source
process isolation, **not an OS sandbox for untrusted Python**. Original source
must be reviewed for IO before using the runner. Class/controller methods,
Frappe utilities, database reads, and framework effects need a later platform
harness.

Hypothesis plus hypothesis-jsonschema generates at least 1,000 cases per unit
from its declared invocation schema, with a fixed seed, no database, and an
integer nonce to prevent finite schemas terminating early. Test-derived seeds
run first; their source evidence is recorded. A unit with no direct tests must
say so, rather than inventing test coverage. Hypothesis/package versions are
pinned; the interpreter version must also be retained with run evidence.
Generator domains are explicit in each report; agreement within them is not
proof over every untyped Python input. NFC wire canonicalization preserves
list order, distinguishes booleans from numbers in the report format, and
normalizes text as Norn does. Decimal/float outputs are restricted to the
source function's declared JSON domain; IEEE arithmetic is not reimplemented.

Reports include source, exact artifact-file and exact parity-manifest SHA256, generator seed, case
counts, agreement rate, the full input domain, and **every** failed case with
both outcomes and a Hypothesis-minimized example. Shrinking is deterministic
and shared for identical outcome pairs. Shrinking retains the known failing
case in its strategy and reuses one admitted evaluator/source process per
unit; the source still creates a fresh namespace for each invocation. This
avoids re-admitting a large ontology for every shrink attempt. JSON-lines
frames split only on LF, preserving Unicode separators inside strings. An empty divergence list is a pass.
An accepted divergence requires an explicit reason and review; the harness
never accepts a divergence automatically. A failed unit exits nonzero.

The scoreboard counts source-file LOC separately from translated function
LOC and counts units by status. Norn definition LOC is TS authoring-source LOC
(including shared support). IR JSON lines are reported separately as compile
and import output, with rule and ontology subtotals; formatting is unchanged. A missing, duplicate, stale or mismatched report is unmeasured: every current inventory artifact ID and exact file hash, plus the current original source-file and parity-manifest hashes, must match.
`scoreboard --manifest` is required; a changed domain, seeds, generator config,
invocation adapter or any other manifest byte invalidates prior evidence. Reports
without manifest binding are unmeasured and must be regenerated. Elapsed time and model-token
usage remain null when unknown; null is not zero.

## Reuse

Reuse decision: existing norn-api admission, norn-decisions evaluation,
and RuntimeValue canonicalization supply the Rust behavior. Python's AST/subprocess/hashlib/json/argparse supply the process
adapter. Hypothesis, hypothesis-jsonschema and jsonschema supply generation,
shrinking and schema validation. The custom code connects these boundaries;
there is no new runtime, IR contract, Rust dependency, or source-code engine.

## Authoring

Author candidates as `Norn.decision` modules using `tools/norn-ts`, as in its
sales-order fixture pack. Compile syntax with `compilePack`; never import or
execute the authored modules. The shared RuleIR template declares
`scopeRequired: ["orgId", "environmentId"]`. `Schema.NullOr` decision members
lower to RuleIR's existing nullable closed schema; arbitrary unions remain
rejected and other closed-schema profiles keep their existing boundary.

## Code ontology

`python3 tools/norn-port/code_ontology.py APP_DIRECTORY... --codegraph /path/to/codegraph`
emits deterministic `norn.code-ontology/1` JSON to stdout. Install the pinned
`code-ontology-requirements.txt` into an isolated Python environment first;
tree-sitter JavaScript supplies syntax nodes for the Frappe client rules. Pass `--revision SHA`
for a pinned input snapshot; the default explicitly says `working-tree`. The
source directories are read-only inputs, never imported or executed. SQLite and
copied sources live temporarily under `--scratch` (default
`norn-code-ontology` under the system temporary directory) and are removed even on failure.

The host `CodeGraph` Protocol is the replacement boundary. Its default adapter
uses the MIT-licensed [sunerpy/codegraph-rust v0.54.0](https://github.com/sunerpy/codegraph-rust/tree/v0.54.0)
binary's full index and node-link export, with centrality disabled. Use the
release archive `codegraph-0.54.0-x86_64-unknown-linux-musl.tar.gz`, SHA256
`dcb3da1d007bf0d9c9e7f0d54ad71281514d930ddfd6a7841eccdd8bee9f8c69`;
verify it against that release's `SHA256SUMS` before extracting. `python3 tools/norn-port/codegraph.py --fetch DIR`
downloads that archive, refuses any other digest, unpacks only the binary into `DIR` and prints its path. The adapter
rejects other versions and does not install an MCP server, watcher or daemon.
No new Cargo dependency or SQLite owner is introduced.

The tool reuses `behaviour.py`'s hook ledger and the shared `python_ast.py`
function inventory, so the graph never becomes a second authority for
source-language clauses.
`norn_code_ontology::CodeOntology` (`crates/norn-code-ontology`) is the pure, wasm-safe typed
contract; `facts_for` joins graph facts to inventoried `CodeUnit`s only when path,
symbol, original-byte digest and revision agree. Resolved source facts still
require ontology mapping and original-source parity before porting. Connect can
use the same facts to inspect side effects without claiming parity.

Coverage includes every production Python function/method and literal client
handler in the supplied apps, not only registered entrypoints. Its resolution
denominator is **reads + writes + triggers_on**, with unresolved edges included.
Calls, schedules, guards and APIs do not inflate that percentage. Missing stock
DocTypes require additional metadata inputs; computed SQL fields, dynamic hook
composition, uncertain receivers and external calls remain typed gaps. The
entity matrix contains resolved touching-handler identities; it is not a claim
that every handler was translated or tested. Python control-flow joins are
conservative; runtime-generated registrations, arbitrary receiver inference,
JavaScript computed handlers and nested closures are not resolved. Client
Script evidence uses the JSON document pointer plus `script_line`; the
file line is 1 for that metadata document.

Run goldens against the actual pinned binary:
`CODEGRAPH_BIN=/path/to/codegraph python3 -m unittest discover -s tools/norn-port -p 'code_ontology_test.py'`.
The fixture includes all seven relations, hooks, field operations, literal form
and Client Script targets, and unresolved/dynamic examples. No Frappe service
or source module execution is needed.

## Command

`norn port parity|scoreboard|code-ontology|selftest [ARGS...]` runs `port.py`,
`code_ontology.py` or `selftest.py` from this directory with the remaining
arguments unchanged, and exits with the tool's status. The directory is
`NORN_PORT_HOME` when set, otherwise `tools/norn-port` under the working
directory; the interpreter is `NORN_PORT_PYTHON` when set, otherwise `python3`.
The CLI adds no behaviour of its own and links none of this code. `selftest`
passes `--norn` with the running binary's own path.

## Limits of this directory

- The only source rules are for Frappe apps (Python hooks and controllers,
  literal client handlers). Another framework needs its own rules behind the
  same fact contract; nothing here resolves Java, Groovy or XML service
  definitions.
- The pinned graph binary indexes Java but not Groovy, and gives XML a file
  node only.
- The behaviour-proposal extractor, the Frappe and Shopify schema importers and
  their captured fixtures are not in this repository. `behaviour.py` holds only
  the hook ledger and evidence helpers the code ontology needs.
- Parity admits a unit's definition directory as one self-contained set, through
  `norn check` with an empty context. Composing registry packages into that
  set is not available; a unit that names a `registry` is refused.
- Fixtures here are written for these tests. They are not copies of any
  upstream application.
