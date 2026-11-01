# Field authority admission

- **Caller protection:** non-rule callers cannot supply protected fields, including
  owned-row Add input patches. Refusal leaves facts and ledger unchanged.
- **Default provenance:** owned-row Add materializes declared defaults only after
  validating the caller patch. Kernel-materialized defaults may initialize protected
  fields; they do not authorize subsequent caller writes.
- **No blanket create exemption:** ordinary non-rule creates still reject protected
  fields. The owned-row admission origin is internal to the validated row path.

Conformance: `owned_row_defaults_are_kernel_values_but_protected_inputs_are_refused`
uses the same child schema with and without a caller-supplied protected value.
The former refuses atomically; the latter stores the declared default.
