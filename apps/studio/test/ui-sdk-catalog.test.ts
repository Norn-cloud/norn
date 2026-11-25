import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { CATALOG_PATH, SYMBOLS_PATH, buildCatalog, pinnedVersion } from "../scripts/ui-sdk-catalog.ts"

// The drift gate (#1762): the Ring-1 checker's UI SDK catalog is exactly what the pinned
// `@ui-sdk/core` says about itself. A bump regenerates it (`bun scripts/ui-sdk-catalog.ts --write`)
// and the diff shows the bump lane every option, reason, behaviour and agent step that moved.
test("the UI SDK catalog regenerates byte for byte from the pinned @ui-sdk/core", async () => {
  const built = await buildCatalog()
  expect(built.catalog).toBe(readFileSync(CATALOG_PATH, "utf8"))
  expect(built.symbols).toBe(readFileSync(SYMBOLS_PATH, "utf8"))
  const catalog = JSON.parse(built.catalog) as { version: string; primitives: Record<string, { agent: { actions: string[] } }> }
  expect(catalog.version).toBe(pinnedVersion())
  expect(catalog.primitives["dialog"]?.agent.actions).toContain("setOpen")
})
