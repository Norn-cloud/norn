import { expect, test } from "bun:test"
import { DECISION_FN } from "../src/builder/index.ts"

const schema = (name: string) => Bun.file(new URL(`../../../crates/norn-builtins/src/schemas/${name}.json`, import.meta.url)).json()

test("the decision handle restates the norn-builtins contract exactly", async () => {
  expect(DECISION_FN.id).toBe("norn.decision")
  expect(DECISION_FN.version).toBe("1")
  expect(DECISION_FN.input).toEqual(await schema("decision.input"))
  expect(DECISION_FN.output).toEqual(await schema("decision.output"))
  const errors = await schema("decision.error")
  expect(DECISION_FN.errors.map((error) => error.tag)).toEqual(errors.properties._tag.enum)
})
