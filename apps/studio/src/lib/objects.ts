import type { Field, Lifecycle } from "./catalog.ts"
import { member, type Read } from "./exact.ts"
import type { LinkPage, ObjectPage, ObjectRecord } from "./transport.ts"

/** A fact's value as the server sent it: decimals keep their exact text, with their declared currency. */
export function factText(field: Field, record: ObjectRecord, numbers: ReadonlyMap<string, string>, at: string): string {
  const value = record.values[field.id]
  if (value === null || value === undefined) return "—"
  if (field.decimal) {
    const text = numbers.get(`${at}/values/${member(field.id)}`) ?? (typeof value === "string" ? value : "unavailable")
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

/**
 * Order two exact decimal texts by value without a binary float, so
 * `9007199254740993.000000` sorts after `9007199254740992.000000`.
 * Missing values sort first.
 */
export function compareDecimals(a: string | undefined, b: string | undefined): number {
  if (a === undefined || b === undefined) return a === b ? 0 : a === undefined ? -1 : 1
  // Exponent forms are not decimal text the cell writes; order them by magnitude only.
  if (/e/i.test(a) || /e/i.test(b)) return Math.sign(Number(a) - Number(b))
  const parse = (text: string) => {
    const negative = text.startsWith("-")
    const [whole = "", fraction = ""] = text.replace(/^[-+]/, "").split(".")
    return { negative, whole: whole.replace(/^0+(?=\d)/, ""), fraction: fraction.replace(/0+$/, "") }
  }
  const x = parse(a)
  const y = parse(b)
  const zero = (d: typeof x) => /^0*$/.test(d.whole) && d.fraction === ""
  const xNegative = x.negative && !zero(x)
  const yNegative = y.negative && !zero(y)
  if (xNegative !== yNegative) return xNegative ? -1 : 1
  const magnitude = x.whole.length !== y.whole.length
    ? x.whole.length - y.whole.length
    : x.whole !== y.whole ? (x.whole < y.whole ? -1 : 1)
      : x.fraction.padEnd(y.fraction.length, "0") === y.fraction.padEnd(x.fraction.length, "0") ? 0
        : x.fraction.padEnd(y.fraction.length, "0") < y.fraction.padEnd(x.fraction.length, "0") ? -1 : 1
  return xNegative ? -magnitude : magnitude
}

/** One record from one page the server returned, with that page's exact number texts. */
export interface PageRow {
  readonly record: ObjectRecord
  readonly at: string
  readonly numbers: ReadonlyMap<string, string>
}

/** Every page read so far, and whether the server said the scan is complete (a null cursor). */
export interface Collected {
  readonly rows: readonly PageRow[]
  readonly held: number
  readonly complete: boolean
}

/** The cursor for the next server page; `undefined` once the server returns none. */
export const nextAfter = (page: Read<ObjectPage> | null): string | undefined => page?.document.scannedThrough ?? undefined

export function collect(pages: readonly (Read<ObjectPage> | null)[]): Collected {
  const read = pages.filter((page): page is Read<ObjectPage> => page !== null)
  const last = pages.at(-1)
  return {
    rows: read.flatMap((page) => page.document.records.map((record, index) => ({ record, at: `/records/${index}`, numbers: page.numbers }))),
    held: read.reduce((total, page) => total + page.document.quarantineCount, 0),
    complete: last !== undefined && last !== null && last.document.scannedThrough === null,
  }
}

/** The first failed read's message, exactly as the server or transport gave it; nothing reads as "loading" forever. */
export function readProblem(...reads: readonly { readonly error: Error | null }[]): string | undefined {
  return reads.find(({ error }) => error !== null)?.error?.message
}

/** Whether an object read and its link read are both about the object that was asked for. */
export const readsOf = (objectType: string, id: string, object: ObjectRecord, links: LinkPage): boolean =>
  object.objectType === objectType && object.id === id && links.objectType === objectType && links.id === id

/**
 * Whether to read the next server page automatically: only while the server
 * returned a cursor, nothing is in flight, the page bound is not reached and
 * the last continuation read did not fail. A failure waits for an explicit retry.
 */
export const shouldReadNext = (state: {
  readonly hasNextPage: boolean
  readonly isFetchingNextPage: boolean
  readonly isFetchNextPageError: boolean
  readonly pagesRead: number
}, maxPages: number): boolean =>
  state.hasNextPage && !state.isFetchingNextPage && !state.isFetchNextPageError && state.pagesRead < maxPages

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
