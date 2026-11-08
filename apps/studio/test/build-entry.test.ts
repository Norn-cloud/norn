import { expect, test } from "bun:test"
import config from "../vite.config.ts"

// The deploys' served-build checks look for assets/index-<hash>.js: the app's Rollup input must stay keyed `index`.
test("the app builds as the index entry, with the contact sheet beside it", () => {
  const input = (config as { build?: { rollupOptions?: { input?: Record<string, string> } } }).build?.rollupOptions?.input ?? {}
  expect(Object.keys(input).sort()).toEqual(["index", "sheet"])
  expect(input.index?.endsWith("/index.html")).toBe(true)
})
