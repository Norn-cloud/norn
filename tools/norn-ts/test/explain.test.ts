/**
 * #517 `norn explain` in a project: init → explain → break → explain names the
 * source line and the registry's guidance → repair. The report is the checker
 * ABI's `norn/explain@2`, the same one the native `norn explain --defs` prints.
 *
 * One diagnostic model: whatever makes `norn explain` refuse, every diagnostic
 * it prints is `norn/diagnostic@2` with a registered code and exactly that
 * code's registered guidance (`expectV2`).
 */
import { expect, test } from "bun:test"
import { spawnSync } from "node:child_process"
import { mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join, resolve } from "node:path"
import { CHECKER_PKG_ENV, type CheckerAbi } from "../src/checker-abi.ts"
import { explain, FAILED, type ExplainHost } from "../src/explain.ts"
import { admit, artifactId, compilePack, isArtifact, type PackResult } from "../src/pack.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { checker } from "./checker.ts"

const CLI = join(TOOL_ROOT, "src/cli.ts")
if (process.env.CI && !process.env[CHECKER_PKG_ENV]) throw new Error(`explain tests require ${CHECKER_PKG_ENV}`)

function norn(cwd: string, args: string[], env: Record<string, string | undefined> = process.env) {
  const run = spawnSync(process.execPath, [CLI, ...args], { cwd, env, encoding: "utf8" })
  return { status: run.status, out: run.stdout, err: run.stderr }
}

// The registry, read from its own files: the per-code records and the guidance.
const CONFORMANCE = resolve(TOOL_ROOT, "../../crates/norn-checker/conformance")
const REGISTRY = new Map(readdirSync(join(CONFORMANCE, "diagnostic-codes")).map((file) => {
  const record = JSON.parse(readFileSync(join(CONFORMANCE, "diagnostic-codes", file), "utf8")) as { code: string; tier: string; severity: string }
  return [record.code, record] as const
}))
const GUIDANCE = new Map((JSON.parse(readFileSync(join(CONFORMANCE, "diagnostic-guidance.json"), "utf8")) as Array<{ code: string; fix?: string; no_fix?: string }>)
  .map((entry) => [entry.code, entry] as const))
const V2_KEYS = ["artifactId", "code", "fix", "format", "message", "no_fix", "phase", "remediation", "retryable", "severity", "span", "tier"]

/** Every diagnostic is `norn/diagnostic@2`, registered, and carries its own code's guidance. */
function expectV2(diagnostics: ReadonlyArray<Record<string, unknown>>): void {
  expect(diagnostics.length).toBeGreaterThan(0)
  for (const diagnostic of diagnostics) {
    const code = String(diagnostic.code)
    const record = REGISTRY.get(code)
    const guidance = GUIDANCE.get(code)
    if (!record || !guidance) throw new Error(`${code} is not a registered diagnostic code`)
    expect(Object.keys(diagnostic).filter((key) => !["source", "evidence", "compiler"].includes(key)).sort()).toEqual(V2_KEYS)
    expect(diagnostic).toMatchObject({
      format: "norn/diagnostic@2", tier: record.tier, phase: record.tier, severity: record.severity, retryable: false,
      fix: guidance.fix ?? null, no_fix: guidance.no_fix ?? null,
    })
    expect(typeof diagnostic.fix === "string").not.toBe(typeof diagnostic.no_fix === "string")
    expect(typeof diagnostic.message).toBe("string")
  }
}

/** A refused `--json` run: exit status, the `norn/explain@2` envelope and v2 diagnostics. */
function refusedJson(cwd: string, args: string[], status: number, env: Record<string, string | undefined> = process.env) {
  const run = norn(cwd, [...args, "--json"], env)
  expect({ status: run.status, err: run.err }).toEqual({ status, err: "" })
  const response = JSON.parse(run.out) as { diagnostics: Array<Record<string, any>> }
  expect(response).toMatchObject({ format: "norn/explain@2", admitted: false, report: null })
  expectV2(response.diagnostics)
  return response.diagnostics
}

const customer = (identity: string) => `import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Customer = Norn.type({
  name: "shop.Customer", title: "Customer", version: "0.1.0",
  properties: Schema.Struct({ email: Schema.String, name: Schema.String }),
  identity: [{ name: "primary", properties: ["${identity}"] }],
})
`

// TypeScript accepts this module; the Rust checker refuses the expression on line 8.
const refusedByTheChecker = `import { Schema } from "effect"
import { Norn } from "@norn/authoring"
export const Task = Norn.type({name: "demo.Task", title: "Task", version: "1.0.0", properties: Schema.Struct({id: Schema.String}), identity: [{name: "primary", properties: ["id"]}]})
export const Rule = Norn.decision({
  id: "demo.rule.expression", title: "Expression", version: "1.0.0", contract: "decide", scopeRequired: ["orgId", "environmentId"],
  input: Schema.Struct({slice: Schema.Struct({}), event: Schema.Struct({kind: Schema.String})}),
  output: Schema.Struct({facts: Schema.Array(Schema.Struct({op: Schema.Literal("delete"), objectType: Schema.Literal("demo.Task"), key: Schema.String})), commands: Schema.Array(Schema.Struct({actionId: Schema.String, value: Schema.Boolean})), timers: Schema.Array(Schema.Struct({id: Schema.String}))}),
}, (input) => ({facts: [], commands: [{actionId: "probe", value: Norn.unsafeZen<boolean>("(")}], timers: []}))
`

test.skipIf(!process.env[CHECKER_PKG_ENV])("explain_project_roundtrip: explain, break, see the source line and the fix, repair", () => {
  const root = mkdtempSync(join(tmpdir(), "norn-explain-"))
  const dir = join(root, "shop")
  try {
    expect(norn(root, ["init", dir, "--no-install"]).status).toBe(0)
    const explained = norn(dir, ["explain", "action", "shop.product.add", "--json"])
    expect(explained.status).toBe(0)
    expect(JSON.parse(explained.out)).toMatchObject({ format: "norn/explain@2", admitted: true, diagnostics: [], report: { format: expect.any(String) } })
    const text = norn(dir, ["explain", "action", "shop.product.add"])
    expect(text.status).toBe(0)
    expect(text.out).toContain("closure: ")

    // A source that does not compile: the registered compile code, the source line and its fix.
    writeFileSync(join(dir, "src/customer.ts"), customer("uuid"))
    const uncompiled = refusedJson(dir, ["explain", "action", "shop.product.add"], 1)
    expect(uncompiled[0]).toMatchObject({ code: "cli.compile.source_refused", source: { file: "src/customer.ts", line: 7 } })
    // The compiler's own code and whole span are structured members, not only prose.
    expect(uncompiled[0]!.compiler).toMatchObject({ code: expect.stringMatching(/^NORN-/), start: { line: 7, column: expect.any(Number) }, end: { line: expect.any(Number), column: expect.any(Number) } })
    expect(uncompiled[0]!.message).toContain(`: ${uncompiled[0]!.compiler.code}: `)
    // A source error is reported as one with or without the checker package.
    expect(refusedJson(dir, ["explain", "action", "shop.product.add"], 1, { ...process.env, [CHECKER_PKG_ENV]: "" })[0]).toMatchObject({ code: "cli.compile.source_refused" })
    expect(uncompiled[0]!.message).toStartWith("The project's sources did not compile: ")
    const frame = norn(dir, ["explain", "action", "shop.product.add"])
    expect(frame.status).toBe(1)
    expect(frame.out).toContain("src/customer.ts:7")
    expect(frame.out).toContain(`identity: [{ name: "primary", properties: ["uuid"] }],`)
    expect(frame.out).toContain(`  fix: ${GUIDANCE.get("cli.compile.source_refused")!.fix}`)

    writeFileSync(join(dir, "src/customer.ts"), customer("email"))
    expect(norn(dir, ["explain", "concept", "shop.Customer", "--json"]).status).toBe(0)
  } finally { rmSync(root, { recursive: true, force: true }) }
}, 120_000)

test.skipIf(!process.env[CHECKER_PKG_ENV])("a checker refusal is the checker's own findings, with the source line and that code's guidance", () => {
  const root = mkdtempSync(join(tmpdir(), "norn-explain-"))
  const dir = join(root, "shop")
  try {
    expect(norn(root, ["init", dir, "--no-install"]).status).toBe(0)

    // An unknown subject: the checker's own code for it, not a law-binding code.
    const unknown = refusedJson(dir, ["explain", "action", "shop.product.typo"], 1)
    expect(unknown).toHaveLength(1)
    expect(unknown[0]).toMatchObject({ code: "explain.selector.unknown_subject", fix: expect.stringContaining("action, law and concept") })

    // A wording file the entry point refuses: not an array, a requirement of the wrong shape, input over its size limit.
    const wording = join(root, "wording.json")
    writeFileSync(wording, "{}")
    expect(refusedJson(dir, ["explain", "action", "shop.product.add", "--wording", wording, "--locale", "ar"], 2)[0])
      .toMatchObject({ code: "cli.explain.usage", message: expect.stringContaining("JSON array of structured requirements") })
    writeFileSync(wording, `[{"x":1}]`)
    expect(refusedJson(dir, ["explain", "action", "shop.product.add", "--wording", wording, "--locale", "ar"], 1)[0])
      .toMatchObject({ code: "cli.explain.input_refused", message: expect.stringContaining("options") })
    writeFileSync(wording, JSON.stringify([{ pad: "a".repeat(9 * 1024 * 1024) }]))
    expect(refusedJson(dir, ["explain", "action", "shop.product.add", "--wording", wording, "--locale", "ar"], 1)[0])
      .toMatchObject({ code: "cli.explain.input_refused", message: expect.stringContaining("8 MiB") })

    // Sources that compile but the checker refuses: the findings `norn check` reports.
    writeFileSync(join(dir, "src/rule.ts"), refusedByTheChecker)
    const refused = refusedJson(dir, ["explain", "action", "shop.product.add"], 1)
    const mapped = refused.find((diagnostic) => diagnostic.source?.file === "src/rule.ts")
    expect(mapped?.source).toMatchObject({ file: "src/rule.ts", line: 8 })
    const checked = norn(dir, ["check", "src", "--org", "local", "--env", "dev"])
    expect(checked.status).toBe(1)
    // `norn check` also lists the authoring compiler's warnings; the checker's findings are the registered ones.
    const reported = (JSON.parse(checked.out) as { diagnostics: Array<{ code: string }> }).diagnostics.filter((diagnostic) => REGISTRY.has(diagnostic.code))
    expect(refused.map((diagnostic) => diagnostic.code).sort()).toEqual(reported.map((diagnostic) => diagnostic.code).sort())

    // The frame prints the source line and the registered guidance of exactly that code.
    const frame = norn(dir, ["explain", "action", "shop.product.add"])
    expect(frame.status).toBe(1)
    expect(frame.out).toContain(`src/rule.ts:8`)
    const advice = GUIDANCE.get(String(mapped!.code))!
    expect(frame.out).toContain(advice.fix ? `  fix: ${advice.fix}` : `  no automatic fix: ${advice.no_fix}`)
  } finally { rmSync(root, { recursive: true, force: true }) }
}, 120_000)

test("explain refuses with an actionable code outside a project, on bad usage and without the checker", () => {
  const root = mkdtempSync(join(tmpdir(), "norn-explain-"))
  try {
    expect(refusedJson(root, ["explain", "action", "x"], 1)[0]).toMatchObject({ code: "cli.project.not_found" })
    expect(refusedJson(root, ["explain", "widget", "x"], 2)[0]).toMatchObject({ code: "cli.explain.usage" })
    expect(refusedJson(root, ["explain", "law", "x", "--locale", "ar"], 2)[0]).toMatchObject({ code: "cli.explain.usage" })
    expect(norn(root, ["init", join(root, "p"), "--no-install"]).status).toBe(0)
    const missing = refusedJson(join(root, "p"), ["explain", "action", "p.product.add"], 3, { ...process.env, [CHECKER_PKG_ENV]: "" })
    expect(missing[0]).toMatchObject({ code: "cli.setup.checker_unavailable", fix: expect.stringContaining(CHECKER_PKG_ENV) })
    expect(missing[0]!.message).toStartWith("The checker WASM package is unavailable; build it and set NORN_CHECKER_WASM_PKG.")
    // A package that is there but cannot load says why, instead of "unavailable" alone.
    const broken = refusedJson(join(root, "p"), ["explain", "action", "p.product.add"], 3, { ...process.env, [CHECKER_PKG_ENV]: root })
    expect(broken[0]).toMatchObject({ code: "cli.setup.checker_unavailable", message: expect.stringContaining("holds no norn-checker-wasm package") })
    // Without --json the same refusal is a frame with the registered fix.
    const frame = norn(root, ["explain", "action", "x"])
    expect(frame).toMatchObject({ status: 1, out: expect.stringContaining(`  fix: ${GUIDANCE.get("cli.project.not_found")!.fix}`) })
  } finally { rmSync(root, { recursive: true, force: true }) }
})

/** `explain` in this process with parts of its host replaced; returns the exit status and what it wrote. */
async function explained(args: string[], host: Partial<ExplainHost>) {
  let out = ""
  const status = await explain([...args, "--json"], { write: (text) => { out += text }, ...host })
  // Exactly one response, whatever happened.
  expect(out.trimEnd().split("\n")).toHaveLength(1)
  return { status, response: JSON.parse(out) as { format: string; admitted: boolean; report: any; diagnostics: Array<Record<string, any>> } }
}

function project(): { root: string; dir: string } {
  const root = mkdtempSync(join(tmpdir(), "norn-explain-"))
  const dir = join(root, "shop")
  expect(norn(root, ["init", dir, "--no-install"]).status).toBe(0)
  return { root, dir }
}

test("explain is one boundary: whatever is thrown below it is one refused v2 response", async () => {
  const { root, dir } = project()
  try {
    const subject = ["action", "shop.product.add"]
    const failed = (run: { status: number; response: { format: string; admitted: boolean; report: unknown; diagnostics: Array<Record<string, any>> } }, detail: string) => {
      expect(run).toMatchObject({ status: 3, response: { format: "norn/explain@2", admitted: false, report: null } })
      expectV2(run.response.diagnostics)
      expect(run.response.diagnostics).toHaveLength(1)
      expect(run.response.diagnostics[0]).toMatchObject({ code: "cli.explain.failed", message: expect.stringContaining(detail) })
    }
    // Not an Error, not a ProjectError, thrown from a place no path lists.
    failed(await explained(subject, { cwd: dir, compile: () => { throw "a thrown string" } }), "a thrown string")
    failed(await explained(subject, { cwd: dir, compile: () => { throw new TypeError("compiler fell over") } }), "compiler fell over")
    // A checker whose admission throws, and one whose explain entry point returns nonsense.
    const broken = (parts: Partial<CheckerAbi>): ExplainHost["load"] => async () => ({
      checkArtifactSet: () => { throw new RangeError("admission fell over") },
      explainArtifactSet: () => { throw new Error("unreachable") },
      version: () => ({}),
      ...parts,
    })
    failed(await explained(subject, { cwd: dir, load: broken({}) }), "admission fell over")
    // The fallback finding used when the registry itself cannot be read is the registry's entry.
    expect(FAILED).toMatchObject({ tier: REGISTRY.get("cli.explain.failed")!.tier, severity: REGISTRY.get("cli.explain.failed")!.severity, no_fix: GUIDANCE.get("cli.explain.failed")!.no_fix, fix: null })
    expectV2([FAILED as unknown as Record<string, unknown>])
    // A loader that throws is the registered setup refusal, with the reason.
    const unloaded = await explained(subject, { cwd: dir, load: async () => { throw "no loader" } })
    expect(unloaded.status).toBe(3)
    expectV2(unloaded.response.diagnostics)
    expect(unloaded.response.diagnostics[0]).toMatchObject({ code: "cli.setup.checker_unavailable", message: expect.stringContaining("no loader") })
  } finally { rmSync(root, { recursive: true, force: true }) }
}, 120_000)

test.skipIf(!checker)("a checker entry point that throws is a registered refusal, not a stack trace", async () => {
  const { root, dir } = project()
  try {
    const run = await explained(["action", "shop.product.add"], {
      cwd: dir,
      load: async () => ({ ...checker!, explainArtifactSet: () => { throw new Error("options: unknown field `x`") } }),
    })
    expect(run.status).toBe(1)
    expectV2(run.response.diagnostics)
    expect(run.response.diagnostics[0]).toMatchObject({ code: "cli.explain.input_refused", message: expect.stringContaining("unknown field") })
  } finally { rmSync(root, { recursive: true, force: true }) }
}, 120_000)

test.skipIf(!checker)("explain gives the checker the input of norn check's final pass: the installed base is never a candidate", async () => {
  const { root, dir } = project()
  try {
    const pack = compilePack(join(dir, "src"), { scope: { orgId: "local", environmentId: "dev" }, root: dir })
    if (!pack.ok || !checker?.explainArtifactSet) throw new Error(JSON.stringify(pack))
    const entity = pack.artifacts.find((artifact) => isArtifact(artifact) && artifact.kind === "entity")!
    const action = pack.artifacts.find((artifact) => isArtifact(artifact) && artifact.kind === "action")!
    // Through `explain` itself, with a compile that yields an installed base, and a checker that records what it is given.
    const through = async (candidates: typeof pack.artifacts, base: typeof pack.artifacts) => {
      const compiled: PackResult = { ...pack, artifacts: candidates, baseArtifacts: base }
      const given: { check: unknown[][]; explain: unknown[][] } = { check: [], explain: [] }
      const recording: CheckerAbi = {
        ...checker!,
        checkArtifactSet: (artifacts, context) => { given.check.push([artifacts, context ?? null]); return checker!.checkArtifactSet(artifacts, context) },
        explainArtifactSet: (artifacts, context, kind, id, options) => { given.explain.push([artifacts, context ?? null]); return checker!.explainArtifactSet!(artifacts, context, kind, id, options) },
      }
      const run = await explained(["action", "shop.product.add"], { cwd: dir, compile: () => compiled, load: async () => recording })
      // Explain's input is exactly the input of the final check `admit` makes, which is what `norn check` runs.
      expect(given.explain).toHaveLength(1)
      expect(given.explain[0]).toEqual(given.check.at(-1)!)
      expect(given.explain[0]).toEqual([candidates, base.length > 0 ? { base } : null])
      const checked = admit(checker!, compiled as Extract<PackResult, { ok: true }>).report
      expect(run.response.admitted).toBe(checked.admitted)
      if (!checked.admitted) {
        expectV2(run.response.diagnostics)
        expect(run.response.diagnostics.map((diagnostic) => diagnostic.code).sort()).toEqual(checked.diagnostics.diagnostics.map((diagnostic) => diagnostic.code).sort())
      }
      return run
    }
    // The entity installed as base: admitted, and the report says it is not a candidate.
    const installed = await through([action], [entity])
    expect(installed).toMatchObject({ status: 0, response: { admitted: true } })
    const closure = installed.response.report.closure.artifacts as Array<{ artifactId: string; candidate: boolean }>
    expect(closure.find((artifact) => artifact.artifactId === artifactId(entity))).toMatchObject({ candidate: false })
    expect(closure.find((artifact) => artifact.artifactId === artifactId(action))).toMatchObject({ candidate: true })
    // The same entity as a candidate too: it shadows the installed base, and both refuse.
    expect(await through([action, entity], [entity])).toMatchObject({ status: 1, response: { admitted: false } })
  } finally { rmSync(root, { recursive: true, force: true }) }
}, 120_000)
