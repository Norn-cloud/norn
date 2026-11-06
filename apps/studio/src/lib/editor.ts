/**
 * The builder bar, as types: every object a builder can create must say, for
 * every verb, either that it is supported or why it does not apply. A kind
 * that leaves a verb out does not compile.
 */
export const VERBS = ["create", "view", "edit", "delete", "duplicate", "reorder", "undo", "redo", "validate", "save", "reopen", "run"] as const
export type Verb = (typeof VERBS)[number]
export type VerbSupport = "supported" | { readonly na: string }
export type VerbMatrix = { readonly [V in Verb]: VerbSupport }

/** The verb matrix as a Markdown table, for PR bodies: ✓, or N/A with its reason. */
export function verbTable(kinds: Readonly<Record<string, VerbMatrix>>): string {
  const cell = (support: VerbSupport) => support === "supported" ? "✓" : `N/A: ${support.na}`
  return [
    `| Object | ${VERBS.join(" | ")} |`,
    `|---|${VERBS.map(() => "---").join("|")}|`,
    ...Object.entries(kinds).map(([kind, matrix]) => `| ${kind} | ${VERBS.map((verb) => cell(matrix[verb])).join(" | ")} |`),
  ].join("\n")
}

/** Undo and redo over whole editor states; at most `LIMIT` steps back. */
export interface History<T> {
  readonly past: readonly T[]
  readonly present: T
  readonly future: readonly T[]
}
const LIMIT = 100
export const history = {
  start: <T>(present: T): History<T> => ({ past: [], present, future: [] }),
  set: <T>(h: History<T>, next: T): History<T> => Object.is(next, h.present) ? h : { past: [...h.past, h.present].slice(-LIMIT), present: next, future: [] },
  undo: <T>(h: History<T>): History<T> => h.past.length === 0 ? h : { past: h.past.slice(0, -1), present: h.past[h.past.length - 1] as T, future: [h.present, ...h.future] },
  redo: <T>(h: History<T>): History<T> => h.future.length === 0 ? h : { past: [...h.past, h.present], present: h.future[0] as T, future: h.future.slice(1) },
}

/** The list verbs every ordered collection in a builder shares. */
export const list = {
  duplicate: <T>(items: readonly T[], index: number): readonly T[] => index < 0 || index >= items.length ? items : [...items.slice(0, index + 1), structuredClone(items[index] as T), ...items.slice(index + 1)],
  remove: <T>(items: readonly T[], index: number): readonly T[] => items.filter((_, at) => at !== index),
  move: <T>(items: readonly T[], from: number, to: number): readonly T[] => {
    if (from < 0 || from >= items.length || to < 0 || to >= items.length || from === to) return items
    const next = [...items]
    const [item] = next.splice(from, 1)
    next.splice(to, 0, item as T)
    return next
  },
}

/** One problem the server named, placed on the field it concerns when it can be placed. */
export interface Issue {
  readonly code: string
  readonly message: string
  /** A field id (`data-field`) in the editor, or `null` for the object as a whole. */
  readonly field: string | null
}

/** A shortcut's key as the reader's platform writes it. */
/**
 * Whether two editor values are the same: plain objects and arrays by their
 * fields, Maps and Sets by their entries, Dates by their time, and a cycle by
 * the pair it returns to, so no value is lost to JSON or makes it throw.
 */
export function sameValue(a: unknown, b: unknown, seen: Map<object, object> = new Map()): boolean {
  if (Object.is(a, b)) return true
  if (typeof a !== "object" || typeof b !== "object" || a === null || b === null) return false
  if (Object.getPrototypeOf(a) !== Object.getPrototypeOf(b)) return false
  if (seen.get(a) === b) return true
  seen.set(a, b)
  if (a instanceof Date) return a.getTime() === (b as Date).getTime()
  if (a instanceof Map) {
    const other = b as Map<unknown, unknown>
    return a.size === other.size && [...a].every(([key, value]) => other.has(key) && sameValue(value, other.get(key), seen))
  }
  if (a instanceof Set) {
    const other = b as Set<unknown>
    return a.size === other.size && [...a].every((value) => other.has(value))
  }
  const keys = Object.keys(a)
  return keys.length === Object.keys(b).length && keys.every((key) => Object.hasOwn(b, key) && sameValue((a as Record<string, unknown>)[key], (b as Record<string, unknown>)[key], seen))
}

export const modKey = (platform = globalThis.navigator?.platform ?? "") => /mac|iphone|ipad/i.test(platform) ? "⌘" : "Ctrl+"
