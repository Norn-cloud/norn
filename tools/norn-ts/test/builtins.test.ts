import { expect, test } from "bun:test"
import { DECISION_FN, S, actionFn, questionFn } from "../src/builder/index.ts"

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
  const trace = S.record(
    { traceId: S.text(), intentRef: S.text(), sessionDigest: S.text() },
    ["intentRef", "sessionDigest"],
  )
  expect(handle.id).toBe("norn.action")
  expect(handle.version).toBe("1")
  expect(handle.input).toEqual(S.record({ action: S.text(["ops.refund.approve"]), input, trace }, ["trace"]))
  expect(handle.output).toEqual(await schema("action.output"))
  const errors = await schema("action.error")
  expect(handle.errors.map((error) => error.tag)).toEqual(errors.properties._tag.enum)

  const pinned = actionFn("ops.refund.approve", input, { pinFields: ["artifactId", "version", "definitionsVersion"] })
  const pin = S.record({ artifactId: S.text(), version: S.text(), definitionsVersion: S.integer })
  expect(pinned.input).toEqual(
    S.record({ action: S.text(["ops.refund.approve"]), input, trace, pin }, ["trace", "pin"]),
  )
  expect(pin.required).toEqual(["artifactId", "version", "definitionsVersion"])
  expect(() => actionFn("ops.refund.approve", input, { pinFields: [] as unknown as ["artifactId"] })).toThrow(
    /at least one pin field/,
  )
  expect(() => actionFn("ops.refund.approve", input, { pinFields: ["toString"] as unknown as ["artifactId"] })).toThrow(
    /unknown actionFn pin field/,
  )
  expect(() =>
    actionFn("ops.refund.approve", input, { pinFields: ["__proto__"] as unknown as ["artifactId"] }),
  ).toThrow(/unknown actionFn pin field/)
})

test("a question step handle restates the norn-builtins contract exactly", async () => {
  const trace = S.record(
    { traceId: S.text(), intentRef: S.text(), sessionDigest: S.text() },
    ["intentRef", "sessionDigest"],
  )
  const basic = questionFn("inventory.on_hand")
  expect(basic.id).toBe("norn.question")
  expect(basic.version).toBe("1")
  expect(basic.input).toEqual(S.record({ question: S.text(["inventory.on_hand"]), trace }, ["trace"]))
  expect(basic.output).toEqual(await schema("question.output"))

  const errors = await schema("question.error")
  expect(basic.errors.map((error) => error.tag)).toEqual(errors.properties._tag.enum)
  const stale = basic.errors.find((error) => error.tag === "StaleSource")
  expect(stale?.fields).toEqual(
    S.record(
      {
        code: S.text(),
        freshness: S.text(errors.properties.freshness.enum),
        complete: S.boolean,
        sources: S.array(S.text()),
      },
      ["code", "freshness", "complete", "sources"],
    ),
  )

  const inputDoc = await schema("question.input")
  const params = S.record({ warehouse: S.text() })
  const full = questionFn("inventory.on_hand", {
    params,
    asOf: true,
    freshnessFields: ["maxAgeMs", "requireExact"],
    pinFields: ["artifactId", "version", "digest", "definitionsVersion"],
  })
  expect(Object.keys(full.input.properties).sort()).toEqual(Object.keys(inputDoc.properties).sort())
  expect(full.input.required).toEqual(inputDoc.required)

  const pin = full.input.properties.pin as S.ObjectSchema
  expect(Object.keys(pin.properties).sort()).toEqual(Object.keys(inputDoc.properties.pin.properties).sort())
  expect(pin.required).toEqual(["artifactId", "version", "digest", "definitionsVersion"])

  const freshness = full.input.properties.freshness as S.ObjectSchema
  expect(Object.keys(freshness.properties).sort()).toEqual(
    Object.keys(inputDoc.properties.freshness.properties).sort(),
  )
  expect(freshness.required).toEqual(["maxAgeMs", "requireExact"])

  expect(() => questionFn("inventory.on_hand", { pinFields: [] as unknown as ["artifactId"] })).toThrow(
    /at least one pin field/,
  )
  expect(() =>
    questionFn("inventory.on_hand", { pinFields: ["toString"] as unknown as ["artifactId"] }),
  ).toThrow(/unknown questionFn pin field/)
  expect(() =>
    questionFn("inventory.on_hand", { pinFields: ["__proto__"] as unknown as ["artifactId"] }),
  ).toThrow(/unknown questionFn pin field/)
  expect(() =>
    questionFn("inventory.on_hand", { freshnessFields: [] as unknown as ["maxAgeMs"] }),
  ).toThrow(/at least one freshness field/)
  expect(() =>
    questionFn("inventory.on_hand", { freshnessFields: ["allowStale"] as unknown as ["maxAgeMs"] }),
  ).toThrow(/unknown questionFn freshness field/)
})
