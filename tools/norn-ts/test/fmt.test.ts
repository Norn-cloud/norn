import { expect, test } from "bun:test"
import { cpSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join, resolve } from "node:path"
import { toJson } from "../src/canonical.ts"
import { formatSource } from "../src/fmt.ts"
import { compilePack } from "../src/pack.ts"
import { TOOL_ROOT } from "../src/profile.ts"

function sources(directory: string): string[] {
  return readdirSync(directory, { recursive: true, encoding: "utf8" }).filter(path => path.endsWith(".ts")).map(path => join(directory, path)).sort()
}

test("fmt_idempotent", () => {
  for (const file of [...sources(join(TOOL_ROOT, "corpus/accept")), ...sources(join(TOOL_ROOT, "fixtures"))]) {
    const once = formatSource(file, readFileSync(file, "utf8"))
    expect(formatSource(file, once), file).toBe(once)
  }
}, 240000)

test("fmt_preserves_semantics", () => {
  const root = mkdtempSync(join(tmpdir(), "norn-fmt-parity-"))
  let compiled = 0
  let unchanged = 0
  try {
    for (const part of ["fixtures", "corpus", "profile", "src"]) cpSync(join(TOOL_ROOT, part), join(root, part), { recursive: true })
    for (const file of [...sources(join(root, "corpus/accept")), ...sources(join(root, "fixtures"))]) {
      const source = readFileSync(file, "utf8")
      const formatted = formatSource(file, source)
      // A file fmt leaves byte-identical compiles to the same artifacts by
      // determinism; only a file fmt changes can change meaning, so only those
      // are compiled before and after (each compile costs seconds).
      if (formatted === source) {
        unchanged++
        continue
      }
      const compile = () => compilePack(root, { scope: { orgId: "org_demo", environmentId: "dev" }, modules: [file], root })
      const before = compile()
      if (!before.ok) continue // Rejection fixtures have no canonical artifact set.
      try {
        writeFileSync(file, formatted)
        const after = compile()
        expect(after.ok, file).toBe(true)
        if (after.ok) expect(after.artifacts.map(value => JSON.stringify(toJson(value))).sort(), file).toEqual(before.artifacts.map(value => JSON.stringify(toJson(value))).sort())
        compiled++
      } finally { writeFileSync(file, source) }
    }
    expect(compiled + unchanged).toBeGreaterThan(20)
  } finally { rmSync(root, { recursive: true }) }
}, 600000)

test("fmt_check_exit_1", async () => {
  const root = mkdtempSync(join(tmpdir(), "norn-fmt-check-"))
  const file = join(root, "product.ts")
  const original = 'export const x={a:1,b:2};\n'
  const run = async (args: string[]) => {
    const process = Bun.spawn([Bun.which("bun") ?? "bun", resolve(TOOL_ROOT, "src/cli.ts"), "fmt", ...args], { cwd: root, stdout: "pipe", stderr: "pipe" })
    const [out, err, status] = await Promise.all([new Response(process.stdout).text(), new Response(process.stderr).text(), process.exited])
    return { out, err, status }
  }
  try {
    writeFileSync(file, original)
    writeFileSync(join(root, "norn.json"), JSON.stringify({ format: "norn-project/1", name: "shop", sources: ["product.ts"], scope: { orgId: "local", environmentId: "dev" }, dev: { port: 4174, actor: "local:developer" }, cell: ".norn/cell.sqlite" }))
    expect((await run(["--check", "--json"])).status).toBe(1)
    expect(readFileSync(file, "utf8")).toBe(original)
    expect((await run(["--json"])).status).toBe(0)
    expect((await run([file, "--check"])).status).toBe(0)
  } finally { rmSync(root, { recursive: true }) }
})

test("fmt_keeps_unused_definition_imports", () => {
  const source = 'import data from "./definition.json"\nimport { Norn } from "@norn/authoring"\nexport const x = 1\n'
  const result = formatSource(resolve(TOOL_ROOT, "unused.ts"), source)
  expect(result).toContain('"./definition.json"')
  expect(result).toContain('"@norn/authoring"')
})
