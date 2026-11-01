# Lifecycle refusal contract

These invariants apply to the shared kernel contract before any transport or
screen renders a refusal. They do not change transition admission.

- **REF-LIFECYCLE-READABLE:** a lifecycle denial names the object and transition
  in readable text. A source-state denial also names the lifecycle and current
  state. Missing state reads as "unset"; multiple permitted states read as
  alternatives. Rust Option and collection Debug syntax is never the presentation.
- **REF-LIFECYCLE-OPERANDS:** the stable refusal code is preserved. The structured
  reason identifies the violated lifecycle law, subject and named operands from
  the effective definition. Missing state is null; permitted states are an array.
  Rendered text does not replace or erase these values.
- **REF-ACTION-PRESERVATION:** wrapping a decision refusal in an action refusal
  preserves its complete wire representation, including the structured reason.
  JSON round trips preserve the representation.
- **REF-REDACTION:** internal and visibility refusals retain their fixed safe
  messages and expose no structured lifecycle operands.
- **REF-ATOMIC-DENIAL:** refusing an inadmissible transition changes no facts,
  events, commands or timers.

## Correspondence scenarios

`refusal_taxonomy::lifecycle_reason_preserves_definition_operands_through_action_wrapper`
checks readable messages, missing state, multiple allowed states, exact wire
operands, action wrapping and JSON round trips.

`decision_commit::decision_refuses_transition_from_wrong_state` obtains the
permitted state from a compiled definition through the real commit path and
checks both the structured operands and unchanged durable state.

`refusal_taxonomy::existence_never_leaks_matrix`,
`internal_refusals_never_carry_details` and
`nested_wrappers_preserve_internal_redaction` cover redaction.
