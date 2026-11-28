import { readFileSync, readdirSync } from "node:fs"
import { join } from "node:path"
import { TOOL_ROOT } from "./profile.ts"

type Token =
  | { readonly kind: "ident"; readonly value: string }
  | { readonly kind: "string"; readonly value: string }
  | { readonly kind: "punct"; readonly value: string }

function scanTokens(source: string): Token[] {
  const tokens: Token[] = []
  let i = 0
  const len = source.length
  while (i < len) {
    const ch = source[i]!
    // Whitespace
    if (/\s/.test(ch)) {
      i++
      continue
    }
    // Line comment
    if (ch === "/" && source[i + 1] === "/") {
      i += 2
      while (i < len && source[i] !== "\n") i++
      continue
    }
    // Block comment
    if (ch === "/" && source[i + 1] === "*") {
      i += 2
      while (i < len && !(source[i] === "*" && source[i + 1] === "/")) i++
      i += 2
      continue
    }
    // Single- or double-quoted string literal
    if (ch === '"' || ch === "'") {
      const quote = ch
      i++
      let value = ""
      while (i < len && source[i] !== quote) {
        if (source[i] === "\\" && i + 1 < len) {
          value += source[i + 1]!
          i += 2
        } else {
          value += source[i]!
          i++
        }
      }
      if (i < len) i++
      tokens.push({ kind: "string", value })
      continue
    }
    // Template literal (skip content including simple escapes; expressions are tokenized separately only if needed)
    if (ch === "`") {
      i++
      let depth = 0
      while (i < len) {
        if (source[i] === "\\" && i + 1 < len) {
          i += 2
        } else if (source[i] === "$" && source[i + 1] === "{") {
          depth++
          i += 2
        } else if (source[i] === "}" && depth > 0) {
          depth--
          i++
        } else if (source[i] === "`" && depth === 0) {
          i++
          break
        } else {
          i++
        }
      }
      continue
    }
    // Identifier
    if (/[A-Za-z_$]/.test(ch)) {
      let end = i + 1
      while (end < len && /[A-Za-z0-9_$]/.test(source[end]!)) end++
      tokens.push({ kind: "ident", value: source.slice(i, end) })
      i = end
      continue
    }
    tokens.push({ kind: "punct", value: ch })
    i++
  }
  return tokens
}

/** Only literal codes passed to the tool's diagnostic constructor count as emissions. */
export function emittedCliCodes(source: string): Set<string> {
  const codes = new Set<string>()
  const tokens = scanTokens(source)
  for (let i = 0; i + 3 < tokens.length; i++) {
    const a = tokens[i]!
    const b = tokens[i + 1]!
    const c = tokens[i + 2]!
    const d = tokens[i + 3]!
    if (
      a.kind === "ident" && a.value === "new" &&
      b.kind === "ident" && b.value === "ProjectError" &&
      c.kind === "punct" && c.value === "(" &&
      d.kind === "string" && d.value.startsWith("cli.")
    ) {
      codes.add(d.value)
    }
  }
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
