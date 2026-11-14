# norn

`norn check --artifacts artifacts.json --context context.json` runs the same
checker ABI used by TypeScript authoring and returns its admission report.
`norn openapi` emits the generated core wire contract. Embed `norn-cell` for
transactional cell execution; the public CLI does not embed a product server.

`norn eval --defs DIR --org ID --env ID --rule ARTIFACT_ID` admits the
definition set, resolves the rule's sub-decision closure from its pinned `rule`
dependencies (`norn_checker::rule_closure`, the closure admission checks), and
evaluates JSON-lines `{slice,event}` inputs through `norn-decisions`, the cell's
evaluator. Each response carries `closureDigest`, the Norn1 identity of exactly
the rule set evaluated, and its candidate effects as data under a bounded
operation budget. An unresolved, skewed, cyclic or over-bound closure is refused.

CLI-EVAL-BATCH-1: every input line gets one JSON response on stdout, including
refused records; evaluation continues after a refusal. Stderr reports
`norn eval: N accepted, M refused`. Exit status is zero only when no records
were refused (including an empty batch), and nonzero on any refusal or IO/setup
failure. A blank line is malformed input, rather than an empty batch.
The `eval-batch.test.ts` integration cases exercise the native binary in CI.

`norn remote` discovers and calls an external Norn MCP endpoint using its live
catalog, without a local action whitelist:

```sh
norn remote --url https://norn.example/mcp --org example --env dev --token-env NORN_TOKEN list
printf '%s' '{"id":"example"}' | norn remote --url https://norn.example/mcp --org example --env dev --token-env NORN_TOKEN call example.submit --idempotency-key example-submit-1
```

Calls read a JSON object from stdin, preserve exact numeric tokens and forward
the explicit operation identity. Server tool refusals, protocol errors and HTTP
failures exit nonzero and retain a valid JSON-RPC response on stdout. Bearer
tokens come from the named environment variable and require HTTPS. Redirects
are disabled. `--dev-actor` supports an external server explicitly configured
for local actor headers; production identity and authorization remain on the
server. This command does not supply a local server or install definitions.

The client reuses clap, ureq/rustls and the maintained sse-stream decoder;
MCP event data stays text so business decimals never round through floats.

`norn port parity|scoreboard|code-ontology|selftest [ARGS...]` forwards to the
porting tools and exits with the tool's own status. `code-ontology` reads
source as data; `parity` EXECUTES the original function it is given, in a
subprocess that is not a sandbox, so use it only on code you would run
yourself. The CLI links none of that code. It finds the tools from an absolute
`NORN_PORT_HOME`, an installed `<prefix>/share/norn/port` beside
`<prefix>/bin/norn`, or the checkout it was built from, never from the working
directory; refuses unless the tools and interpreter are owned by you or root
and nobody else can write them; and runs them under `python3` from an absolute `PATH` entry (or an absolute
`NORN_PORT_PYTHON`) in isolated mode with a short fixed environment. Today the
tools ship only in a source checkout. Details and limits:
`tools/norn-port/README.md`.
