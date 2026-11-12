import { expect, test } from "bun:test"
import { readFileSync, readdirSync, writeFileSync } from "node:fs"
import { resolve } from "node:path"
import { emittedCliCodes, missingCliEmissions, toolEmissions } from "../src/cli-code-registry.ts"
import { cliCodes } from "../src/cli-diagnostic.ts"
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

test("cli_diagnostics_snapshot_is_the_registry", () => {
  // `generated/cli-diagnostics.json` is what the CLI prints guidance from. It must be
  // exactly the registry's `cli.*` codes with the registry's guidance.
  const conformance = resolve(TOOL_ROOT, "../../crates/norn-checker/conformance")
  const guidance = new Map((JSON.parse(readFileSync(resolve(conformance, "diagnostic-guidance.json"), "utf8")) as Array<{ code: string; fix?: string; no_fix?: string }>)
    .map((entry) => [entry.code, entry] as const))
  const expected = readdirSync(resolve(conformance, "diagnostic-codes")).sort()
    .map((file) => JSON.parse(readFileSync(resolve(conformance, "diagnostic-codes", file), "utf8")) as { code: string; tier: string; severity: string; message: string })
    .filter((record) => record.code.startsWith("cli."))
    .map((record) => ({ code: record.code, tier: record.tier, severity: record.severity, message: record.message,
      fix: guidance.get(record.code)?.fix ?? null, no_fix: guidance.get(record.code)?.no_fix ?? null }))
  const snapshot = resolve(TOOL_ROOT, "generated/cli-diagnostics.json")
  if (process.env.NORN_BLESS === "1") writeFileSync(snapshot, `${JSON.stringify({ codes: expected }, null, 2)}\n`)
  expect(JSON.parse(readFileSync(snapshot, "utf8"))).toEqual({ codes: expected })
  expect([...cliCodes().keys()]).toEqual(expected.map((entry) => entry.code))
  // Every code the CLI can emit has guidance to print.
  for (const code of toolEmissions()) expect(typeof cliCodes().get(code)?.fix === "string" || typeof cliCodes().get(code)?.no_fix === "string").toBe(true)
})
