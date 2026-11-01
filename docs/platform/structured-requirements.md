# Structured requirements: closed demo fragment

`StructuredRequirement` is typed authoring over `DeonticRule` and its existing
`LawIR`. It is not a new artifact kind or a second hierarchy. Scope (root,
types, lifecycle states, commitments), subject, Boolean activation condition and
response remain in LawIR. The normative grade, source span, exact authority
version, exceptions and preferred identities remain in DeonticRule.

Invariants:

- **SR1 Identity:** serialization preserves every law, source, scope and numeric
  expression byte. English/Arabic templates cannot alter semantic fields.
  Structural identities must be nonblank and all source spans well formed.
- **SR2 Timing:** only every-committed-state and same-commit safety are supported.
  Both interpret the condition pointwise in the candidate state; same-commit
  never means an event edge. Eventually, until and any deadline refuse with
  `requirement.timing.unsupported`. Unknown timing tokens fail closed at decode.
- **SR3 Grade:** obligations and permissions use always; prohibitions use never.
  A prohibition's response describes the forbidden predicate, not its negation.
  Permission never asserts occurrence or grants runtime authorization. Existing
  executable lowering still refuses permission and defeasible declarations.
- **SR4 Typed predicates:** activation, predicate response and exceptions must
  typecheck as Boolean under the existing bounded ZEN profile. Arithmetic stays
  exact, including decimal spelling; readable templates do not execute. Action
  and transition occurrence responses refuse as `requirement.response.unsupported`.
- **SR5 Authority:** no priority is inferred. Grounding for `norm_precedence`
  retains the original identity/grade/authority and exception domains. Its
  superior/posterior/specialis rules remain the only precedence algorithm;
  normative consistency never overrides protected physics.

`check_requirement` checks the authoring fragment, not ontology bindings,
writer preservation, signatures, runtime enforcement or solver evidence.
Consumers must retain the complete wrapper including its commit timing, then
perform ordinary LawIR admission. No FRET compatibility or timed-liveness
claim is made. Terminal commitment persistence is a state invariant; deadline
breach needs a separate future clock-aware requirement.

Bilingual view conformance:

- **SR6 View identity:** `RequirementView` retains the complete typed requirement;
  text is derived deterministically, never parsed back heuristically. A locale
  switch changes labels/templates and direction only. Law and semantic slot IDs
  remain stable; expressions and source identities use Unicode bidi isolates.
- **SR7 One target:** English and Arabic views round-trip to the same requirement,
  including scope, exact predicates, timing, grade and sources. Arabic is a view,
  not a separate proof target. Consumers apply `direction()` to the display.
- The corpus covers all seven physics rows, with immutable-history prohibition
  and compensating-correction obligation as separate requirements. Inputs are
  typed bounded collections/totals for one declared partition/dimension/currency;
  these are authoring conformance cases, not admitted aggregate proofs. Terminal
  outcomes use persistent state, not a fictitious event trigger or deadline.
