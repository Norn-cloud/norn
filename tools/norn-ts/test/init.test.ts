import { expect, test } from "bun:test"
import { mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { compilePack, checkPack } from "../src/pack.ts"
import { TOOL_ROOT } from "../src/profile.ts"
import { parseProject, readProject } from "../src/project.ts"
import { projectDependencies } from "../src/init.ts"
import { checker } from "./checker.ts"

async function run(args: string[]) {
  const child = Bun.spawn([process.execPath, join(TOOL_ROOT, "src/cli.ts"), "init", ...args], { stdout: "pipe", stderr: "pipe" })
  const [out, err, status] = await Promise.all([new Response(child.stdout).text(), new Response(child.stderr).text(), child.exited])
  return { out, err, status }
}
function scratch() { return mkdtempSync(join(tmpdir(), "norn-init-")) }

test("init_writes_exact_tree", async () => {
  const root = scratch()
  try {
    const dir = join(root, "shop")
    const result = await run([dir, "--no-install", "--json"])
    expect(result.status).toBe(0)
    expect(JSON.parse(result.out).manifest).toEqual({ format: "norn-project/1", name: "shop", sources: ["src"], scope: { orgId: "local", environmentId: "dev" }, dev: { port: 4174, actor: "local:developer" }, cell: ".norn/cell.sqlite" })
    expect(readdirSync(dir, { recursive: true }).sort()).toEqual([".gitignore", "AGENTS.md", "norn.json", "package.json", "src", "src/product.ts", "tsconfig.json"].sort())
    expect(readFileSync(join(dir, ".gitignore"), "utf8")).toBe(".norn/\nnode_modules/\n")
    expect(readFileSync(join(dir, "src/product.ts"), "utf8")).toBe(readFileSync(join(TOOL_ROOT, "templates/starter/src/product.ts"), "utf8").replaceAll("__NAMESPACE__", "shop"))
    expect(readFileSync(join(dir, "AGENTS.md"), "utf8").split("\n")).toHaveLength(31)
    expect(JSON.parse(readFileSync(join(dir, "tsconfig.json"), "utf8"))).toEqual({ extends: "@norn/cli/tsconfig.authoring.json", include: ["src/**/*.ts"] })
    expect(JSON.parse(readFileSync(join(dir, "package.json"), "utf8"))).toEqual({ name: "shop", private: true, type: "module", dependencies: projectDependencies() })
    expect(readProject(join(dir, "src")).root).toBe(dir)
  } finally { rmSync(root, { recursive: true }) }
})

test("init_refuses_nonempty_dir", async () => {
  const root = scratch()
  try {
    writeFileSync(join(root, "precious"), "keep")
    const result = await run([root, "--no-install"])
    expect(result.status).toBe(1)
    expect(JSON.parse(result.out).code).toBe("cli.project.exists")
    expect(readdirSync(root)).toEqual(["precious"])
    expect(readFileSync(join(root, "precious"), "utf8")).toBe("keep")
  } finally { rmSync(root, { recursive: true }) }
})

test("init_invalid_namespace", async () => {
  const root = scratch()
  try {
    const result = await run([join(root, "shop"), "--namespace", "Bad.Name", "--no-install"])
    expect(result.status).toBe(1)
    expect(JSON.parse(result.out).code).toBe("cli.project.invalid_name")
    expect(readdirSync(root)).toEqual([])
  } finally { rmSync(root, { recursive: true }) }
})

test("starter_admits", async () => {
  const root = scratch()
  try {
    const dir = join(root, "shop")
    expect((await run([dir, "--no-install"])).status).toBe(0)
    const pack = compilePack(join(dir, "src"), { scope: { orgId: "local", environmentId: "dev" }, root: dir })
    expect(pack.ok).toBe(true)
    if (!pack.ok) throw new Error(JSON.stringify(pack))
    if (!checker) throw new Error("NORN_CHECKER_WASM_PKG is required")
    expect(pack.artifacts).toHaveLength(2)
    const result = checkPack(checker, pack.artifacts, pack.baseArtifacts)
    expect(result.admitted).toBe(true)
    expect(result.diagnostics.diagnostics).toEqual([])
  } finally { rmSync(root, { recursive: true }) }
}, 60000)

test("init_empty_and_explicit_scope", async () => {
  const root = scratch()
  try {
    const dir = join(root, "My-Shop")
    expect((await run([dir, "--template", "empty", "--org", "example", "--env", "local", "--no-install"])).status).toBe(0)
    expect(readdirSync(join(dir, "src"))).toEqual([])
    const { manifest } = readProject(dir)
    expect(manifest.scope).toEqual({ orgId: "example", environmentId: "local" })
    expect(() => parseProject({ ...manifest, surprise: true })).toThrow()
    expect(() => parseProject({ ...manifest, scope: { ...manifest.scope, surprise: true } })).toThrow()
  } finally { rmSync(root, { recursive: true }) }
})

test("published_mode_refuses", () => { expect(() => projectDependencies("published")).toThrow("Published distribution is unavailable") })

test("repository_install_uses_local_packages", async () => {
  const root = scratch()
  try {
    const dir = join(root, "shop")
    const result = await run([dir, "--json"])
    expect(result.status, result.err).toBe(0)
    expect(JSON.parse(result.out).created).toBe(dir)
    const pkg = JSON.parse(readFileSync(join(dir, "package.json"), "utf8"))
    expect(pkg.dependencies["@norn/authoring"]).toBe(`file:${TOOL_ROOT}`)
    expect(pkg.dependencies["@norn/cli"]).toBe(`file:${TOOL_ROOT}`)
    expect(readFileSync(join(dir, "node_modules/@norn/cli/tsconfig.authoring.json"), "utf8")).toContain('"strict": true')
  } finally { rmSync(root, { recursive: true }) }
}, 60000)

test("starter_is_formatted: a new project passes norn fmt --check", async () => {
  const root = scratch()
  try {
    const dir = join(root, "shop")
    expect((await run([dir, "--no-install"])).status).toBe(0)
    const child = Bun.spawn([process.execPath, join(TOOL_ROOT, "src/cli.ts"), "fmt", "--check", "--json"], { cwd: dir, stdout: "pipe", stderr: "pipe" })
    const [out, status] = await Promise.all([new Response(child.stdout).text(), child.exited])
    expect({ status, out: JSON.parse(out) }).toEqual({ status: 0, out: { changed: [], check: true } })
  } finally { rmSync(root, { recursive: true }) }
}, 60000)
