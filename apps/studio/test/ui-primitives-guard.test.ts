import { expect, test } from "bun:test"
import { readdirSync, readFileSync, statSync } from "node:fs"
import { join, relative } from "node:path"

/**
 * Hard-cutover CI guard: Studio uses @base-ui/react + @ui-sdk/core + @ui-sdk/react exclusively.
 * Any reintroduction of radix-ui, @radix-ui/* imports, or --radix-* CSS custom properties fails CI.
 */
const ROOT = join(import.meta.dir, "..")
const SRC = join(ROOT, "src")
const GENERATED = new Set(["checker", "engine"].map((dir) => join(SRC, dir)))
const SOURCE_FILES = /\.(tsx?|css|json|html)$/

const FORBIDDEN: readonly { readonly name: string; readonly pattern: RegExp }[] = [
  { name: "radix-ui package import", pattern: /["']radix-ui(?:\/[^"']*)?["']/ },
  { name: "@radix-ui scoped package import", pattern: /["']@radix-ui\/[^"']*["']/ },
  { name: "--radix- CSS custom property", pattern: /--radix-[a-z0-9-]+/i },
]

function walk(dir: string): string[] {
  return readdirSync(dir).flatMap((entry) => {
    const full = join(dir, entry)
    if (GENERATED.has(full)) return []
    if (statSync(full).isDirectory()) return walk(full)
    return SOURCE_FILES.test(entry) ? [full] : []
  })
}

test("apps/studio/src contains zero radix-ui imports or --radix-* CSS tokens", () => {
  const files = walk(SRC)
  expect(files.length).toBeGreaterThan(50)
  const hits: string[] = []
  for (const file of files) {
    const text = readFileSync(file, "utf8")
    const lines = text.split("\n")
    for (let index = 0; index < lines.length; index += 1) {
      const line = lines[index]!
      for (const rule of FORBIDDEN) {
        if (rule.pattern.test(line)) {
          hits.push(`${relative(ROOT, file)}:${index + 1}: ${rule.name}: ${line.trim()}`)
        }
      }
    }
  }
  expect(hits).toEqual([])
})

test("apps/studio/package.json declares Base UI, UI SDK, and Geist fonts with zero Radix or Inter", () => {
  const pkg = JSON.parse(readFileSync(join(ROOT, "package.json"), "utf8")) as {
    dependencies?: Record<string, string>
    devDependencies?: Record<string, string>
  }
  const all = { ...pkg.dependencies, ...pkg.devDependencies }
  const radixKeys = Object.keys(all).filter((name) => name === "radix-ui" || name.startsWith("@radix-ui/"))
  expect(radixKeys).toEqual([])
  expect(all["@fontsource-variable/inter"]).toBeUndefined()
  expect(all["@base-ui/react"]).toBeDefined()
  expect(all["@ui-sdk/core"]).toBeDefined()
  expect(all["@ui-sdk/react"]).toBeDefined()
  expect(all["@fontsource-variable/geist"]).toBeDefined()
  expect(all["@fontsource-variable/geist-mono"]).toBeDefined()
})
