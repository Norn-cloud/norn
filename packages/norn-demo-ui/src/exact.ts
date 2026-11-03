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

/** Structural JSON equality, independent of member order. */
export function same(left: unknown, right: unknown): boolean {
  if (left === right) return true
  if (typeof left !== "object" || typeof right !== "object" || left === null || right === null) return false
  if (Array.isArray(left) !== Array.isArray(right)) return false
  const leftKeys = Object.keys(left)
  const rightKeys = Object.keys(right)
  return leftKeys.length === rightKeys.length && leftKeys.every((key) =>
    Object.hasOwn(right, key) && same((left as Record<string, unknown>)[key], (right as Record<string, unknown>)[key]))
}

/**
 * The exact number texts of `source`, after checking that the whole source
 * (no trailing text) is the parsed `document`, field for field.
 */
export function exactSource(document: unknown, source: string): ReadonlyMap<string, string> {
  if (!same(JSON.parse(source), document)) throw new Error("Source and parsed document differ")
  return numberTexts(source)
}
