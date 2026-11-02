# Definition catalog contract

Canonical IR is authoritative. The catalog is a read-only projection, never an
admitted artifact kind or an authored ontology.

- **Identity:** a member binds an artifact id, exact version and typed member.
  Its stable id preserves member kind, type, lifecycle and local name.
- **Declaration:** properties, links and transitions bind their declaring
  artifact, including extensions and inherited members.
- **Admission correspondence:** writes bind the same definitions as the
  checker, including pinned effects and inherited owned collections.
- **Observation:** mappings observe declared members; they never author writes.
- **Law binding:** law subjects use admission; action responses use required
  dependency versions and inherited transitions bind their declaring type.
- **Closure:** candidates and installed artifacts contribute rows and exact
  dependency edges. Unknown or refused members do not gain fabricated rows.
- **Determinism:** rows are sorted and unique, independent of input ordering.

Projection requires an admitted artifact set. Undecodable payloads contribute
no member rows; checker findings remain authoritative. Inherited lifecycle
states and rule writes other than decide-graph fact changes are outside scope.

Correspondence tests are added alongside each builder capability. The first slice
defines identity types; builder slices add executable correspondence checks.
The entity-only projection slice defers dependency edges to the action/binding slice. GRAPH-META-1 preserves enum lifecycle states, initial and terminal sets through admission's local type-ref resolver.
