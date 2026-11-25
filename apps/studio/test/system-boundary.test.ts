import { expect, test } from "bun:test"
import { readdirSync, readFileSync } from "node:fs"
import { join } from "node:path"
import ts from "typescript"

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

/**
 * Every module a file names, read by the compiler's own scanner rather than a pattern: static and
 * bare imports, type-only imports, re-exports, `import x = require()`, `require()` and `import()`.
 */
const specifiers = (text: string) => ts.preProcessFile(text, true, true).importedFiles.map(({ fileName }) => fileName)

test("the boundary reads every way a file can name a module", () => {
  const forms = [
    'import "@/lib/side-effect"',
    'import type { A } from "@/lib/a"',
    'export * from "../b"',
    'const c = require("@/lib/c")',
    'import d = require("@/lib/d")',
    'const e = await import("@/lib/e")',
    'import { f } from "./f"',
  ]
  expect(specifiers(forms.join("\n")).sort()).toEqual(["../b", "./f", "@/lib/a", "@/lib/c", "@/lib/d", "@/lib/e", "@/lib/side-effect"])
})

test("the design system imports only tokens, cn, Base UI, the UI SDK and its own files", () => {
  const files = readdirSync(SYSTEM).filter((name) => /\.tsx?$/.test(name))
  expect(files.length).toBeGreaterThan(0)
  const outside = files.flatMap((file) =>
    specifiers(readFileSync(join(SYSTEM, file), "utf8"))
      .filter((spec) => !ALLOWED.some((allowed) => allowed.test(spec)))
      .map((spec) => `${file}: ${spec}`))
  expect(outside).toEqual([])
})
