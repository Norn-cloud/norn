import { expect, test } from "bun:test"
import { DECISION_FN, S, actionFn } from "../src/builder/index.ts"

const schema = (name: string) => Bun.file(new URL(`../../../crates/norn-builtins/src/schemas/${name}.json`, import.meta.url)).json()

test("the decision handle restates the norn-builtins contract exactly", async () => {
  expect(DECISION_FN.id).toBe("norn.decision")
  expect(DECISION_FN.version).toBe("1")
  expect(DECISION_FN.input).toEqual(await schema("decision.input"))
  expect(DECISION_FN.output).toEqual(await schema("decision.output"))
  const errors = await schema("decision.error")
  expect(DECISION_FN.errors.map((error) => error.tag)).toEqual(errors.properties._tag.enum)
})

test("an action step handle restates the norn-builtins contract, with the action's own input", async () => {
  const input = S.record({ orderId: S.text() })
  const handle = actionFn("ops.refund.approve", input)
  expect(handle.id).toBe("norn.action")
  expect(handle.version).toBe("1")
  expect(handle.input).toEqual(S.record({ action: S.text(["ops.refund.approve"]), input }))
  expect(handle.output).toEqual(await schema("action.output"))
  const errors = await schema("action.error")
  expect(handle.errors.map((error) => error.tag)).toEqual(errors.properties._tag.enum)
})
