import { expect, test } from "bun:test"
import { cpSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs"
import { availableParallelism, tmpdir } from "node:os"
import { join, resolve } from "node:path"
import { formatSource } from "../src/fmt.ts"
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

test("fmt_preserves_semantics", async () => {
  // Every file fmt changes is compiled as written and as formatted, and the
  // two artifact sets must be equal. A file fmt leaves byte-identical compiles
  // to the same artifacts by determinism, so it needs no compile. Each compile
  // costs seconds, so they run in parallel processes, one per core: `before`
  // holds the sources as written, `after` every changed file formatted (so a
  // module is also compiled against its formatted imports).
  const before = mkdtempSync(join(tmpdir(), "norn-fmt-before-"))
  const after = mkdtempSync(join(tmpdir(), "norn-fmt-after-"))
  try {
    for (const root of [before, after]) {
      for (const part of ["fixtures", "corpus", "profile", "src"]) cpSync(join(TOOL_ROOT, part), join(root, part), { recursive: true })
    }
    const changed: string[] = []
    let unchanged = 0
    for (const file of [...sources(join(before, "corpus/accept")), ...sources(join(before, "fixtures"))]) {
      const source = readFileSync(file, "utf8")
      const formatted = formatSource(file, source)
      if (formatted === source) {
        unchanged++
        continue
      }
      const relativePath = file.slice(before.length + 1)
      changed.push(relativePath)
      writeFileSync(join(after, relativePath), formatted)
    }
    // One module's canonical artifact set, in its own process.
    const program = `
      import { toJson } from ${JSON.stringify(join(TOOL_ROOT, "src/canonical.ts"))}
      import { compilePack } from ${JSON.stringify(join(TOOL_ROOT, "src/pack.ts"))}
      const [root, file] = process.argv.slice(-2)
      const compiled = compilePack(root, { scope: { orgId: "org_demo", environmentId: "dev" }, modules: [file], root })
      const artifacts = compiled.ok ? compiled.artifacts.map(value => JSON.stringify(toJson(value))).sort() : []
      process.stdout.write(JSON.stringify({ ok: compiled.ok, artifacts }))`
    const compileIn = async (root: string, relativePath: string) => {
      const child = Bun.spawn(["bun", "-e", program, root, join(root, relativePath)], { stdout: "pipe", stderr: "pipe" })
      const [text, status] = await Promise.all([new Response(child.stdout).text(), child.exited])
      if (status !== 0) throw new Error(`${relativePath}: compile exited ${status}: ${await new Response(child.stderr).text()}`)
      return JSON.parse(text) as { ok: boolean; artifacts: string[] }
    }
    const jobs = changed.flatMap(path => [() => compileIn(before, path), () => compileIn(after, path)])
    const results: Array<{ ok: boolean; artifacts: string[] }> = new Array(jobs.length)
    let next = 0
    await Promise.all(Array.from({ length: Math.max(1, availableParallelism()) }, async () => {
      while (next < jobs.length) {
        const index = next++
        results[index] = await jobs[index]!()
      }
    }))
    let compiled = 0
    changed.forEach((path, index) => {
      const [was, now] = [results[2 * index]!, results[2 * index + 1]!]
      if (!was.ok) return // Rejection fixtures have no canonical artifact set.
      expect(now.ok, path).toBe(true)
      expect(now.artifacts, path).toEqual(was.artifacts)
      compiled++
    })
    expect(compiled + unchanged).toBeGreaterThan(20)
  } finally {
    rmSync(before, { recursive: true })
    rmSync(after, { recursive: true })
  }
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
