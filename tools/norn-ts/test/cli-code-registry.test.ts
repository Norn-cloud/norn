import { expect, test } from "bun:test"
import { readFileSync, readdirSync } from "node:fs"
import { resolve } from "node:path"
import { emittedCliCodes, missingCliEmissions, toolEmissions } from "../src/cli-code-registry.ts"
import { TOOL_ROOT } from "../src/profile.ts"

test("cli_registry_requires_actual_emissions", () => {
  const directory = resolve(TOOL_ROOT, "../../crates/norn-checker/conformance/diagnostic-codes")
  const registered = readdirSync(directory).map(file => JSON.parse(readFileSync(resolve(directory, file), "utf8")).code as string)
  const emitted = toolEmissions()
  expect(missingCliEmissions(registered, emitted)).toEqual([])
  expect([...emitted].filter(code => !registered.includes(code))).toEqual([])
})

test("cli_unemitted_code_fails_even_with_comments_and_tests", () => {
  const emitted = emittedCliCodes('// new ProjectError("cli.project.ghost", "comment")\nconst code = "cli.project.ghost"')
  expect(missingCliEmissions(["cli.project.ghost"], emitted)).toEqual(["cli.project.ghost"])
})

test("checker_code_mentioned_only_in_ts_does_not_count", () => {
  expect([...emittedCliCodes('new ProjectError("entity.validation.only_in_ts", "not a Rust emission")')]).toEqual([])
})
