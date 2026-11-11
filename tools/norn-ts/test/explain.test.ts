/**
 * #517 `norn explain` in a project: init → explain → break → explain names the
 * source line → repair. The report is the checker ABI's `norn/explain@2`, the
 * same one the native `norn explain --defs` prints.
 */
import { expect, test } from "bun:test"
import { spawnSync } from "node:child_process"
import { mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { CHECKER_PKG_ENV } from "../src/checker-abi.ts"
import { TOOL_ROOT } from "../src/profile.ts"

const CLI = join(TOOL_ROOT, "src/cli.ts")
if (process.env.CI && !process.env[CHECKER_PKG_ENV]) throw new Error(`explain tests require ${CHECKER_PKG_ENV}`)

function norn(cwd: string, args: string[], env: Record<string, string | undefined> = process.env) {
  const run = spawnSync(process.execPath, [CLI, ...args], { cwd, env, encoding: "utf8" })
  return { status: run.status, out: run.stdout, err: run.stderr }
}

const customer = (identity: string) => `import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Customer = Norn.type({
  name: "shop.Customer", title: "Customer", version: "0.1.0",
  properties: Schema.Struct({ email: Schema.String, name: Schema.String }),
  identity: [{ name: "primary", properties: ["${identity}"] }],
})
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

    writeFileSync(join(dir, "src/customer.ts"), customer("uuid"))
    const refused = norn(dir, ["explain", "action", "shop.product.add", "--json"])
    expect(refused.status).toBe(1)
    const response = JSON.parse(refused.out)
    expect(response).toMatchObject({ format: "norn/explain@2", admitted: false, report: null })
    expect(response.diagnostics[0].source).toMatchObject({ file: "src/customer.ts", line: 7 })
    const frame = norn(dir, ["explain", "action", "shop.product.add"])
    expect(frame.out).toContain("src/customer.ts:7")
    expect(frame.out).toContain(`identity: [{ name: "primary", properties: ["uuid"] }],`)

    writeFileSync(join(dir, "src/customer.ts"), customer("email"))
    expect(norn(dir, ["explain", "concept", "shop.Customer", "--json"]).status).toBe(0)
  } finally { rmSync(root, { recursive: true, force: true }) }
}, 120_000)

test("explain refuses with an actionable code outside a project, on bad usage and without the checker", () => {
  const root = mkdtempSync(join(tmpdir(), "norn-explain-"))
  try {
    expect(JSON.parse(norn(root, ["explain", "action", "x"]).out).code).toBe("cli.project.not_found")
    expect(norn(root, ["explain", "widget", "x"])).toMatchObject({ status: 2 })
    expect(norn(root, ["explain", "law", "x", "--locale", "ar"])).toMatchObject({ status: 2 })
    expect(norn(root, ["init", join(root, "p"), "--no-install"]).status).toBe(0)
    const missing = norn(join(root, "p"), ["explain", "action", "p.product.add"], { ...process.env, [CHECKER_PKG_ENV]: "" })
    expect(missing.status).toBe(3)
    expect(JSON.parse(missing.out).code).toBe("cli.setup.checker_unavailable")
  } finally { rmSync(root, { recursive: true, force: true }) }
})
