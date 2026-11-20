/**
 * A lifecycle guard in plain words: `self.amount <= 1000` reads "amount ≤ 1000". Only the
 * shapes a reader can follow at a glance are rewritten (comparisons of one field with a value,
 * joined by `and`); anything else returns null and the caller keeps its fallback. The words are
 * the server's declared guard restated, never a decision: the cell decides every call.
 */
const OPS: Readonly<Record<string, string>> = { "<=": "≤", ">=": "≥", "<": "<", ">": ">", "==": "is", "!=": "is not" }
const COMPARISON = /^\(?\s*self\.([A-Za-z_][A-Za-z0-9_]*)\s*(<=|>=|==|!=|<|>)\s*(-?\d+(?:\.\d+)?|'[^']*'|"[^"]*"|true|false|null)\s*\)?$/

function comparison(text: string): string | null {
  const found = COMPARISON.exec(text.trim())
  if (found === null) return null
  const [, field = "", op = "", value = ""] = found
  const shown = value.startsWith("'") || value.startsWith("\"") ? value.slice(1, -1) : value === "null" ? "empty" : value
  return `${field.replaceAll("_", " ")} ${OPS[op]} ${shown}`
}

/** One guard, or null when it is not a plain comparison (or a conjunction of them). */
export function guardWords(guard: string): string | null {
  const parts = guard.split(/\s+and\s+|\s*&&\s*/)
  const words = parts.map(comparison)
  return words.every((word): word is string => word !== null) ? words.join(" and ") : null
}

/** Every guard of a step in words, or null when one is not plain. */
export function guardsWords(guards: readonly string[]): string | null {
  const words = guards.map(guardWords)
  return guards.length > 0 && words.every((word): word is string => word !== null) ? words.join(" and ") : null
}

/** The guard a refusal quotes (`guard \`self.amount <= 1000\` of transition …`), in words. */
export function refusedGuardWords(message: string): string | null {
  const quoted = /guard `([^`]+)`/.exec(message)?.[1]
  return quoted === undefined ? null : guardWords(quoted)
}
