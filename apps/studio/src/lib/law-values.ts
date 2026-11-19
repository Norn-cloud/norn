/**
 * DIAG-REASON-1 / LAW-LINK-MANY-1: the values a law compared, as the cell read them (a refusal's
 * `values`, a finding's `values`). A path is the law's own read: `total`, `ledger_entry.amount`, or
 * `lines.amount`, every line's amount, sent as a list. Nothing here evaluates the law: a list is
 * shown as the exact sum of the decimals the cell sent, beside its parts.
 */

const DECIMAL = /^(-?)(\d+)(?:\.(\d+))?$/

/** The exact decimal sum of `values`, at the widest scale among them; undefined if one is not a decimal. */
export function exactSum(values: readonly unknown[]): string | undefined {
  const parsed = values.map((value) => DECIMAL.exec(String(value)))
  if (parsed.some((match) => match === null)) return undefined
  const scale = Math.max(0, ...parsed.map((match) => match![3]?.length ?? 0))
  let total = 0n
  for (const match of parsed) {
    const [, sign, whole, fraction = ""] = match!
    const units = BigInt(`${whole}${fraction.padEnd(scale, "0")}`)
    total += sign === "-" ? -units : units
  }
  const negative = total < 0n
  const digits = (negative ? -total : total).toString().padStart(scale + 1, "0")
  const text = scale === 0 ? digits : `${digits.slice(0, -scale)}.${digits.slice(-scale)}`
  return negative ? `-${text}` : text
}

/** The member a list read reaches: `lines.amount` → `lines`. */
export const roleOf = (path: string): string => path.split(".")[0] ?? path

/** One compared value in words: a list is its sum (and its parts); a scalar is its text. */
export function compared(value: unknown): { readonly text: string; readonly parts?: readonly string[] } {
  if (!Array.isArray(value)) return { text: String(value) }
  const parts = value.map(String)
  return { text: exactSum(parts) ?? parts.join(" + "), parts }
}

/** Whether a path reads a list of values (every source of an inverse role). */
export const isList = (value: unknown): value is readonly unknown[] => Array.isArray(value)
