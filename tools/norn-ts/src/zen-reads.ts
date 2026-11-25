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

export type ZenTokenKind = "identifier" | "literal" | "number" | "string" | "boolean" | "operator" | "bracket" | "template" | "text"
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

  /** A template string, with its text segments as `lexer.rs` pushes them (`str_start` and all). */
  function template(): void {
    at++
    push("template", "`")
    let inExpression = false
    let textStart = at
    const text = (end: number) => push("text", chars.slice(textStart, end).join(""))
    while (at < chars.length) {
      const c = chars[at]!
      if (c === "`") { if (textStart < at) text(at); at++; push("template", "`"); return }
      if (!inExpression) {
        if (c === "$" && chars[at + 1] === "{") { text(at); at += 2; inExpression = true; push("template", "${"); continue }
        at++
        continue
      }
      if (c === "}") { push("template", "}"); at++; textStart = at; inExpression = false; continue }
      item()
    }
    // An unterminated template string is closed at the end, as the lexer closes it.
    if (!inExpression && textStart < chars.length) text(chars.length)
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
  | { readonly k: "number"; readonly text: string }
  | { readonly k: "call"; readonly name: string; readonly args: readonly ZenNode[] }
  | { readonly k: "identifier"; readonly name: string }
  | { readonly k: "string"; readonly value: string }
  | { readonly k: "root" | "pointer" | "closure" | "assignments" }
  | { readonly k: "parenthesized" | "unary"; readonly inner: ZenNode }
  | { readonly k: "member"; readonly node: ZenNode; readonly property: ZenNode }
  /** Any node with ordered children; `literal` marks a list or record literal (its size is bounded). */
  | { readonly k: "list"; readonly parts: readonly ZenNode[]; readonly literal?: "array" | "object" }

class ZenParseError extends Error {}

class Parser {
  #at = 0
  #depth = 0
  /**
   * `memoized_expression` (`parser.rs`): a top-level expression parsed once per (position, closure
   * depth), its end position (or its failure) reused, so the interval parser's speculation stays
   * linear on nested brackets.
   */
  readonly #memo = new Map<string, { readonly node: ZenNode; readonly end: number } | { readonly error: ZenParseError }>()
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
    if (precedence !== 0) return this.#binaryAt(precedence)
    const key = `${this.#at}:${this.#depth}`
    const known = this.#memo.get(key)
    if (known !== undefined) {
      if ("error" in known) throw known.error
      this.#at = known.end
      return known.node
    }
    try {
      const node = this.#binaryAt(0)
      this.#memo.set(key, { node, end: this.#at })
      return node
    } catch (error) {
      if (error instanceof ZenParseError) this.#memo.set(key, { error })
      throw error
    }
  }

  #binaryAt(precedence: number): ZenNode {
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
      case "boolean": this.#at++; return { k: "leaf" }
      case "number": this.#at++; return { k: "number", text: t.value }
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
    return this.#postfix({ k: "call", name, args: parts })
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
    return this.#postfix({ k: "list", parts, literal: "array" })
  }

  /** `object`: unquoted keys are strings (`{actor: 1}` names no variable); no postfix follows. */
  #object(): ZenNode {
    this.#expect("bracket", "{")
    const parts: ZenNode[] = []
    if (this.#is("bracket", "}")) { this.#at++; return { k: "list", parts, literal: "object" } }
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
    return { k: "list", parts, literal: "object" }
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
      const t = this.#current
      if (t === undefined) throw new ZenParseError("unterminated template")
      if (t.kind === "text") { this.#at++; parts.push({ k: "string", value: t.value }); continue }
      if (t.kind === "template" && t.value === "}") { this.#at++; continue }
      if (!(t.kind === "template" && t.value === "${")) throw new ZenParseError("template token")
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
    case "leaf": case "number": case "string": case "identifier": return false
    case "root": case "pointer": case "closure": case "assignments": return true
    case "parenthesized": case "unary": return readsActor(node.inner)
    case "member": return readsActor(node.node) || readsActor(node.property)
    case "list": return node.parts.some(readsActor)
    case "call": return node.args.some(readsActor)
  }
}

/** `StructuralLimits::NORN_EXPR_4` (`norn_expr::check::bounds`). */
const LIMITS = { sourceBytes: 16_384, nodes: 4096, depth: 128, parseNesting: 512, stringBytes: 4096, literalItems: 1024 }
const utf8 = new TextEncoder()
const isWordByte = (b: number) => (b >= 48 && b <= 57) || (b >= 65 && b <= 90) || (b >= 97 && b <= 122) || b === 95
const OPENING = new Set([..."+-*/%^!<>=&|?:"].map((c) => c.charCodeAt(0)))

/**
 * `bounds::lexical`, on the raw UTF-8 source with no string tracking, as the kernel scans it: the
 * source size, bracket nesting, and the count of characters and keywords that open a parser level.
 */
function lexicalBounds(source: string): boolean {
  const bytes = utf8.encode(source)
  if (bytes.length > LIMITS.sourceBytes) return false
  let brackets = 0, levels = 0, word = ""
  const digitAt = (i: number) => i >= 0 && i < bytes.length && bytes[i]! >= 48 && bytes[i]! <= 57
  for (let i = 0; i <= bytes.length; i++) {
    const b = i < bytes.length ? bytes[i]! : 32
    if (isWordByte(b)) { word += String.fromCharCode(b); continue }
    const keyword = word === "and" || word === "or" || word === "not" || word === "in"
    word = ""
    let opens = false
    if (b === 40 || b === 91 || b === 123) {
      brackets++
      if (brackets > LIMITS.depth) return false
      opens = true
    } else if (b === 41 || b === 93 || b === 125) {
      brackets = Math.max(0, brackets - 1)
      opens = true
    } else if (b === 46) opens = !(digitAt(i - 1) && digitAt(i + 1))
    else opens = OPENING.has(b)
    levels += (opens ? 1 : 0) + (keyword ? 1 : 0)
    if (levels > LIMITS.parseNesting) return false
  }
  return true
}

/**
 * A number literal both checks admit: the parser's `Decimal::from_str_exact` on the raw token (at
 * most 28 fraction digits, trailing zeros included, and a 96-bit mantissa; its `from_scientific`
 * fallback needs an exponent, which the next check refuses), then `Number::from_str` (a JSON
 * decimal with no exponent).
 */
function exactNumber(text: string): boolean {
  const match = /^-?(0|[1-9][0-9]*)(?:\.([0-9]+))?$/.exec(text)
  if (match === null) return false
  const fraction = match[2] ?? ""
  if (fraction.length > 28) return false
  return BigInt(`${match[1]}${fraction}`) < 2n ** 96n
}

const children = (node: ZenNode): readonly ZenNode[] => {
  switch (node.k) {
    case "parenthesized": case "unary": return [node.inner]
    case "member": return [node.node, node.property]
    case "list": return node.parts
    case "call": return node.args
    default: return []
  }
}

/** `bounded_string_call`: `tokens`, `take` and `join` with their literal bounds. */
function boundedCall(node: Extract<ZenNode, { k: "call" }>): boolean {
  const literal = (arg: ZenNode | undefined, max: number) => arg?.k === "number" && /^\d+$/.test(arg.text) && Number(arg.text) <= max
  switch (node.name) {
    case "tokens": return node.args.length === 4 && node.args[1]?.k === "string" && node.args[1].value === "alnum" && literal(node.args[2], 256) && literal(node.args[3], 256)
    case "take": return node.args.length === 2 && literal(node.args[1], 16_384)
    case "join": return node.args.length === 2 && node.args[1]?.k === "string" && utf8.encode(node.args[1].value).length <= 256
    default: return true
  }
}

/**
 * `bounds::structure`, on the AST: node count, nesting depth, literal sizes, exact plain-decimal
 * number literals, and the bounded string builtins. (Closure depth needs no check: any closure is
 * already a read.)
 */
function structuralBounds(root: ZenNode): boolean {
  let nodes = 0
  const stack: Array<readonly [ZenNode, number]> = [[root, 1]]
  while (stack.length > 0) {
    const [node, depth] = stack.pop()!
    if (++nodes > LIMITS.nodes || depth > LIMITS.depth) return false
    if (node.k === "number" && !exactNumber(node.text)) return false
    if (node.k === "string" && utf8.encode(node.value).length > LIMITS.stringBytes) return false
    if (node.k === "list" && node.literal !== undefined && (node.literal === "array" ? node.parts.length : node.parts.length / 2) > LIMITS.literalItems) return false
    if (node.k === "call" && !boundedCall(node)) return false
    for (const child of children(node)) stack.push([child, depth + 1])
  }
  return true
}

/**
 * Whether `norn_kernel::actor_derived_fields` counts the expression as reading the verified actor:
 * `norn_expr::expression_read_paths(source, "actor")` is non-empty, or the source fails the checks
 * that run first (`validate_evaluation_source`: the lexical and structural bounds, exact number
 * literals) or does not lex or parse. A read is a literal `actor…` path, a root (`$root`), a closure
 * or its pointer, or an assignment, anywhere. A bare `$` or a top-level `#` is an identifier, an
 * unquoted object key a string, and a name inside a string literal no name at all.
 */
export function mayReadActor(source: string): boolean {
  if (!lexicalBounds(source)) return true
  let root: ZenNode
  try {
    root = new Parser(zenTokens(source)).parse()
  } catch (error) {
    if (error instanceof ZenLexError || error instanceof ZenParseError) return true
    throw error
  }
  return !structuralBounds(root) || readsActor(root)
}
