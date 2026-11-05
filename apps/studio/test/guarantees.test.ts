import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { join } from "node:path"
import { cases, field, levelWords, scalar, verdictWords } from "../src/lib/guarantees.ts"
import type { Guarantees } from "../src/lib/transport.ts"

// Recorded model-checker documents (fixtures/demo/guarantees).
const recorded = (name: string) =>
  JSON.parse(readFileSync(join(import.meta.dir, "../../../fixtures/demo/guarantees", `constitution.${name}.guarantees.json`), "utf8")) as Guarantees

test("the declared law holds on every eligible goal: 3 of 3 discharged, 7 of 10 unknown", () => {
  const original = recorded("original")
  expect(verdictWords(original.verdict)).toBe("Holds in every checked case")
  expect([original.discharged, original.eligible, original.total]).toEqual([3, 3, 10])
  expect(original.goals.filter(({ verdict }) => verdict.kind === "unknown")).toHaveLength(7)
  expect(levelWords(original.level)).toBe("Proved on the model (every case within the bounds)")
})

test("the broken copy is refuted by the exact counterexample 2 g in, 1 g out, 0 g loss", () => {
  const mutant = recorded("mutant")
  expect(verdictWords(mutant.verdict)).toBe("Refuted")
  const scenario = mutant.goals.find(({ scenario }) => scenario?.kind === "counterexample")?.scenario
  const after = scenario?.states.at(-1)?.values ?? {}
  expect(Object.entries(after).map(([name, value]) => `${field(name)} ${scalar(value)}`).sort())
    .toEqual(["input fine g 2 g", "loss fine g 0 g", "output fine g 1 g"])
})

test("a scaled coefficient reads as exact decimal text", () => {
  expect(scalar({ coefficient: "125", scale: 2, unit: { code: "g", system: "ucum" } })).toBe("1.25 g")
  expect(scalar({ coefficient: "-5", scale: 3 })).toBe("-0.005")
})

test("the declared bounds name how many cases were checked", () => {
  expect(cases(recorded("original").boundsJson)).toBe(77)
  expect(cases(JSON.stringify({ profile: {} }))).toBeUndefined()
})
