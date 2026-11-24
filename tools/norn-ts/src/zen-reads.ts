/**
 * Whether a ZEN expression may read the verified actor, decided as the kernel decides it
 * (`norn_kernel::actor_derived_fields` → `norn_expr::expression_read_paths(source, "actor")`):
 * the expression is lexed and parsed as the vendored ZEN lexer and parser do
 * (`vendor/zen/core/expression/src/lexer/lexer.rs`, `parser/parser.rs`, `parser/standard.rs`), and
 * the AST is walked as `collect_paths` walks it. An expression that does not lex or parse counts
 * as a read, as it does for the kernel.
 *
 * The scanner keeps the lexer's rules: its character classes, its string rule (a string ends at the
 * next matching quote; ZEN has no escapes), its number, dot (`.`/`..`), `=`/`==`, `?`/`??`, `;` and
 * template string (`${…}`) handling, and its refusals (an unmatched symbol or an unterminated string).
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


/** `FunctionKind`: the internal, deprecated and closure builtins the parser recognises by name. */
const INTERNAL = new Set([
  "len", "contains", "flatten", "merge", "mergeDeep", "upper", "lower", "trim", "startsWith", "endsWith", "matches",
  "extract", "fuzzyMatch", "split", "tokens", "take", "join", "abs", "sum", "avg", "min", "max", "rand", "median", "mode",
  "floor", "ceil", "round", "trunc", "isNumeric", "string", "number", "bool", "type", "keys", "values", "d", "date",
  "timestamp", "days_between", "add_days", "seconds_between", "add_seconds", "intervals_overlap", "interval_gap_seconds",
  "interval_intersection_seconds", "resolve_local_timestamp",
])
const DEPRECATED = new Set([
  "date", "time", "duration", "year", "dayOfWeek", "dayOfMonth", "dayOfYear", "weekOfYear", "monthOfYear", "monthString",
  "dateString", "weekdayString", "startOf", "endOf",
])
const CLOSURES = new Set(["all", "none", "some", "one", "filter", "map", "flatMap", "count"])
/** `MethodKind`: the date methods, the only methods the parser admits after `.`. */
const METHODS = new Set([
  "add", "sub", "set", "format", "startOf", "endOf", "diff", "tz", "isSame", "isBefore", "isAfter", "isSameOrBefore",
  "isSameOrAfter", "second", "minute", "hour", "day", "dayOfYear", "week", "weekday", "month", "quarter", "year",
  "timestamp", "offsetName", "isValid", "isYesterday", "isToday", "isTomorrow", "isLeapYear",
])
/** `BINARY_OPERATORS` (`parser/constants.rs`): precedence, and whether the operator is right-associative. */
const BINARY: ReadonlyMap<string, readonly [number, boolean]> = new Map<string, readonly [number, boolean]>([
  ["or", [10, false]], ["and", [15, false]],
  ...["==", "!=", "<", ">", "<=", ">=", "not in", "in"].map((op): [string, readonly [number, boolean]] => [op, [20, false]]),
  ["+", [30, false]], ["-", [30, false]], ["*", [60, false]], ["/", [60, false]], ["%", [60, false]], ["^", [70, true]],
  ["??", [80, false]],
])
/** `UNARY_OPERATORS`: `not` (also spelled `!`) and the signs. */
const UNARY: ReadonlyMap<string, number> = new Map([["not", 50], ["!", 50], ["+", 200], ["-", 200]])

/** The AST, as far as `collect_paths` distinguishes it. */
type ZenNode =
  | { readonly k: "leaf" }
  | { readonly k: "identifier"; readonly name: string }
  | { readonly k: "string"; readonly value: string }
  | { readonly k: "root" | "pointer" | "closure" | "assignments" }
  | { readonly k: "parenthesized" | "unary"; readonly inner: ZenNode }
  | { readonly k: "member"; readonly node: ZenNode; readonly property: ZenNode }
  | { readonly k: "list"; readonly parts: readonly ZenNode[] }

class ZenParseError extends Error {}

class Parser {
  #at = 0
  #depth = 0
  readonly #hasRange: boolean
  constructor(readonly tokens: readonly ZenToken[]) {
    this.#hasRange = tokens.some((t) => t.kind === "operator" && t.value === "..")
  }

  get #current(): ZenToken | undefined { return this.tokens[this.#at] }
  #is(kind: ZenTokenKind, value?: string): boolean {
    const t = this.#current
    return t !== undefined && t.kind === kind && (value === undefined || t.value === value)
  }
  #expect(kind: ZenTokenKind, value: string): void {
    if (!this.#is(kind, value)) throw new ZenParseError(`expected ${value}`)
    this.#at++
  }

  /** `Standard::parse`: one expression that consumes every token. */
  parse(): ZenNode {
    const root = this.#binary(0)
    if (this.#at < this.tokens.length) throw new ZenParseError("trailing tokens")
    return root
  }

  #binary(precedence: number): ZenNode {
    let left = this.#unary()
    for (;;) {
      const t = this.#current
      if (t === undefined || t.kind !== "operator") break
      const op = BINARY.get(t.value)
      if (op === undefined || op[0] < precedence) break
      this.#at++
      const right = this.#binary(op[1] ? op[0] : op[0] + 1)
      left = { k: "list", parts: [left, right] }
    }
    if (precedence === 0 && this.#is("operator", "?")) {
      this.#at++
      const onTrue = this.#binary(0)
      this.#expect("operator", ":")
      const onFalse = this.#binary(0)
      left = { k: "list", parts: [left, onTrue, onFalse] }
    }
    return left
  }

  #unary(): ZenNode {
    const t = this.#current
    if (t === undefined) throw new ZenParseError("unexpected end")
    if (this.#depth > 0 && t.kind === "identifier" && t.value === "#") {
      this.#at++
      return this.#postfix({ k: "pointer" })
    }
    if (t.kind === "operator") {
      const precedence = UNARY.get(t.value)
      if (precedence === undefined) throw new ZenParseError(`not a unary operator: ${t.value}`)
      this.#at++
      return { k: "unary", inner: this.#binary(precedence) }
    }
    const interval = this.#interval()
    if (interval !== null) return interval
    if (t.kind === "bracket" && t.value === "(") {
      this.#at++
      const inner = this.#binary(0)
      this.#expect("bracket", ")")
      return this.#postfix({ k: "parenthesized", inner })
    }
    return this.#literal()
  }

  /** `interval`: `[a..b]`, `(a..b)`, `]a..b[` and the like; any failure restores the position. */
  #interval(): ZenNode | null {
    if (!this.#hasRange || !this.#is("bracket")) return null
    const start = this.#at
    try {
      this.#at++
      const left = this.#binary(0)
      this.#expect("operator", "..")
      const right = this.#binary(0)
      if (!this.#is("bracket")) throw new ZenParseError("interval end")
      this.#at++
      return this.#postfix({ k: "list", parts: [left, right] })
    } catch (error) {
      if (!(error instanceof ZenParseError)) throw error
      this.#at = start
      return null
    }
  }

  #literal(): ZenNode {
    const t = this.#current
    if (t === undefined) throw new ZenParseError("expected a literal")
    switch (t.kind) {
      case "identifier":
        if (t.value === "null") { this.#at++; return { k: "leaf" } }
        return this.#identifier()
      case "literal": return this.#identifier()
      case "boolean": case "number": this.#at++; return { k: "leaf" }
      case "string": this.#at++; return { k: "string", value: t.value }
      case "template": if (t.value === "`") return this.#template(); throw new ZenParseError("template token")
      case "bracket":
        if (t.value === "(" || t.value === ")" || t.value === "]") {
          const interval = this.#interval()
          if (interval === null) throw new ZenParseError("invalid syntax")
          return interval
        }
        if (t.value === "[") return this.#interval() ?? this.#array()
        if (t.value === "{") return this.#object()
        throw new ZenParseError("unexpected }")
      default: throw new ZenParseError(`unexpected ${t.kind} ${t.value}`)
    }
  }

  #identifier(): ZenNode {
    const name = this.#current!.value
    this.#at++
    if (!this.#is("bracket", "(")) {
      const node = this.#postfix(name === "$root" ? { k: "root" } : { k: "identifier", name })
      // `assigned_object`: an `x = …; …` sequence is `Node::Assignments`, which `collect_paths`
      // counts as a read whatever it assigns.
      if (this.#is("operator", "=")) return { k: "assignments" }
      return node
    }
    if (!INTERNAL.has(name) && !DEPRECATED.has(name) && !CLOSURES.has(name)) throw new ZenParseError(`unknown builtin ${name}`)
    this.#at++
    const parts: ZenNode[] = []
    if (CLOSURES.has(name)) {
      parts.push(this.#binary(0))
      if (this.#is("literal", "as")) {
        this.#at++
        if (!this.#is("literal")) throw new ZenParseError("alias")
        this.#at++
      }
      this.#expect("operator", ",")
      this.#depth++
      this.#binary(0)
      this.#depth--
      parts.push({ k: "closure" })
      this.#expect("bracket", ")")
    } else {
      parts.push(...this.#arguments())
    }
    return this.#postfix({ k: "list", parts })
  }

  /** Call arguments up to and including `)`. */
  #arguments(): ZenNode[] {
    const parts: ZenNode[] = []
    while (!this.#is("bracket", ")")) {
      parts.push(this.#binary(0))
      if (!this.#is("operator", ",")) break
      this.#at++
    }
    this.#expect("bracket", ")")
    return parts
  }

  #postfix(node: ZenNode): ZenNode {
    if (this.#is("operator", ".")) {
      this.#at++
      const property = this.#current
      this.#at++
      if (property === undefined) throw new ZenParseError("expected a property")
      const valid = property.kind === "identifier" || property.kind === "literal"
        || (property.kind === "operator" && ["and", "or", "not", "!", "??", "in"].includes(property.value))
      if (!valid) throw new ZenParseError(`invalid property ${property.value}`)
      if (this.#is("bracket", "(")) {
        if (!METHODS.has(property.value)) throw new ZenParseError(`unknown method ${property.value}`)
        this.#at++
        return this.#postfix({ k: "list", parts: [node, ...this.#arguments()] })
      }
      return this.#postfix({ k: "member", node, property: { k: "string", value: property.value } })
    }
    if (this.#is("bracket", "[")) {
      this.#at++
      if (this.#current === undefined) throw new ZenParseError("expected a property")
      if (this.#is("operator", ":")) {
        this.#at++
        const to = this.#is("bracket", "]") ? [] : [this.#binary(0)]
        this.#expect("bracket", "]")
        return this.#postfix({ k: "list", parts: [node, ...to] })
      }
      const from = this.#binary(0)
      if (this.#current === undefined) throw new ZenParseError("unexpected end")
      if (this.#is("operator", ":")) {
        this.#at++
        if (this.#current === undefined) throw new ZenParseError("invalid slice")
        const to = this.#is("bracket", "]") ? [] : [this.#binary(0)]
        this.#expect("bracket", "]")
        return this.#postfix({ k: "list", parts: [node, from, ...to] })
      }
      this.#expect("bracket", "]")
      return this.#postfix({ k: "member", node, property: from })
    }
    return node
  }

  #array(): ZenNode {
    this.#expect("bracket", "[")
    const parts: ZenNode[] = []
    while (!this.#is("bracket", "]")) {
      if (parts.length > 0) {
        this.#expect("operator", ",")
        if (this.#is("bracket", "]")) break
      }
      parts.push(this.#binary(0))
    }
    this.#expect("bracket", "]")
    return this.#postfix({ k: "list", parts })
  }

  /** `object`: unquoted keys are strings (`{actor: 1}` names no variable); no postfix follows. */
  #object(): ZenNode {
    this.#expect("bracket", "{")
    const parts: ZenNode[] = []
    if (this.#is("bracket", "}")) { this.#at++; return { k: "list", parts } }
    for (;;) {
      parts.push(this.#objectKey())
      this.#expect("operator", ":")
      parts.push(this.#binary(0))
      const t = this.#current
      if (t === undefined) break
      if (t.kind === "operator" && t.value === ",") { this.#at++; continue }
      if (t.kind === "bracket" && t.value === "}") break
      throw new ZenParseError("invalid object syntax")
    }
    this.#expect("bracket", "}")
    return { k: "list", parts }
  }

  #objectKey(): ZenNode {
    const t = this.#current
    if (t === undefined) throw new ZenParseError("expected an object key")
    switch (t.kind) {
      case "identifier": case "boolean": case "number": case "literal": case "string":
        this.#at++
        return { k: "string", value: t.value }
      case "bracket": {
        if (t.value !== "[") throw new ZenParseError("bracket key")
        this.#at++
        const key = this.#binary(0)
        this.#expect("bracket", "]")
        return key
      }
      default: throw new ZenParseError("object key")
    }
  }

  #template(): ZenNode {
    this.#expect("template", "`")
    const parts: ZenNode[] = []
    while (!this.#is("template", "`")) {
      if (!this.#is("template", "${")) throw new ZenParseError("template token")
      this.#at++
      parts.push(this.#binary(0))
      this.#expect("template", "}")
    }
    this.#expect("template", "`")
    return { k: "list", parts }
  }
}

/** `literal_path`: an identifier, or a chain of static members on one, through parentheses. */
function literalPath(node: ZenNode): string[] | null {
  switch (node.k) {
    case "identifier": return [node.name]
    case "parenthesized": return literalPath(node.inner)
    case "member": {
      if (node.property.k !== "string") return null
      const path = literalPath(node.node)
      return path === null ? null : [...path, node.property.value]
    }
    default: return null
  }
}

/** `collect_paths(node, "actor")` is non-empty. */
function readsActor(node: ZenNode): boolean {
  const path = literalPath(node)
  if (path !== null) return path[0] === "actor"
  switch (node.k) {
    case "leaf": case "string": case "identifier": return false
    case "root": case "pointer": case "closure": case "assignments": return true
    case "parenthesized": case "unary": return readsActor(node.inner)
    case "member": return readsActor(node.node) || readsActor(node.property)
    case "list": return node.parts.some(readsActor)
  }
}

/**
 * Whether `norn_expr::expression_read_paths(source, "actor")` is non-empty, or the expression does
 * not parse: a literal `actor…` path, or a root (`$root`), closure pointer, closure or assignment
 * anywhere. A bare `$` or a top-level `#` is an identifier, an unquoted object key a string, and a
 * name inside a string literal no name at all. The kernel also refuses source over its size bounds
 * (`validate_evaluation_source`); an admitted set never carries such an expression.
 */
export function mayReadActor(source: string): boolean {
  try {
    return readsActor(new Parser(zenTokens(source)).parse())
  } catch (error) {
    if (error instanceof ZenLexError || error instanceof ZenParseError) return true
    throw error
  }
}
