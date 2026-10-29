# Pinned OSI schema fixture

The unmodified OSI 0.1.1 schema is acquired at runtime from Apache Ossie,
commit `faf581054dcf7964d5fe0ceae7d6f415c8ce32a5`. Its source and exact SHA256
are in `scripts/importers/erpnext-runtime.lock.json`. Acquire it outside the
checkout with `python3 scripts/importers/erpnext_runtime.py --output EXTERNAL_SCRATCH --interop-only`
and set `NORN_UPSTREAM_FIXTURE_DIR=EXTERNAL_SCRATCH` before running the
`norn-checker` `metric_interchange` tests. Missing, altered or in-tree input fails.

The upstream identifies copyright 2026 Open Semantic Interchange and separate
Apache-2.0 code / CC BY 4.0 document licences. This records those terms without
asserting which grant covers the schema. No upstream schema or licence-text
copy is shipped. Real upstream validation remains mandatory at runtime.

MetricFlow schema research follows **v0.213.0**, commit
**4200f85c59b2bb334f0b0dea851b38d0b8198134**:
[schemas.py](https://github.com/dbt-labs/metricflow/blob/4200f85c59b2bb334f0b0dea851b38d0b8198134/metricflow_semantic_interfaces/parsing/schemas.py).
Its Apache-2.0 implementation is referenced rather than vendored.
OSI's native metric shape is expression + dialects; MetricFlow type/type_params
belongs in OSI's DBT `custom_extensions` slot, whose data is a JSON string.

The adjacent import samples are Norn-authored, minimal examples using those
published field names, not copies of upstream implementation or documentation.
