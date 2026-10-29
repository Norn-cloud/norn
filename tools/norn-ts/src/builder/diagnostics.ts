/**
 * Builder diagnostics and source locations (T7-FLOW-1).
 *
 * Builder findings use the same stable `NORN-FLOW-*` codes as the profile
 * checker (`profile/norn-effect-1.json`), so a definition written with the
 * data builder and the same definition written in `Effect.gen` syntax fail
 * with the same code.
 *
 * Every builder node may carry `at`, the source location of the call that
 * made it. The syntax compiler sets it from the TypeScript span it lowered;
 * a hand-written builder call captures its own call site. Locations go only
 * into the non-executable source map, never into an artifact, so builder and
 * compiler output stay byte-identical.
 */
import { relative, sep } from "node:path"
import { TOOL_ROOT } from "../profile.ts"

export interface SourceLocation {
  /** Path relative to `tools/norn-ts`, `/`-separated. */
  readonly file: string
  /** 1-based. */
  readonly line: number
  /**
   * 1-based; set by the syntax compiler from the span it lowered. A captured
   * call site records the line only: stack columns differ between JS engines,
   * and goldens must not.
   */
  readonly column?: number
}

export interface BuildDiagnostic {
  readonly code: string
  readonly message: string
  readonly at?: SourceLocation
}

const BUILDER_DIR = `${sep}src${sep}builder${sep}`
const FRAME = /\(?((?:file:\/\/)?[^\s()]+?):(\d+):(\d+)\)?\s*$/

/**
 * The location of the first stack frame outside the builder modules: the
 * hand-written builder call. `undefined` when the runtime gives no usable
 * stack (the source map then records no location).
 */
export function callSite(): SourceLocation | undefined {
  const stack = new Error().stack?.split("\n").slice(1) ?? []
  for (const frame of stack) {
    const match = FRAME.exec(frame.trim())
    if (!match) continue
    const file = (match[1] ?? "").replace(/^file:\/\//, "")
    if (file.includes(BUILDER_DIR) || file.includes("/src/builder/")) continue
    if (!file.startsWith("/")) continue
    return { file: relative(TOOL_ROOT, file).split(sep).join("/"), line: Number(match[2]) }
  }
  return undefined
}

/** Collects diagnostics; lowering continues past a finding where it can. */
export class Diagnostics {
  readonly items: BuildDiagnostic[] = []
  private readonly seen = new Set<string>()

  /** Record a finding; the same finding reached along several paths is recorded once. */
  report(code: string, message: string, at: SourceLocation | undefined): void {
    const where = at ? `${at.file}:${at.line}:${at.column ?? 0}` : ""
    const key = `${code}\u0000${message}\u0000${where}`
    if (this.seen.has(key)) return
    this.seen.add(key)
    this.items.push(at ? { code, message, at } : { code, message })
  }

  get empty(): boolean {
    return this.items.length === 0
  }
}

/**
 * Attach a source location: the span the compiler lowered, or else the
 * hand-written call site.
 */
export function located<T extends object>(node: T, at: SourceLocation | undefined): T {
  const where = at ?? callSite()
  return where ? { ...node, at: where } : node
}
