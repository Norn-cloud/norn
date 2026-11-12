import type { Field, Lifecycle } from "./catalog.ts"
import { member } from "./exact.ts"
import type { LinkPage, ObjectRecord } from "./transport.ts"

/**
 * An exact decimal's text written to its declared scale: the cell stores decimals canonically
 * (72.00 is kept as 72), so the declared places are put back by padding the fraction with zeros.
 * Digits are never rounded or dropped; text that is not a plain decimal is left as sent.
 */
export function atScale(text: string, scale: number | undefined): string {
  if (scale === undefined || scale <= 0 || !/^-?\d+(\.\d+)?$/.test(text)) return text
  const [whole, fraction = ""] = text.split(".")
  return fraction.length >= scale ? text : `${whole}.${fraction.padEnd(scale, "0")}`
}

/** A fact's value as the server sent it: decimals keep their exact text at their declared scale, with their declared currency. */
export function factText(field: Field, record: ObjectRecord, numbers: ReadonlyMap<string, string>, at: string): string {
  const value = record.values[field.id]
  if (value === null || value === undefined) return "—"
  if (field.decimal) {
    const text = atScale(numbers.get(`${at}/values/${member(field.id)}`) ?? (typeof value === "string" ? value : "unavailable"), field.decimal.scale)
    const currency = field.decimal.currencyFrom ? record.values[field.decimal.currencyFrom] : undefined
    const unit = field.decimal.unit ?? (typeof currency === "string" ? currency : undefined)
    const per = field.decimal.perUnit ? ` per ${field.decimal.perUnit}` : ""
    return unit ? `${text} ${unit}${per}` : `${text}${per}`
  }
  // Server values are shown as sent: codes and identifiers are never reworded, and nested numbers keep their exact text.
  return typeof value === "string" ? value : exactJson(value, numbers, `${at}/values/${member(field.id)}`)
}

/** The record's own name when its concept declares one, else its identity. */
export const recordName = (record: ObjectRecord): string => {
  const name = record.values["label"] ?? record.values["display_name"]
  return typeof name === "string" ? name : record.id
}

/** The first failed read's message, exactly as the server or transport gave it; nothing reads as "loading" forever. */
export function readProblem(...reads: readonly { readonly error: Error | null }[]): string | undefined {
  return reads.find(({ error }) => error !== null)?.error?.message
}

/** Whether an object read and its link read are both about the object that was asked for. */
export const readsOf = (objectType: string, id: string, object: ObjectRecord, links: LinkPage): boolean =>
  object.objectType === objectType && object.id === id && links.objectType === objectType && links.id === id

/** A lifecycle's states in the order the declared transitions first reach them from the initial states. */
export function lifecycleStates(lifecycle: Lifecycle): readonly string[] {
  const order = [...lifecycle.initial]
  for (let index = 0; index < order.length; index += 1) {
    for (const transition of lifecycle.transitions ?? []) {
      if (transition.from.includes(order[index] ?? "") && !order.includes(transition.to)) order.push(transition.to)
    }
  }
  return order
}

/** The record's position in a lifecycle and the steps the definition declares from there. */
export function lifecycleOf(lifecycle: Lifecycle, record: ObjectRecord) {
  const value = record.values[lifecycle.property]
  const state = typeof value === "string" ? value : undefined
  return {
    states: lifecycleStates(lifecycle),
    state,
    terminal: state !== undefined && (lifecycle.terminal ?? []).includes(state),
    next: state === undefined ? [] : (lifecycle.transitions ?? []).filter(({ from }) => from.includes(state)),
  }
}

/**
 * A parsed JSON value as text, with every number at or below `pointer` written
 * as the exact token the server sent (`numbers`, keyed by RFC 6901 pointer)
 * rather than as JavaScript re-serializes it.
 */
export function exactJson(value: unknown, numbers: ReadonlyMap<string, string>, pointer: string): string {
  if (Array.isArray(value)) return `[${value.map((item, index) => exactJson(item, numbers, `${pointer}/${index}`)).join(",")}]`
  if (typeof value === "number") return numbers.get(pointer) ?? String(value)
  if (value !== null && typeof value === "object") {
    return `{${Object.entries(value).map(([key, item]) => `${JSON.stringify(key)}:${exactJson(item, numbers, `${pointer}/${member(key)}`)}`).join(",")}}`
  }
  return JSON.stringify(value)
}

/** `exactJson`, indented two spaces a level, for reading. */
export function exactPretty(value: unknown, numbers: ReadonlyMap<string, string>, pointer: string, depth = 0): string {
  const pad = "  ".repeat(depth + 1)
  const end = "  ".repeat(depth)
  if (Array.isArray(value)) {
    return value.length === 0 ? "[]" : `[\n${value.map((item, index) => pad + exactPretty(item, numbers, `${pointer}/${index}`, depth + 1)).join(",\n")}\n${end}]`
  }
  if (typeof value === "number") return numbers.get(pointer) ?? String(value)
  if (value !== null && typeof value === "object") {
    const entries = Object.entries(value)
    return entries.length === 0 ? "{}" : `{\n${entries.map(([key, item]) => `${pad}${JSON.stringify(key)}: ${exactPretty(item, numbers, `${pointer}/${member(key)}`, depth + 1)}`).join(",\n")}\n${end}}`
  }
  return JSON.stringify(value)
}
