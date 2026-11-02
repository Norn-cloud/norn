import { expect, test } from "bun:test"
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { compilePack } from "../src/pack.ts"
import { TOOL_ROOT } from "../src/profile.ts"

// The scripted authoring portion; dev/HTTP/subscriptions await P6/P7.
test("new_developer_init_author_and_repair", async () => {
  const root = mkdtempSync(join(tmpdir(), "norn-developer-"))
  try {
    const dir = join(root, "shop")
    const child = Bun.spawn([process.execPath, join(TOOL_ROOT, "src/cli.ts"), "init", dir, "--no-install", "--json"], { stdout: "pipe", stderr: "pipe" })
    await Promise.all([new Response(child.stdout).text(), new Response(child.stderr).text()])
    expect(await child.exited).toBe(0)
    const product = readFileSync(join(dir, "src/product.ts"), "utf8")
    const customer = product.replaceAll("Product", "Customer").replaceAll("product", "customer")
    const file = join(dir, "src/customer.ts")
    writeFileSync(file, customer)
    const compile = () => compilePack(join(dir, "src"), { scope: { orgId: "local", environmentId: "dev" }, root: dir })
    const good = compile()
    expect(good.ok).toBe(true)
    if (good.ok) expect(good.artifacts).toHaveLength(4)
    writeFileSync(file, customer.replace('properties: ["sku"]', 'properties: ["uuid"]'))
    expect(compile().ok).toBe(false)
    writeFileSync(file, customer)
    expect(compile()).toEqual(good)
  } finally { rmSync(root, { recursive: true }) }
}, 60000)
