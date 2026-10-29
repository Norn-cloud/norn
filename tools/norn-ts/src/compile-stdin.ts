/** Native MCP compiler adapter. Reads data, compiles syntax, never imports authored code. */
import { mkdtempSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import ts from "typescript"
import { canonicalJson } from "./canonical.ts"
import { compileDefinitions } from "./compiler/definitions.ts"
import { compileFile } from "./compiler/index.ts"

const MAX_BYTES = 1_048_576
export function compileModule(source: string, scope: Readonly<Record<string, string>>) {
  if (new TextEncoder().encode(source).length > MAX_BYTES) return { ok: false, diagnostics: [{code: "NORN-FLOW-UNBOUNDED", message: "module exceeds 1 MiB"}] }
  // Single-module MCP requests cannot read arbitrary host files via imports or references.
  const info = ts.preProcessFile(source, true, true)
  if (info.referencedFiles.length || info.typeReferenceDirectives.length || info.libReferenceDirectives.length || info.importedFiles.some((i) => i.fileName !== "@norn/authoring" && !/^effect(?:\/[A-Za-z0-9_-]+)*$/.test(i.fileName))) {
    return { ok: false, diagnostics: [{code: "NORN-FLOW-MODULE", start: {line: 1, column: 1}, message: "MCP modules import only effect and @norn/authoring; include local definitions in this module"}] }
  }
  const dir = mkdtempSync(join(tmpdir(), "norn-authoring-"))
  try {
    const file = join(dir, "module.ts")
    writeFileSync(file, source, { mode: 0o600 })
    const result = compileDefinitions(file, { scope, sourcePath: "module.ts" }, {}, { allowEmpty: true })
    if (!result.ok) return result
    const artifacts = [...result.artifacts]
    const warnings = [...result.warnings]
    if (result.flows > 0) {
      const flow = compileFile(file, {scope, sourcePath: "module.ts"})
      if (!flow.ok) return flow
      artifacts.push(...flow.artifacts)
      warnings.push(...flow.warnings)
    }
    if (!artifacts.length) return { ok: false, diagnostics: [{code: "NORN-FLOW-RESULT", start: {line: 1, column: 1}, message: "module declares no definitions"}] }
    return { ok: true, artifacts, sourceMap: result.sourceMap.map((s) => ({...s, at: {...s.at, file: "module.ts"}})), warnings }
  } finally { rmSync(dir, { recursive: true, force: true }) }
}

if (import.meta.main) {
  try {
    const bytes: Uint8Array[] = []
    let size = 0
    for await (const chunk of Bun.stdin.stream()) {
      size += chunk.byteLength
      if (size > MAX_BYTES * 2) throw new Error("compiler request exceeds 2 MiB")
      bytes.push(chunk)
    }
    const request = JSON.parse(Buffer.concat(bytes).toString("utf8")) as {source: string; scope: Record<string, string>}
    if (typeof request.source !== "string" || !request.scope || typeof request.scope !== "object") throw new Error("expected source and trusted scope")
    console.log(canonicalJson(compileModule(request.source, request.scope)))
  } catch (error) {
    console.log(JSON.stringify({ok: false, diagnostics: [{code: "NORN-FLOW-MODULE", message: error instanceof Error ? error.message : String(error)}]}))
  }
}
