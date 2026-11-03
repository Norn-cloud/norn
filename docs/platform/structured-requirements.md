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

## Locale wording (LAW-WORDING-LOCALE-1)

Laws are language agnostic. `requirement.wording` is an ordered map keyed by
well-formed BCP 47 tags. Each value declares `direction` (`ltr` or `rtl`),
`scope`, `activation`, `component`, `response`, and complete `labels` and `terms`
records. The checked-in `fixtures/wording/locales.json` and `french.json` show
complete entries. Adding a language requires data only, including all renderer
labels and phrases. Text is reviewed display data, never executable input.

Admission and rendering validate every declared locale. Missing or blank slots,
labels or terms refuse with `law.wording.incomplete_locale`; malformed tags use
`law.wording.invalid_locale`; duplicate tags, compared case insensitively before
map insertion, use `law.wording.duplicate_locale`. Rendering and direction
inspection take the requested tag and refuse an unavailable tag with
`law.wording.unknown_locale`. BCP 47 tag casing does not affect selection; no
language, region or default fallback occurs. Direction comes only from the
entry. Unicode controls in all display text remain visibly escaped, while
semantic identifiers and expressions retain Unicode isolation.

Named scenarios: `wording_third_locale_is_data_only_and_unknown_locale_refuses`,
`wording_incomplete_locale_refuses_even_when_rendering_another_locale`, and
`wording_tags_duplicates_and_nonblank_text_are_strict`.
`wording_locale_round_trip_preserves_semantics_and_rendering` samples arbitrary
reviewed text while retaining exact normative fields and deterministic display.
