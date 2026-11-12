/**
 * Typed handles of the engine's host built-in Functions (FUNCTION-HOST-1,
 * Core 07). The Rust crate `norn-builtins` owns each contract; these handles
 * restate its schema documents for `perform`, and `test/builtins.test.ts`
 * fails if they drift from `crates/norn-builtins/src/schemas`.
 */
import * as S from "./schema.ts"
import { fn, taggedError, type FnRef } from "./flow.ts"

const at = { file: "tools/norn-ts/src/builder/builtins.ts", line: 1 }

/** One decision option: `id` is `[a-z][a-z0-9_]*`; its effect names exactly one of an action or a binding. */
const OPTION = S.record({
  id: S.text(),
  label: S.text(),
  effect: S.record({ action: S.text(), binding: S.text() }, ["action", "binding"]),
  scope: S.text(),
})

/**
 * `norn.decision@1`: ask a person (D58). Lower it as `perform` followed by `route` on `choice`.
 * Only the cell resolves it, by issuing the receipt for a verified decider.
 * The schemas cannot carry these bounds, so the cell enforces them: question and note at most 500
 * characters, one to nine options, unique option ids, `agentPick` naming an option, `expiresInMs` at
 * most seven days, and `choice` naming an option.
 */
export const DECISION_FN: FnRef = fn({
  id: "norn.decision",
  version: "1",
  input: S.record(
    {
      question: S.text(),
      risk: S.text(["low", "medium", "high"]),
      options: S.array(OPTION),
      agentPick: S.text(),
      decider: S.record({ roles: S.array(S.text()), law: S.text() }, ["law"]),
      expiresInMs: S.integer,
    },
    ["agentPick", "expiresInMs"],
  ),
  output: S.record({ choice: S.text(), note: S.text() }, ["note"]),
  errors: [taggedError("Expired", S.record({}), { at }), taggedError("Withdrawn", S.record({}), { at })],
  at,
})

/**
 * `norn.action@1` (ACTION-STEP-1): a step that calls the governed action `action` with an input of
 * `input`'s shape (the action's own input record). The cell executes it through the ordinary
 * action path, under the run starter's authority for the whole run; a person who decided an
 * earlier step lends it nothing. Its output is the action's target. Its typed failures are `Denied`
 * (the starter may not call the action) and `Refused` (any other refusal, with its `code`). One
 * handle per action: the input schema is the action's, the Function is the same.
 */
export function actionFn(action: string, input: S.ObjectSchema): FnRef {
  return fn({
    id: "norn.action",
    version: "1",
    input: S.record({ action: S.text([action]), input }),
    output: S.record({ objectType: S.text(), objectId: S.text(), version: S.integer }),
    errors: [taggedError("Denied", S.record({}), { at }), taggedError("Refused", S.record({ code: S.text() }, ["code"]), { at })],
    at,
  })
}
