import { expect, test } from "bun:test"
import { copyFileSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join, resolve } from "node:path"
import ts from "typescript"
import { CodegenError, generateTypes } from "../src/codegen.ts"
import { admit, compilePack } from "../src/pack.ts"
import { CHECKER_PKG_ENV, checker } from "./checker.ts"

if (!checker && process.env.CI) throw new Error(`${CHECKER_PKG_ENV} is required in CI`)

const ROOT = resolve(import.meta.dir, "..")
const FIXTURES = join(ROOT, "fixtures/codegen")
const DOMAINS = ["tracking", "ml"] as const

function compiled(domain: string) {
  const pack = compilePack(join(FIXTURES, domain), { scope: { orgId: "org_a", environmentId: "dev" }, root: ROOT })
  if (!pack.ok) throw new Error(JSON.stringify(pack.failures))
  return pack
}

function cli(args: readonly string[]) {
  const run = Bun.spawnSync(["bun", join(ROOT, "src/cli.ts"), "codegen", ...args], { cwd: ROOT, stdout: "pipe", stderr: "pipe" })
  return { code: run.exitCode, stdout: run.stdout.toString(), stderr: run.stderr.toString() }
}

for (const domain of DOMAINS) {
  test(`CODEGEN-TS-1 (${domain}): one deterministic .d.ts per admitted set, byte for byte the committed golden`, () => {
    const pack = compiled(domain)
    if (checker) expect(admit(checker, pack).report.admitted).toBe(true)
    const text = generateTypes(pack.artifacts)
    expect(text).toBe(readFileSync(join(FIXTURES, `${domain}.d.ts`), "utf8"))
    // Artifact order and envelope provenance never move a byte.
    expect(generateTypes([...pack.artifacts].reverse())).toBe(text)
    // Types only: no runtime declaration of any kind.
    expect(text).not.toMatch(/^\s*export\s+(const|let|var|function|class|enum|default)\b/m)
    expect(text).not.toMatch(/\bdeclare\s+(const|let|var|function|class)\b/)
  })
}

test("CODEGEN-TS-2: --check passes on the golden and fails on drift or a missing file", () => {
  const dir = mkdtempSync(join(tmpdir(), "norn-codegen-"))
  try {
    const set = join(dir, "tracking.json")
    writeFileSync(set, JSON.stringify(compiled("tracking").artifacts))
    const out = join(dir, "tracking.d.ts")
    copyFileSync(join(FIXTURES, "tracking.d.ts"), out)
    expect(cli(["--ts", set, "--out", out, "--check"]).code).toBe(0)

    writeFileSync(out, readFileSync(out, "utf8").replace('"tracking.Parcel"', '"tracking.Parcel2"'))
    const drift = cli(["--ts", set, "--out", out, "--check"])
    expect(drift.code).toBe(1)
    expect(drift.stderr).toContain("is out of date")
    // --check never writes.
    expect(readFileSync(out, "utf8")).toContain('"tracking.Parcel2"')

    const missing = cli(["--ts", set, "--out", join(dir, "absent.d.ts"), "--check"])
    expect(missing.code).toBe(1)
    expect(missing.stderr).toContain("is missing")

    // Without --check the file is regenerated, after which --check passes again.
    expect(cli(["--ts", set, "--out", out]).code).toBe(0)
    expect(readFileSync(out, "utf8")).toBe(readFileSync(join(FIXTURES, "tracking.d.ts"), "utf8"))
    expect(cli(["--ts", set, "--out", out, "--check"]).code).toBe(0)

    // A changed definition is drift too: the digest pins the typed set.
    const changed = compiled("ml").artifacts
    writeFileSync(set, JSON.stringify(changed))
    expect(cli(["--ts", set, "--out", out, "--check"]).code).toBe(1)
    expect(cli(["--ts", set, "--check"]).code).toBe(2)
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})

test("CODEGEN-TS-3: a shape codegen does not model fails the run instead of widening to unknown", () => {
  const artifacts = structuredClone(compiled("ml").artifacts) as Array<{ kind: string; payload: { inputSchema?: { properties: Record<string, unknown> } } }>
  const action = artifacts.find((a) => a.kind === "action")!
  action.payload.inputSchema!.properties["epochs"] = { oneOf: [{ type: "integer" }, { type: "string" }] }
  expect(() => generateTypes(artifacts)).toThrow(CodegenError)
  expect(() => generateTypes(artifacts)).toThrow("ml.run.record.action#/inputSchema/properties/epochs: schema member `oneOf` is not modelled by codegen")
})

test("CODEGEN-TS-4: generated maps type the client: unknown actions, wrong fields and JS numbers for decimals do not compile", () => {
  const usage = join(FIXTURES, "usage.ts")
  const program = ts.createProgram([usage], {
    target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext, moduleResolution: ts.ModuleResolutionKind.Bundler,
    lib: ["lib.esnext.d.ts", "lib.dom.d.ts"], types: [], strict: true, noUncheckedIndexedAccess: true,
    exactOptionalPropertyTypes: true, noEmit: true, skipLibCheck: true, allowImportingTsExtensions: true, verbatimModuleSyntax: true,
    noUnusedLocals: false,
  })
  const diagnostics = ts.getPreEmitDiagnostics(program).map((d) => ts.flattenDiagnosticMessageText(d.messageText, "\n"))
  // Zero diagnostics means every `@ts-expect-error` line is a real error (TS2578 otherwise).
  expect(diagnostics).toEqual([])
  const source = readFileSync(usage, "utf8")
  expect(source.match(/^\/\/ @ts-expect-error/gm)?.length).toBe(11)
})
