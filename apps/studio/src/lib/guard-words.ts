/**
 * A lifecycle guard in plain words: `self.amount <= 1000` reads "amount ≤ 1000". Only the
 * shapes a reader can follow at a glance are rewritten (comparisons of one field with a value,
 * joined by `and`); anything else returns null and the caller keeps its fallback. The words are
 * the server's declared guard restated, never a decision: the cell decides every call.
 */
const OPS: Readonly<Record<string, string>> = { "<=": "≤", ">=": "≥", "<": "<", ">": ">", "==": "is", "!=": "is not" }
// Grouping parentheses around a comparison (`(((self.a != '') and …`) carry no meaning in words.
const COMPARISON = /^\(*\s*self\.([A-Za-z_][A-Za-z0-9_]*)\s*(<=|>=|==|!=|<|>)\s*(-?\d+(?:\.\d+)?|'[^']*'|"[^"]*"|true|false|null)\s*\)*$/

type Clause = { readonly field: string; readonly op: string; readonly shown: string }

function comparison(text: string): Clause | null {
  const found = COMPARISON.exec(text.trim())
  if (found === null) return null
  const [, field = "", op = "", value = ""] = found
  const quoted = value.startsWith("'") || value.startsWith("\"") ? value.slice(1, -1) : undefined
  // An empty string or null is a value too: `self.candidate != ''` reads "candidate is not empty", never "candidate is not".
  const shown = quoted === "" || value === "null" ? "empty" : quoted ?? value
  return { field: field.replaceAll("_", " "), op: OPS[op] ?? op, shown }
}

/** Every clause of a guard, or null when one is not a plain comparison. */
function clauses(guard: string): Clause[] | null {
  const parts = guard.split(/\s+and\s+|\s*&&\s*/).map(comparison)
  return parts.every((part): part is Clause => part !== null) ? parts : null
}

/**
 * Clauses in words; neighbours that say the same of different fields share one clause:
 * "candidate and candidate digest are not empty", not the same words twice.
 */
function sentence(all: readonly Clause[]): string {
  const groups: { fields: string[]; op: string; shown: string }[] = []
  for (const { field, op, shown } of all) {
    const last = groups.at(-1)
    if (last && last.op === op && last.shown === shown) last.fields.push(field)
    else groups.push({ fields: [field], op, shown })
  }
  return groups.map(({ fields, op, shown }) => {
    const many = fields.length > 1
    const names = many ? `${fields.slice(0, -1).join(", ")} and ${fields.at(-1)}` : fields[0]
    const verb = many && op === "is" ? "are" : many && op === "is not" ? "are not" : op
    return `${names} ${verb} ${shown}`
  }).join(" and ")
}

/** One guard, or null when it is not a plain comparison (or a conjunction of them). */
export function guardWords(guard: string): string | null {
  const found = clauses(guard)
  return found === null ? null : sentence(found)
}

/** Every guard of a step in words, or null when one is not plain. */
export function guardsWords(guards: readonly string[]): string | null {
  const found = guards.map(clauses)
  return guards.length > 0 && found.every((each): each is Clause[] => each !== null) ? sentence(found.flat()) : null
}

/** The guard a refusal quotes (`guard \`self.amount <= 1000\` of transition …`), in words. */
export function refusedGuardWords(message: string): string | null {
  const quoted = /guard `([^`]+)`/.exec(message)?.[1]
  return quoted === undefined ? null : guardWords(quoted)
}
