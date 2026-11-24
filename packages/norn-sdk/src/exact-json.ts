/**
 * JSON text read with every number kept as its exact source token (a string), never a binary64
 * `number`: the cell's runtime values are exact decimals (`norn_ir::RuntimeValue`), and
 * `JSON.parse` rounds `9007199254740993` and drops the scale of `12.500`. Pure TypeScript, so it
 * reads the same in every engine. Strings are decoded by `JSON.parse` on their own token; objects
 * own every member (a `__proto__` key is a member, not a prototype). Anything that is not JSON
 * (RFC 8259) returns `undefined`.
 */

export type ExactJson = null | boolean | string | readonly ExactJson[] | { readonly [key: string]: ExactJson }

const NUMBER = /-?(?:0|[1-9][0-9]*)(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?/y
const STRING = /"(?:[^"\\\u0000-\u001f]|\\(?:["\\/bfnrt]|u[0-9a-fA-F]{4}))*"/y
const SPACE = /[ \t\n\r]*/y

export function parseExactJson(text: string): ExactJson | undefined {
  let at = 0
  const fail = (): never => { throw new SyntaxError(`not JSON at ${at}`) }
  const space = () => { SPACE.lastIndex = at; SPACE.exec(text); at = SPACE.lastIndex }
  const token = (pattern: RegExp): string | null => {
    pattern.lastIndex = at
    const match = pattern.exec(text)
    if (match === null) return null
    at = pattern.lastIndex
    return match[0]
  }
  const keyword = (word: string) => { if (text.startsWith(word, at)) { at += word.length; return true } return false }
  const member = (object: Record<string, ExactJson>, key: string, value: ExactJson) => {
    Object.defineProperty(object, key, { value, enumerable: true, writable: true, configurable: true })
  }
  function value(): ExactJson {
    space()
    const c = text[at]
    if (c === "{") {
      at++
      const object: Record<string, ExactJson> = {}
      space()
      if (text[at] === "}") { at++; return object }
      for (;;) {
        space()
        const key = token(STRING) ?? fail()
        space()
        if (text[at] !== ":") fail()
        at++
        member(object, JSON.parse(key) as string, value())
        space()
        if (text[at] === ",") { at++; continue }
        if (text[at] === "}") { at++; return object }
        return fail()
      }
    }
    if (c === "[") {
      at++
      const list: ExactJson[] = []
      space()
      if (text[at] === "]") { at++; return list }
      for (;;) {
        list.push(value())
        space()
        if (text[at] === ",") { at++; continue }
        if (text[at] === "]") { at++; return list }
        return fail()
      }
    }
    if (c === "\"") return JSON.parse(token(STRING) ?? fail()) as string
    if (keyword("true")) return true
    if (keyword("false")) return false
    if (keyword("null")) return null
    return token(NUMBER) ?? fail()
  }
  try {
    const result = value()
    space()
    return at === text.length ? result : undefined
  } catch (error) {
    // Too deep to read is no document either.
    if (error instanceof SyntaxError || error instanceof RangeError) return undefined
    throw error
  }
}
