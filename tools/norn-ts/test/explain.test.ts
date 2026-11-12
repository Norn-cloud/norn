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
import { CHECKER_PKG_ENV } from "../src/checker-abi.ts"
import { admissionInput, checkPack, compilePack } from "../src/pack.ts"
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
    expect(Object.keys(diagnostic).filter((key) => key !== "source" && key !== "evidence").sort()).toEqual(V2_KEYS)
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
    expect(refusedJson(root, ["explain", "widget", "x"], 2)[0]).toMatchObject({ code: "cli.project.usage" })
    expect(refusedJson(root, ["explain", "law", "x", "--locale", "ar"], 2)[0]).toMatchObject({ code: "cli.project.usage" })
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

test.skipIf(!checker)("explain and check give the checker the same input: the installed base is never a candidate", () => {
  const root = mkdtempSync(join(tmpdir(), "norn-explain-"))
  try {
    const dir = join(root, "shop")
    expect(norn(root, ["init", dir, "--no-install"]).status).toBe(0)
    const pack = compilePack(join(dir, "src"), { scope: { orgId: "local", environmentId: "dev" }, root: dir })
    if (!pack.ok || !checker?.explainArtifactSet) throw new Error(JSON.stringify(pack))
    const entity = pack.artifacts.find((artifact) => (artifact as { kind: string }).kind === "entity")!
    const action = pack.artifacts.find((artifact) => (artifact as { kind: string }).kind === "action")!
    const verdicts = (candidates: typeof pack.artifacts, base: typeof pack.artifacts) => {
      const input = admissionInput(candidates, base)
      const checked = checkPack(checker!, candidates, base)
      const explained = checker!.explainArtifactSet!(input.artifacts, input.context, "action", "shop.product.add", { requirements: [] })
      // One input, one verdict, and for a refusal the same codes.
      expect(explained.admitted).toBe(checked.admitted)
      if (!checked.admitted) {
        expectV2(explained.diagnostics as unknown as Array<Record<string, unknown>>)
        expect(explained.diagnostics.map((diagnostic) => diagnostic.code).sort())
          .toEqual(checked.diagnostics.diagnostics.map((diagnostic) => diagnostic.code).sort())
      }
      return explained
    }
    // The entity installed as base: admitted, and the report says it is not a candidate.
    const installed = verdicts([action], [entity])
    expect(installed.admitted).toBe(true)
    const closure = (installed.report as { closure: { artifacts: Array<{ artifactId: string; candidate: boolean }> } }).closure.artifacts
    expect(closure.find((artifact) => artifact.artifactId === (entity as { artifactId: string }).artifactId)).toMatchObject({ candidate: false })
    expect(closure.find((artifact) => artifact.artifactId === (action as { artifactId: string }).artifactId)).toMatchObject({ candidate: true })
    // The same entity as a candidate too: it shadows the installed base, and both refuse.
    expect(verdicts([action, entity], [entity]).admitted).toBe(false)
  } finally { rmSync(root, { recursive: true, force: true }) }
}, 120_000)
