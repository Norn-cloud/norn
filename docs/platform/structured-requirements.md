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

- **SR6 View identity:** locale changes only display labels/direction; stable law/slot
  IDs and complete IR remain. Bidi controls have visible escapes inside isolates.
  Display is never parsed as free prose. Duplicate exception IDs are refused.
- **SR7 One target:** round trips retain scope, predicates, timing, grade and sources;
  Arabic is a view, not a proof target. Consumers honor `direction()`.
- Eight requirements cover seven physics categories; history prohibition and
  compensation are separate. Activation predicates are row-specific typed data.
  Inputs are bounded partition/dimension/currency data: authoring conformance,
  not admitted aggregate proofs. Terminal history is state; posted facts stay immutable.
