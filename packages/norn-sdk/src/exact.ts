/**
 * Exact decimals (#1271, D37): the cell sends a decimal as a JSON number token
 * with its full scale, which `JSON.parse` turns into a binary double. A reader
 * keeps the parsed document for its structure and reads every number from the
 * exact text the server sent, by RFC 6901 pointer.
 */
const malformed = (): never => { throw new Error("Source is not the JSON of one document") }

const space = (source: string, at: number): number => {
  while (at < source.length && " \t\n\r".includes(source[at] ?? "")) at += 1
  return at
}

function stringEnd(source: string, at: number): number {
  let index = at + 1
  while (source[index] !== '"') {
    if (index >= source.length) malformed()
    index += source[index] === "\\" ? 2 : 1
  }
  return index + 1
}

/** The RFC 6901 pointer token for one object member. */
export const member = (key: string): string => key.replaceAll("~", "~0").replaceAll("/", "~1")

/**
 * Every number in one JSON document, keyed by its RFC 6901 pointer, as the
 * exact text the server sent: decimals keep their scale (`100.000` stays
 * `100.000`), which a parsed JavaScript number cannot.
 */
export function numberTexts(source: string): ReadonlyMap<string, string> {
  const texts = new Map<string, string>()
  const value = (start: number, pointer: string): number => {
    const at = space(source, start)
    const first = source[at]
    if (first === '"') return stringEnd(source, at)
    if (first === "{" || first === "[") {
      const close = first === "{" ? "}" : "]"
      const seen = new Set<string>()
      let index = space(source, at + 1)
      for (let position = 0; source[index] !== close; position += 1) {
        let token = String(position)
        if (first === "{") {
          if (source[index] !== '"') malformed()
          const keyEnd = stringEnd(source, index)
          token = member(JSON.parse(source.slice(index, keyEnd)) as string)
          index = space(source, keyEnd)
          if (source[index] !== ":") malformed()
          index += 1
          // A repeated member replaces the earlier one, as `JSON.parse` keeps the last: forget its numbers.
          if (seen.has(token)) {
            const at = `${pointer}/${token}`
            for (const key of [...texts.keys()]) if (key === at || key.startsWith(`${at}/`)) texts.delete(key)
          }
          seen.add(token)
        }
        index = space(source, value(index, `${pointer}/${token}`))
        if (source[index] === ",") index = space(source, index + 1)
        else if (source[index] !== close) malformed()
      }
      return index + 1
    }
    let end = at
    while (end < source.length && !",}] \t\n\r".includes(source[end] ?? "")) end += 1
    if (end === at) malformed()
    const text = source.slice(at, end)
    if (text !== "true" && text !== "false" && text !== "null") texts.set(pointer, text)
    return end
  }
  value(0, "")
  return texts
}

/** A server document beside the exact text of every number in it. */
export interface Exact<T> {
  readonly document: T
  /** Every number's exact text, keyed by its RFC 6901 pointer (`/values/price`). */
  readonly numbers: ReadonlyMap<string, string>
}

/** Parse one JSON document, keeping the exact text of each of its numbers. */
export const readExact = <T,>(source: string): Exact<T> =>
  ({ document: JSON.parse(source) as T, numbers: numberTexts(source) })

/** The numbers under `prefix` (`/body`), re-keyed relative to it. */
export function numbersUnder(numbers: ReadonlyMap<string, string>, prefix: string): ReadonlyMap<string, string> {
  const under = new Map<string, string>()
  for (const [pointer, text] of numbers) {
    if (pointer === prefix) under.set("", text)
    else if (pointer.startsWith(`${prefix}/`)) under.set(pointer.slice(prefix.length), text)
  }
  return under
}
