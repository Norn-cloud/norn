import { expect, test } from "bun:test"
import { celldConfig, celldStudio } from "../celld-config.ts"

const SECRET = "s".repeat(32)
const BUILD = "93dfdc450d".padEnd(40, "0")
const BASE = { STUDIO_STAGE: "demo", STUDIO_BUILD: BUILD, BETTER_AUTH_URL: "https://demo.norn.cloud", BETTER_AUTH_SECRET: SECRET }

test("a stage writes the Worker, assets, auth database and secrets as celld reads them", () => {
  expect(celldConfig(celldStudio(BASE))).toEqual({
    name: "norn-studio-demo",
    main: "worker/index.ts",
    compatibility_date: "2026-11-01",
    compatibility_flags: ["nodejs_compat"],
    assets: { directory: "dist", binding: "ASSETS", not_found_handling: "single-page-application", run_worker_first: ["/api/*"] },
    d1_databases: [{ binding: "AUTH_DB", database_name: "norn-studio-auth-demo", migrations_dir: "worker/migrations" }],
    vars: { BETTER_AUTH_SECRET: SECRET, BETTER_AUTH_URL: "https://demo.norn.cloud", AUTH_SCRYPT: "js", STUDIO_BUILD: BUILD },
  })
})

test("the cell host and the assistant bind only when configured", () => {
  const config = celldConfig(celldStudio({ ...BASE, STUDIO_DEFAULT_ORG: "org_demo", NORN_HOST_SERVICE: "norn-cell", AI_GATEWAY_URL: "https://gw", AI_GATEWAY_TOKEN: "t" }))
  expect(config.services).toEqual([{ binding: "NORN_HOST", service: "norn-cell" }])
  expect(config.durable_objects).toEqual({ bindings: [{ name: "AGENT_BUDGET", class_name: "AgentBudget" }] })
  expect(config.migrations).toEqual([{ tag: "v1", new_sqlite_classes: ["AgentBudget"] }])
  expect(config.vars).toEqual({ BETTER_AUTH_SECRET: SECRET, BETTER_AUTH_URL: "https://demo.norn.cloud", AUTH_SCRYPT: "js", STUDIO_BUILD: BUILD, STUDIO_DEFAULT_ORG: "org_demo", AI_GATEWAY_URL: "https://gw", AI_GATEWAY_TOKEN: "t" })
})

test("production, weak secrets, email and half an assistant refuse before anything is written", () => {
  expect(() => celldStudio({ ...BASE, STUDIO_STAGE: "prod" })).toThrow("stays on Cloudflare")
  expect(() => celldStudio({ ...BASE, STUDIO_STAGE: "Demo" })).toThrow("STUDIO_STAGE")
  expect(() => celldStudio({ ...BASE, STUDIO_BUILD: undefined })).toThrow("STUDIO_BUILD")
  expect(() => celldStudio({ ...BASE, STUDIO_BUILD: "93dfdc450d" })).toThrow("full commit")
  expect(() => celldStudio({ ...BASE, BETTER_AUTH_URL: "https://demo.norn.cloud/x" })).toThrow("https origin")
  expect(() => celldStudio({ ...BASE, BETTER_AUTH_SECRET: "short" })).toThrow("32 characters")
  expect(() => celldStudio({ ...BASE, EMAIL_FROM: "a@b.c" })).toThrow("Email Service")
  expect(() => celldStudio({ ...BASE, AI_GATEWAY_URL: "https://gw" })).toThrow("together")
  expect(() => celldStudio({ ...BASE, NORN_HOST_SERVICE: "Norn_Cell" })).toThrow("script name")
})

test("the tool gateway reaches the Worker only whole, as an https origin with Studio's client", () => {
  const tool = { TOOL_GATEWAY_URL: "https://gateway-next.norn.cloud", TOOL_GATEWAY_CLIENT_ID: "client", TOOL_GATEWAY_CLIENT_SECRET: "c".repeat(32) }
  expect(celldConfig(celldStudio({ ...BASE, ...tool })).vars).toEqual({ BETTER_AUTH_SECRET: SECRET, BETTER_AUTH_URL: "https://demo.norn.cloud", AUTH_SCRYPT: "js", STUDIO_BUILD: BUILD, ...tool })
  expect(celldConfig(celldStudio(BASE)).vars).not.toHaveProperty("TOOL_GATEWAY_URL")
  expect(() => celldStudio({ ...BASE, TOOL_GATEWAY_URL: tool.TOOL_GATEWAY_URL })).toThrow("go together")
  expect(() => celldStudio({ ...BASE, ...tool, TOOL_GATEWAY_CLIENT_ID: "" })).toThrow("non-empty")
  expect(() => celldStudio({ ...BASE, ...tool, TOOL_GATEWAY_CLIENT_SECRET: "" })).toThrow("non-empty")
  expect(() => celldStudio({ ...BASE, ...tool, TOOL_GATEWAY_URL: "http://127.0.0.1:9878" })).toThrow("https origin")
})

test("the secret-bearing config is created exclusively, readable by its owner only", async () => {
  const { mkdtempSync, statSync, writeFileSync, readFileSync, chmodSync } = await import("node:fs")
  const { join } = await import("node:path")
  const { tmpdir } = await import("node:os")
  const { writeSecretConfig } = await import("../celld-config.ts")
  const dir = mkdtempSync(join(tmpdir(), "celld-config-"))
  const path = join(dir, "wrangler.json")
  writeSecretConfig(path, { vars: { BETTER_AUTH_SECRET: SECRET } })
  expect(statSync(path).mode & 0o777).toBe(0o600)
  expect(JSON.parse(readFileSync(path, "utf8")).vars.BETTER_AUTH_SECRET).toBe(SECRET)
  // A stale file is never reused, whatever its permissions; its mode and contents stay as they were.
  chmodSync(path, 0o644)
  writeFileSync(path, "{}")
  expect(() => writeSecretConfig(path, { vars: { BETTER_AUTH_SECRET: SECRET } })).toThrow("already exists")
  expect(statSync(path).mode & 0o777).toBe(0o644)
  expect(readFileSync(path, "utf8")).toBe("{}")
})
