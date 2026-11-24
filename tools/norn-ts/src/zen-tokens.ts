/**
 * The token stream of a ZEN expression, scanned as `vendor/zen/core/expression/src/lexer/lexer.rs`
 * scans it: the same character classes, the same string rule (a string ends at the next matching
 * quote; ZEN has no escapes), the same number, dot (`.`/`..`), `=`/`==`, `?`/`??`, `;` and template
 * string (`${…}`) handling, and the same refusals (an unmatched symbol or an unterminated string is
 * a `ZenLexError`). It does not parse; callers ask questions a token stream can answer exactly.
 */

export type ZenTokenKind = "identifier" | "literal" | "number" | "string" | "boolean" | "operator" | "bracket" | "template"
export interface ZenToken { readonly kind: ZenTokenKind; readonly value: string }

export class ZenLexError extends Error {
  constructor(message: string) {
    super(message)
    this.name = "ZenLexError"
  }
}

const SPACE = new Set([" ", "\n", "\t"])
const BRACKET = new Set(["(", ")", "[", "]", "{", "}"])
const CMP = new Set([">", "<", "!"])
const OPERATOR = new Set([",", ":", "+", "-", "/", "*", "^", "%"])
const isDigit = (c: string | undefined): boolean => c !== undefined && c >= "0" && c <= "9"
const isAlpha = (c: string | undefined): boolean => c !== undefined && (/[A-Za-z]/.test(c) || c === "$" || c === "_" || c === "#")
const isAlphanumeric = (c: string | undefined): boolean => isAlpha(c) || isDigit(c)
/** `Identifier` in the lexer's token set: the context and root references, the callback pointer, null. */
const IDENTIFIERS = new Set(["$", "$root", "#", "null"])

export function zenTokens(source: string): ZenToken[] {
  const chars = [...source]
  const tokens: ZenToken[] = []
  let at = 0
  const push = (kind: ZenTokenKind, value: string) => { tokens.push({ kind, value }) }

  function item(): void {
    const c = chars[at]!
    if (SPACE.has(c)) { at++; return }
    if (c === "'" || c === "\"") { string(c); return }
    if (isDigit(c)) { number(); return }
    if (BRACKET.has(c)) { push("bracket", c); at++; return }
    if (CMP.has(c)) { at++; const eq = chars[at] === "="; if (eq) at++; push("operator", eq ? `${c}=` : c); return }
    if (OPERATOR.has(c)) { push("operator", c); at++; return }
    if (c === "?") { at++; const nullish = chars[at] === "?"; if (nullish) at++; push("operator", nullish ? "??" : "?"); return }
    if (c === "=") { at++; const eq = chars[at] === "="; if (eq) at++; push("operator", eq ? "==" : "="); return }
    if (c === "`") { template(); return }
    if (c === ".") { at++; const range = chars[at] === "."; if (range) at++; push("operator", range ? ".." : "."); return }
    if (c === ";") { push("operator", ";"); at++; return }
    if (isAlpha(c)) { identifier(); return }
    throw new ZenLexError(`unmatched symbol \`${c}\` at ${at}`)
  }

  function string(quote: string): void {
    const start = ++at
    while (at < chars.length && chars[at] !== quote) at++
    if (at >= chars.length) throw new ZenLexError(`unterminated string at ${start - 1}`)
    push("string", chars.slice(start, at).join(""))
    at++
  }

  function number(): void {
    const start = at++
    let fractal = false
    while (isDigit(chars[at]) || chars[at] === "_" || chars[at] === ".") {
      if (chars[at] === ".") {
        if (fractal || chars[at + 1] === ".") break
        if (at + 1 < chars.length) fractal = true
      }
      at++
    }
    if (chars[at] === "e") {
      const e = at++
      if (chars[at] === "+" || chars[at] === "-") at++
      const digits = at
      while (isDigit(chars[at])) at++
      if (at === digits) at = e
    }
    push("number", chars.slice(start, at).join(""))
  }

  function template(): void {
    at++
    push("template", "`")
    let inExpression = false
    while (at < chars.length) {
      const c = chars[at]!
      if (c === "`") { at++; push("template", "`"); return }
      if (!inExpression) {
        if (c === "$" && chars[at + 1] === "{") { at += 2; inExpression = true; push("template", "${"); continue }
        at++
        continue
      }
      if (c === "}") { at++; inExpression = false; push("template", "}"); continue }
      item()
    }
    // An unterminated template string is closed at the end, as the lexer closes it.
    push("template", "`")
  }

  function identifier(): void {
    const start = at++
    while (isAlphanumeric(chars[at])) at++
    const value = chars.slice(start, at).join("")
    if (value === "and" || value === "or" || value === "in") push("operator", value)
    else if (value === "true" || value === "false") push("boolean", value)
    else if (value === "not") {
      if (chars.slice(at, at + 4).join("") === " in ") { at += 4; push("operator", "not in") } else push("operator", "not")
    } else push(IDENTIFIERS.has(value) ? "identifier" : "literal", value)
  }

  while (at < chars.length) item()
  return tokens
}

/** The closure builtins (`ClosureFunction`): their second argument is a closure over `#`. */
const CLOSURES = new Set(["all", "none", "some", "one", "filter", "map", "flatMap", "count"])

/**
 * Whether `norn_expr::expression_read_paths(source, "actor")` may be non-empty: a literal `actor`
 * path (an `actor` name that is not a member after `.`), or a node it cannot resolve to a path (the
 * root `$`/`$root`, the pointer `#`, a closure builtin call, an assignment or a `;` sequence). An
 * expression that does not lex counts too, as the kernel counts one that does not parse.
 *
 * Counting every `$` and every `#` is deliberately conservative. The parser
 * (`vendor/zen/core/expression/src/parser/parser.rs`) makes only `$root` the root node; a bare `$`
 * is an identifier there, and a `#` outside a closure is not the closure's pointer. A token stream
 * cannot tell those apart, so some expressions the kernel would let a caller fill are dropped here. That direction is safe: a
 * dropped field is never offered, while an offered actor field is one the kernel refuses.
 */
export function mayReadActor(source: string): boolean {
  let tokens: ZenToken[]
  try { tokens = zenTokens(source) } catch (error) {
    if (error instanceof ZenLexError) return true
    throw error
  }
  return tokens.some((token, index) => {
    const before = tokens[index - 1], after = tokens[index + 1]
    const member = before?.kind === "operator" && before.value === "."
    if (token.kind === "identifier") return token.value !== "null"
    if (token.kind === "operator") return token.value === "=" || token.value === ";"
    if (token.kind !== "literal" || member) return false
    if (token.value === "actor") return true
    return CLOSURES.has(token.value) && after?.kind === "bracket" && after.value === "("
  })
}
