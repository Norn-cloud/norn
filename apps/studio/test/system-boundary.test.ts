import { expect, test } from "bun:test"
import { readdirSync, readFileSync } from "node:fs"
import { join } from "node:path"

/**
 * `src/components/system/` is the design system ready to move into a shared `@norn/ui` package
 * (#1762): tokens, `cn`, Base UI, the UI SDK and props only. Nothing in it may import app code
 * (routes, transport, queries, `lib/` logic), so data always arrives through props and the server
 * stays the authority.
 */
const SYSTEM = join(import.meta.dir, "../src/components/system")
const ALLOWED = [
  /^react$/, /^react\/jsx-runtime$/, /^lucide-react$/, /^class-variance-authority$/,
  /^@base-ui\/react(\/[a-z-]+)*$/, /^@ui-sdk\/react(\/[a-z-]+)*$/,
  /^\.\/[a-z-]+$/, /^@\/lib\/utils$/,
]

test("the design system imports only tokens, cn, Base UI, the UI SDK and its own files", () => {
  const files = readdirSync(SYSTEM).filter((name) => /\.tsx?$/.test(name))
  expect(files.length).toBeGreaterThan(0)
  const outside = files.flatMap((file) => {
    const text = readFileSync(join(SYSTEM, file), "utf8")
    return [...text.matchAll(/(?:^|\n)\s*(?:import|export)\b[^"'`]*?from\s+["']([^"']+)["']|import\(\s*["']([^"']+)["']\s*\)/g)]
      .map((match) => match[1] ?? match[2]!)
      .filter((spec) => !ALLOWED.some((allowed) => allowed.test(spec)))
      .map((spec) => `${file}: ${spec}`)
  })
  expect(outside).toEqual([])
})
