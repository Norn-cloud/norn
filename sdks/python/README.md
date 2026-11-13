# Python client

A Python client for a Norn cell, generated from the frozen OpenAPI document
(`../openapi/norn-api.openapi.json`). It is generated in CI and never checked
in, so it cannot drift from the document.

```sh
sdks/python/run-demo.sh WORKDIR   # generate into WORKDIR/client, type-check, run the demo
```

`run-demo.sh` needs Python 3.11 or later, Bun and a built `norn-dev-host`
(`NORN_DEV_HOST_BIN`). The generated package is `WORKDIR/client/norn_client`;
put `WORKDIR/client` on `PYTHONPATH`. It is not published to a package index
yet.

## Generator

[`openapi-python-client`](https://github.com/openapi-generators/openapi-python-client)
0.30.0, MIT-licensed. Chosen because it is maintained (0.30.0 released
2026-11-07), permissive, deterministic (two runs over the same document are
byte-identical), and emits a typed client whose operations take an
authenticated client carrying the document's bearer scheme. The generator and
every package it installs are pinned by version and hash in
`requirements.txt` (compiled from `requirements.in`); the demo's environment is
pinned the same way in `examples/requirements.txt`.

Generation fails on any generator warning, and the result is type-checked with
mypy (`mypy.ini` names the one exemption, in the generator's fixed template).

## What `prepare.py` changes

The generator reads a derived copy of the document; the frozen document is
never edited. The copy differs only in names that become Python identifiers:

- operation tags such as `crate::object_routes` become module names
  (`actions`, `definitions`, `objects`, `questions`); an unmapped tag fails;
- an object or map schema nested inline in a `oneOf`/`anyOf` variant's
  property becomes a named component, because the generator otherwise names it
  twice and drops it (three schemas today).

Every wire shape is unchanged.
