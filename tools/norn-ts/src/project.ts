import { existsSync, readFileSync } from "node:fs"
import { dirname, isAbsolute, resolve } from "node:path"
import type { Diagnostic } from "./check.ts"

export interface Project {
  readonly format: "norn-project/1"
  readonly name: string
  readonly sources: readonly string[]
  readonly scope: { readonly orgId: string; readonly environmentId: string }
  readonly dev: { readonly port: number; readonly actor: string }
  readonly cell: string
}

export class ProjectError extends Error {
  readonly diagnostic: Diagnostic
  constructor(readonly code: string, message: string, readonly exitCode: 1 | 2 | 3 = 1) {
    super(message)
    this.diagnostic = { code, message, construct: "project", start: { line: 1, column: 1 }, end: { line: 1, column: 1 } }
  }
}

function object(value: unknown, keys: readonly string[]): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    && Object.keys(value).length === keys.length && keys.every(key => key in value)
}
function text(value: unknown): value is string { return typeof value === "string" && value.trim().length > 0 }
/** A relative path that stays inside the project: not absolute, no `..` segment. */
function inside(value: unknown): value is string {
  return text(value) && !isAbsolute(value) && !/^[a-zA-Z]:/.test(value) && !value.split(/[\\/]/).includes("..")
}

export function parseProject(value: unknown): Project {
  if (object(value, ["format", "name", "sources", "scope", "dev", "cell"])
    && value.format === "norn-project/1" && text(value.name)
    && Array.isArray(value.sources) && value.sources.length > 0 && value.sources.every(inside)
    && new Set(value.sources).size === value.sources.length
    && object(value.scope, ["orgId", "environmentId"]) && text(value.scope.orgId) && text(value.scope.environmentId)
    && object(value.dev, ["port", "actor"]) && typeof value.dev.port === "number" && Number.isInteger(value.dev.port)
    && value.dev.port >= 0 && value.dev.port <= 65535 && text(value.dev.actor) && inside(value.cell)) {
    return { format: value.format, name: value.name, sources: value.sources,
      scope: { orgId: value.scope.orgId, environmentId: value.scope.environmentId },
      dev: { port: value.dev.port, actor: value.dev.actor }, cell: value.cell }
  }
  throw new ProjectError("cli.project.invalid_manifest", "Invalid norn.json; use norn-project/1 with explicit sources, scope, dev and cell, and remove unknown keys.")
}

export function readProject(start = process.cwd()): { root: string; manifest: Project } {
  let root = resolve(start)
  while (!existsSync(resolve(root, "norn.json"))) {
    const parent = dirname(root)
    if (parent === root) throw new ProjectError("cli.project.not_found", "No norn.json found; run norn init <dir> first.")
    root = parent
  }
  try { return { root, manifest: parseProject(JSON.parse(readFileSync(resolve(root, "norn.json"), "utf8"))) } }
  catch (error) {
    if (error instanceof ProjectError) throw error
    throw new ProjectError("cli.project.invalid_manifest", "Cannot read norn.json; repair its JSON and check file permissions.")
  }
}
