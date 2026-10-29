/**
 * Loader for the machine-readable `norn-effect/1` profile
 * (`profile/norn-effect-1.json`). The JSON is the single source of the
 * admitted/rejected symbol tables and the stable `NORN-FLOW-*` codes; the
 * checker and the corpus tests read it, nothing duplicates it in code.
 */
import { readFileSync } from "node:fs"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"

export const TOOL_ROOT = join(dirname(fileURLToPath(import.meta.url)), "..")
export const PROFILE_PATH = join(TOOL_ROOT, "profile", "norn-effect-1.json")
export const INTRINSICS_PATH = join(TOOL_ROOT, "profile", "norn-authoring.d.ts")

export type Use = "call" | "method" | "value" | "class-heritage"
export type Context = "flow" | "definition" | "retry-policy" | "any"

export interface AdmittedSymbol {
  readonly symbol: string
  readonly probe: string
  readonly row: string
  readonly use: Use
  readonly context: Context
  readonly note?: string
}

export interface RejectedSymbol {
  readonly symbol: string
  readonly probe: string
  readonly code: string
  readonly note?: string
}

export interface RejectedModule {
  readonly module: string
  readonly code: string
  readonly note?: string
}

export interface RejectedConstruct {
  readonly construct: string
  readonly code: string
  readonly fixtures: ReadonlyArray<string>
}

export interface Pin {
  readonly version: string
  readonly integrity: string
  readonly license: string
}

export interface Profile {
  readonly profile: string
  readonly pins: { readonly effect: Pin; readonly typescript: Pin }
  readonly intrinsicsModule: string
  /** Package specifiers a definition may import (`x/*` = any subpath); plus the intrinsics module and relative source modules. */
  readonly allowedImports: { readonly packages: ReadonlyArray<string> }
  readonly diagnostics: Readonly<Record<string, { readonly summary: string; readonly fix: string }>>
  readonly rows: Readonly<Record<string, string>>
  readonly admitted: ReadonlyArray<AdmittedSymbol>
  readonly rejected: ReadonlyArray<RejectedSymbol>
  readonly rejectedModules: ReadonlyArray<RejectedModule>
  readonly defaultCode: string
  readonly rejectedConstructs: ReadonlyArray<RejectedConstruct>
}

export class ProfileError extends Error {}

/** Load and structurally validate the profile: every referenced code and row must exist. */
export function loadProfile(path: string = PROFILE_PATH): Profile {
  const profile = JSON.parse(readFileSync(path, "utf8")) as Profile
  const codes = new Set(Object.keys(profile.diagnostics))
  const requireCode = (code: string, where: string) => {
    if (!codes.has(code)) throw new ProfileError(`${where}: unknown diagnostic code ${code}`)
  }
  const seen = new Set<string>()
  for (const a of profile.admitted) {
    if (!(a.row in profile.rows)) throw new ProfileError(`admitted ${a.symbol}: unknown row ${a.row}`)
    if (seen.has(a.symbol)) throw new ProfileError(`duplicate symbol ${a.symbol}`)
    seen.add(a.symbol)
  }
  for (const r of profile.rejected) {
    requireCode(r.code, `rejected ${r.symbol}`)
    if (seen.has(r.symbol)) throw new ProfileError(`symbol both admitted and rejected: ${r.symbol}`)
    seen.add(r.symbol)
  }
  for (const m of profile.rejectedModules) requireCode(m.code, `rejectedModules ${m.module}`)
  for (const c of profile.rejectedConstructs) requireCode(c.code, `rejectedConstructs ${c.construct}`)
  requireCode(profile.defaultCode, "defaultCode")
  return profile
}
