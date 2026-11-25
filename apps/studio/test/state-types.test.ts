import { expect, test } from "bun:test"
import { matchActionState, matchReadState, matchWorkspaceState, type ActionCallState, type ReadState, type WorkspaceState } from "../src/lib/state/index.ts"

/**
 * #1762 Milestone A: every state is handled, or the page does not compile. Each `@ts-expect-error` row
 * is checked by `tsc` (`bun run typecheck` covers `test/`): if a matcher ever accepts arms with a state
 * left out, or a state outside its union, the directive is unused and the typecheck fails.
 */
type Read = ReadState<{ readonly id: string }, { readonly code: string }>
type Call = ActionCallState<{ readonly commit: number }, { readonly actionId: string }, { readonly id: string }, { readonly code: string }>
type Workspace = WorkspaceState<{ readonly orgId: string }, "live reader", "recorded reader">

const noop = () => "x"
const readArms = { pending: noop, ok: noop, refused: noop, not_served: noop, unavailable: noop, answer_moved: noop, timed_out: noop }
const callArms = { editing: noop, confirming: noop, sending: noop, pending_approval: noop, committed: noop, refused: noop, not_served: noop, unavailable: noop, timed_out: noop, undoing: noop, undo_failed: noop, undone: noop }
const workspaceArms = { deciding: noop, unavailable: noop, recorded: noop, live: noop }

export function missingBranches(read: Read, call: Call, workspace: Workspace): readonly string[] {
  const { timed_out: _timedOut, ...readWithoutTimeout } = readArms
  const { undo_failed: _undoFailed, ...callWithoutUndoFailed } = callArms
  const { deciding: _deciding, ...workspaceWithoutDeciding } = workspaceArms
  return [
    // @ts-expect-error: a read's `timed_out` state has no arm
    matchReadState(read, readWithoutTimeout),
    // @ts-expect-error: a call's `undo_failed` state has no arm
    matchActionState(call, callWithoutUndoFailed),
    // @ts-expect-error: the workspace's `deciding` state has no arm
    matchWorkspaceState(workspace, workspaceWithoutDeciding),
    matchReadState(read, readArms),
    matchActionState(call, callArms),
    matchWorkspaceState(workspace, workspaceArms),
  ]
}

/** A hand-written switch is exhaustive too: a state it leaves out reaches `never`, and does not compile. */
export function switched(read: Read): string {
  switch (read.kind) {
    case "pending": return "pending"
    case "ok": return read.document.id
    case "refused": return read.refusal.code
    case "not_served": case "unavailable": case "answer_moved": return read.error.message
    default: {
      // @ts-expect-error: `timed_out` is left out, so the default is not `never`
      const left: never = read
      return left
    }
  }
}

export function closed(): readonly unknown[] {
  return [
    // @ts-expect-error: no read state is called `failed`
    ((): Read => ({ kind: "failed" }))(),
    // @ts-expect-error: a document's freshness is live, snapshot or stale
    ((): Read => ({ kind: "ok", document: { id: "r" }, freshness: "cached" }))(),
    // @ts-expect-error: a call that never answered keeps the call it sent
    ((): Call => ({ kind: "timed_out", message: null }))(),
    // @ts-expect-error: a refusal carries the cell's own fields
    ((): Call => ({ kind: "refused" }))(),
  ]
}

test("each matcher calls the arm its state names, with that state", () => {
  const read = (state: Read) => matchReadState(state, {
    pending: () => "pending", ok: ({ document, freshness }) => `ok ${document.id} ${freshness}`, refused: ({ refusal }) => `refused ${refusal.code}`,
    not_served: ({ error }) => `not served ${error.message}`, unavailable: ({ retry }) => `unavailable ${retry}`,
    answer_moved: () => "moved", timed_out: () => "timed out",
  })
  expect(read({ kind: "pending" })).toBe("pending")
  expect(read({ kind: "ok", document: { id: "r1" }, freshness: "stale" })).toBe("ok r1 stale")
  expect(read({ kind: "refused", refusal: { code: "authz.denied" } })).toBe("refused authz.denied")
  expect(read({ kind: "not_served", error: new Error("Records are not served") })).toBe("not served Records are not served")
  expect(read({ kind: "unavailable", error: new Error("down"), retry: true })).toBe("unavailable true")
  expect(read({ kind: "answer_moved", error: new Error("moved") })).toBe("moved")
  expect(read({ kind: "timed_out", error: new Error("late") })).toBe("timed out")

  const sent = { input: "{}", key: "k1" }
  const call = (state: Call) => matchActionState(state, { ...callArms, committed: ({ receipt, undo }) => `committed ${receipt.commit} ${undo?.actionId ?? "none"}`, timed_out: ({ sent: again }) => `timed out ${again.key}`, refused: ({ code }) => `refused ${code}` })
  expect(call({ kind: "committed", receipt: { commit: 7 }, undo: null })).toBe("committed 7 none")
  expect(call({ kind: "timed_out", sent, message: null })).toBe("timed out k1")
  expect(call({ kind: "refused", code: "action.denied" })).toBe("refused action.denied")
  expect(call({ kind: "editing" })).toBe("x")

  const workspace = (state: Workspace) => matchWorkspaceState(state, { ...workspaceArms, live: ({ scope, reader }) => `${scope.orgId} ${reader}`, recorded: ({ reader }) => reader })
  expect(workspace({ kind: "live", scope: { orgId: "org_a" }, reader: "live reader" })).toBe("org_a live reader")
  expect(workspace({ kind: "recorded", reader: "recorded reader" })).toBe("recorded reader")
  expect(workspace({ kind: "deciding" })).toBe("x")
  // A state from outside the union (an old cache, a hand-made object) is refused, never matched to an arm.
  expect(() => read({ kind: "failed" } as unknown as Read)).toThrow("Unknown state")
})
