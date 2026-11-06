/**
 * The data builder and the flow lowering run in a browser too (Studio's
 * canvas lowers what the user draws), so nothing they import may reach a
 * Node module.
 */
import { expect, test } from "bun:test"
import { readFileSync } from "node:fs"
import { dirname, join } from "node:path"

const IMPORT = /^\s*(?:import|export)\s[^"']*?from\s+["']([^"']+)["']/gm

function reached(entry: string, seen = new Set<string>()): Set<string> {
  if (seen.has(entry)) return seen
  seen.add(entry)
  for (const [, target = ""] of readFileSync(entry, "utf8").matchAll(IMPORT)) {
    if (target.startsWith(".")) reached(join(dirname(entry), target), seen)
    else seen.add(target)
  }
  return seen
}

test("the builder reaches no Node module", () => {
  const modules = [...reached(join(import.meta.dir, "../src/builder/index.ts"))]
  expect(modules.filter((module) => module.startsWith("node:") || !module.endsWith(".ts"))).toEqual([])
})
