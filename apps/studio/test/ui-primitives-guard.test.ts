import { expect, test } from "bun:test"
import { readdirSync, readFileSync, statSync } from "node:fs"
import { join, relative } from "node:path"

/**
 * Hard-cutover CI guard: Studio uses @base-ui/react + @ui-sdk/core + @ui-sdk/react exclusively.
 * Any reintroduction of radix-ui, @radix-ui/* (direct or transitive in bun.lock), cmdk, or
 * --radix-* CSS custom properties anywhere across package.json, bun.lock, src/, test/, or e2e/ fails CI.
 */
const ROOT = join(import.meta.dir, "..")
const SELF = join(import.meta.dir, "ui-primitives-guard.test.ts")
const GENERATED = new Set(["checker", "engine"].map((dir) => join(ROOT, "src", dir)))
const SOURCE_FILES = /\.(tsx?|css|json|html)$/

const RADIX_PATTERNS: readonly { readonly name: string; readonly pattern: RegExp }[] = [
  { name: "radix-ui reference", pattern: /\bradix-ui\b/i },
  { name: "@radix-ui scoped package", pattern: /@radix-ui\//i },
  { name: "--radix- CSS custom property", pattern: /--radix-[a-z0-9-]+/i },
]

function walk(dir: string): string[] {
  return readdirSync(dir).flatMap((entry) => {
    const full = join(dir, entry)
    if (GENERATED.has(full) || full === SELF) return []
    if (statSync(full).isDirectory()) return walk(full)
    return SOURCE_FILES.test(entry) ? [full] : []
  })
}

test("apps/studio/src, test, and e2e contain zero radix-ui, @radix-ui/, or --radix-* references", () => {
  const dirs = [join(ROOT, "src"), join(ROOT, "test"), join(ROOT, "e2e")]
  const files = dirs.flatMap(walk)
  expect(files.length).toBeGreaterThan(100)
  const hits: string[] = []
  for (const file of files) {
    const text = readFileSync(file, "utf8")
    const lines = text.split("\n")
    for (let index = 0; index < lines.length; index += 1) {
      const line = lines[index]!
      for (const rule of RADIX_PATTERNS) {
        if (rule.pattern.test(line)) {
          hits.push(`${relative(ROOT, file)}:${index + 1}: ${rule.name}: ${line.trim()}`)
        }
      }
    }
  }
  expect(hits).toEqual([])
})

test("apps/studio/package.json and bun.lock contain zero direct or transitive Radix or cmdk dependencies", () => {
  const pkgText = readFileSync(join(ROOT, "package.json"), "utf8")
  const lockText = readFileSync(join(ROOT, "bun.lock"), "utf8")
  const pkg = JSON.parse(pkgText) as {
    dependencies?: Record<string, string>
    devDependencies?: Record<string, string>
  }
  const all = { ...pkg.dependencies, ...pkg.devDependencies }

  const forbiddenDeps = Object.keys(all).filter(
    (name) => name === "radix-ui" || name.startsWith("@radix-ui/") || name === "cmdk" || name === "@fontsource-variable/inter",
  )
  expect(forbiddenDeps).toEqual([])

  for (const rule of RADIX_PATTERNS) {
    expect(rule.pattern.test(pkgText), `package.json matched ${rule.name}`).toBe(false)
    expect(rule.pattern.test(lockText), `bun.lock matched ${rule.name}`).toBe(false)
  }
  expect(/"cmdk[@"]/.test(lockText), "bun.lock must not pull cmdk").toBe(false)

  expect(all["@base-ui/react"]).toBeDefined()
  expect(all["@ui-sdk/core"]).toBeDefined()
  expect(all["@ui-sdk/react"]).toBeDefined()
  expect(all["@fontsource-variable/geist"]).toBeDefined()
  expect(all["@fontsource-variable/geist-mono"]).toBeDefined()
})
