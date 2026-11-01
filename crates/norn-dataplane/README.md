# norn-dataplane

Ring-one contracts for receiving observations, with no engine dependency.
Observations are evidence and grant no action or mutation authority.

`raw_observation` admits exact JSON or refuses it (DP-EXACT-1–2), and
applies versioned redaction only after validating the entire policy
(DP-REDACT-1–3). Its module documentation specifies these invariants;
`boundary_invariants_conform` supplies sampled correspondence evidence.

The only Norn dependency is `norn-ir`, including its canonical hashing
re-export. `boundary.json` and the skeleton policy declare this boundary;
`machine_boundaries_match_dependency_policy` checks their correspondence.
