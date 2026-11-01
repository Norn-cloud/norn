# Structured requirements: closed demo fragment

`StructuredRequirement` is typed authoring over `DeonticRule` and `LawIR`.
The law retains scope, subject, conditions and response; the norm retains grade,
exact sources, authority, exceptions and preferences. No second hierarchy.

- **SR1 Identity:** serialization preserves every law, source, scope and numeric
  expression byte. English/Arabic templates cannot alter semantic fields.
  Structural identities must be nonblank and all source spans well formed.
- **SR2 Timing:** only every-committed-state and same-commit safety are supported.
  Both interpret the condition pointwise in the candidate state; same-commit
  never means an event edge. Eventually, until and any deadline refuse with
  `law.validation.unsupported_temporal`. Unknown timing tokens fail closed at decode.
- **SR3 Grade:** obligations and permissions use always; prohibitions use never.
  A prohibition's response describes the forbidden predicate, not its negation.
  Permission never asserts occurrence or grants runtime authorization. Existing
  executable lowering still refuses permission and defeasible declarations.
- **SR4 Typed predicates:** activation, predicate response and exceptions must
  typecheck as Boolean under the existing bounded ZEN profile. Arithmetic stays
  exact, including decimal spelling; readable templates do not execute. Action
  and transition occurrence responses refuse with typed `UnsupportedResponse`.
- **SR5 Authority:** original identities, grades, authorities and exception
  domains feed the existing grounded superior/posterior/specialis checker.
  No priority is inferred; normative consistency never overrides core physics.

Structural checking confers no ontology admission, runtime enforcement,
signatures or solver evidence. Retain the complete wrapper, including timing,
and run normal LawIR admission. Diagnostics reuse its stable code catalog.
No FRET or timed-liveness claim: deadline breach needs a future clock-aware requirement.

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
