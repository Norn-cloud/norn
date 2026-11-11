import { readFileSync, readdirSync } from "node:fs"
import { join, resolve } from "node:path"
import ts from "typescript"
import { TOOL_ROOT } from "./profile.ts"

/** Only literal codes passed to the tool's diagnostic constructor count as emissions. */
export function emittedCliCodes(source: string): Set<string> {
  const codes = new Set<string>()
  const file = ts.createSourceFile("source.ts", source, ts.ScriptTarget.Latest, true)
  function visit(node: ts.Node): void {
    if (ts.isNewExpression(node) && ts.isIdentifier(node.expression) && node.expression.text === "ProjectError") {
      const code = node.arguments?.[0]
      if (code && ts.isStringLiteral(code) && code.text.startsWith("cli.")) codes.add(code.text)
    }
    ts.forEachChild(node, visit)
  }
  visit(file)
  return codes
}

export function toolEmissions(directory = join(TOOL_ROOT, "src")): Set<string> {
  const codes = new Set<string>()
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name)
    if (entry.isDirectory()) for (const code of toolEmissions(path)) codes.add(code)
    else if (entry.name.endsWith(".ts") && !entry.name.endsWith(".test.ts")) {
      for (const code of emittedCliCodes(readFileSync(path, "utf8"))) codes.add(code)
    }
  }
  return codes
}

export function missingCliEmissions(registered: readonly string[], emitted: ReadonlySet<string>): string[] {
  return registered.filter(code => code.startsWith("cli.") && !emitted.has(code))
}

if (import.meta.main) process.stdout.write(JSON.stringify([...toolEmissions()].sort()))
