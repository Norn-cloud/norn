/**
 * What `<NornAppRenderer>` can draw, and what a contract asks of it (UI-VIEW-1, L6).
 *
 * `requiredCapabilities` is the renderer capability catalog a contract requires, as
 * `<family>:<name>` strings: the checker's own `renderer_capabilities`
 * (`crates/norn-ui-checker/src/ui_sdk.rs`), walked here over the same document so the renderer
 * decides before it draws, with no WASM on the page's first paint. `test/capabilities.test.ts`
 * holds the two to identical answers through the checker's WASM export (`rendererCapabilities`).
 *
 * The renderer fails closed: a page that needs a capability outside `RENDERER_CAPABILITIES`, or a
 * binding or control form the renderer does not draw (`unsupported`), is refused with what is
 * missing, never drawn in part.
 */
import { UI_SDK } from "../../../tools/norn-ts/generated/ui-sdk.ts"
import { nodesOf, UI_SDK_PREFIX, type ComponentKind, type WidgetNode, type WidgetUx } from "./ux.ts"

/** The agent capabilities every renderer needs (L6, `AGENT_CAPABILITIES` in ui_sdk.rs). */
export const AGENT_CAPABILITIES = ["agent:confirm", "agent:redaction"] as const

/**
 * UI SDK primitives this renderer draws as catalog layers (`ui-sdk:<name>` held open in `heldIn`).
 */
export const RENDERED_PRIMITIVES = ["alert-dialog", "dialog"] as const
export type RenderedPrimitive = (typeof RENDERED_PRIMITIVES)[number]

/**
 * UI SDK primitives backing this renderer's native components (`section` tabs, `control`/`action`
 * buttons, `form` fields/inputs/checkboxes/selects). Their foundation behaviours are declared in
 * {@link RENDERED_BEHAVIORS} without claiming them as catalog `Layer` primitives.
 */
export const NATIVE_PRIMITIVES = ["button", "checkbox", "field", "input", "select", "tabs"] as const

/**
 * The foundation behaviours of {@link RENDERED_PRIMITIVES} and {@link NATIVE_PRIMITIVES}, as the
 * pinned catalog (`crates/norn-ui-checker/catalog/ui-sdk.json`) lists them;
 * `test/capabilities.test.ts` holds this list to the catalog, so a UI SDK bump that changes them
 * fails there.
 */
export const RENDERED_BEHAVIORS = [
  "disclosure/disclosure", "focus/collection", "focus/press-keeps-focus", "focus/roving-focus", "focus/tabbable",
  "forms/control", "forms/form", "forms/text-field", "forms/toggle-control", "forms/value",
  "lists/async-options", "lists/field-name", "lists/item-icon", "lists/listbox", "lists/listbox-engine",
  "lists/listbox-keyboard", "lists/listbox-keys", "lists/listbox-listing", "lists/listbox-parts",
  "lists/listbox-popup", "lists/listbox-search", "lists/listbox-select-all", "lists/listbox-value",
  "motion/panel", "motion/presence", "platform/anchoring", "platform/announce", "platform/button", "platform/env",
  "platform/keyboard", "platform/layers", "platform/layout", "platform/moves", "platform/platform",
  "platform/position", "platform/scrollbar", "platform/sides", "platform/style",
  "popups/bridge", "popups/create-popup", "popups/dismissal", "popups/frame-guards", "popups/hover",
  "popups/modal", "popups/modal-attribute", "popups/popup", "popups/popup-behavior", "popups/popup-elements",
  "popups/popup-features", "popups/popup-payload", "popups/popup-presets", "popups/popup-slots",
  "popups/popup-steps", "popups/presentation", "selection/indicator",
  "shortcuts/create-shortcuts", "shortcuts/shortcut-order", "shortcuts/shortcut-slot", "shortcuts/shortcuts",
  "text/accessible-name", "text/locale", "text/match", "text/typeahead",
] as const

/**
 * Component kinds this renderer draws. `child_grid` and `composition` remain refused until their
 * contracts have dedicated renderers.
 */
export const RENDERED_COMPONENTS: readonly ComponentKind[] = ["action", "catalog", "control", "detail", "dialog", "field", "form", "list", "navigation", "section"]

const OUTCOMES = ["accepted", "refused", "pending_approval", "answer_moved", "network_failure", "not_served", "stale", "subscription_refused", "timeout", "cancelled"] as const

/** Every capability this renderer declares. */
export const RENDERER_CAPABILITIES: ReadonlySet<string> = new Set([
  ...AGENT_CAPABILITIES,
  ...RENDERED_COMPONENTS.map((kind) => `component:${kind}`),
  ...RENDERED_PRIMITIVES.map((name) => `ui-sdk:${name}`),
  ...RENDERED_BEHAVIORS.map((name) => `behavior:${name}`),
  "view:collection", "view:record", "view:form",
  "event:page_opened", "event:user", "event:outcome", "event:subscription", "event:timer",
  ...OUTCOMES.map((outcome) => `outcome:${outcome}`),
  "effect:start:action", "effect:start:read", "effect:cancel", "effect:navigate",
  // A context read (UI-CONTEXT-READ-1), read through the host's reader (`BindingRead` `record`).
  "read:context",
])

const primitives: ReadonlySet<string> = new Set(Object.keys(UI_SDK.components))

/** The UI SDK primitive a node draws, when it names one the pinned catalog has (`primitive_of`). */
export function primitiveOf(node: WidgetNode): string | null {
  if (node.component.kind !== "catalog" || node.component.entry.widget !== undefined) return null
  const { entryId } = node.component.entry
  if (!entryId.startsWith(UI_SDK_PREFIX)) return null
  const name = entryId.slice(UI_SDK_PREFIX.length)
  return primitives.has(name) ? name : null
}

/** The capabilities `ux` requires, sorted as the checker sorts them (`BTreeSet<String>`). */
export function requiredCapabilities(ux: WidgetUx): string[] {
  const out = new Set<string>(AGENT_CAPABILITIES)
  if ((ux.contextReads ?? []).length > 0) out.add("read:context")
  for (const page of ux.pages) {
    for (const node of nodesOf(page.root)) {
      out.add(`component:${node.component.kind}`)
      const primitive = primitiveOf(node)
      if (primitive !== null) out.add(`ui-sdk:${primitive}`)
      for (const behavior of node.behaviors ?? []) out.add(`behavior:${behavior}`)
    }
    if (page.view !== undefined) out.add(`view:${page.view.kind}`)
    const machine = page.machine
    if (machine === undefined) continue
    for (const event of machine.events) {
      out.add(`event:${event.source.kind}`)
      if (event.source.kind === "outcome") out.add(`outcome:${event.source.outcome}`)
    }
    for (const transition of machine.transitions) {
      for (const effect of transition.effects ?? []) out.add(effect.kind === "start" ? `effect:start:${effect.call.kind}` : `effect:${effect.kind}`)
    }
  }
  return [...out].sort((left, right) => (left < right ? -1 : left > right ? 1 : 0))
}

type ZenToken =
  | { readonly kind: "ident"; readonly value: string }
  | { readonly kind: "number"; readonly value: string }
  | { readonly kind: "string"; readonly value: string }
  | { readonly kind: "template"; readonly parts: readonly ({ readonly kind: "text"; readonly value: string } | { readonly kind: "expr"; readonly tokens: readonly ZenToken[] })[] }
  | { readonly kind: "punct"; readonly value: string }

function tokenizeZen(expression: string): ZenToken[] {
  const tokens: ZenToken[] = []
  let i = 0
  while (i < expression.length) {
    const ch = expression[i]!
    if (/\s/.test(ch)) {
      i += 1
      continue
    }
    if (ch === '"' || ch === "'") {
      // Match vendor/zen lexer.rs:188 — simple strings scan directly to matching quote without backslash escapes.
      const quote = ch
      const start = i + 1
      i += 1
      while (i < expression.length && expression[i] !== quote) i += 1
      const value = expression.slice(start, i)
      if (i < expression.length) i += 1
      tokens.push({ kind: "string", value })
      continue
    }
    if (ch === "`") {
      // Match vendor/zen lexer.rs:131 — template strings do not escape `${` with backslashes.
      const parts: Array<{ readonly kind: "text"; readonly value: string } | { readonly kind: "expr"; readonly tokens: readonly ZenToken[] }> = []
      i += 1
      let textStart = i
      while (i < expression.length && expression[i] !== "`") {
        if (expression[i] === "$" && expression[i + 1] === "{") {
          if (i > textStart) parts.push({ kind: "text", value: expression.slice(textStart, i) })
          i += 2
          let depth = 1
          const exprStart = i
          while (i < expression.length && depth > 0) {
            const c = expression[i]!
            if (c === '"' || c === "'") {
              const q = c
              i += 1
              while (i < expression.length && expression[i] !== q) i += 1
              if (i < expression.length) i += 1
              continue
            }
            if (c === "{") depth += 1
            else if (c === "}") depth -= 1
            if (depth > 0) i += 1
          }
          const innerExpr = expression.slice(exprStart, i)
          if (i < expression.length && expression[i] === "}") i += 1
          parts.push({ kind: "expr", tokens: tokenizeZen(innerExpr) })
          textStart = i
          continue
        }
        i += 1
      }
      if (i > textStart) parts.push({ kind: "text", value: expression.slice(textStart, i) })
      if (i < expression.length && expression[i] === "`") i += 1
      tokens.push({ kind: "template", parts })
      continue
    }
    if (/[0-9]/.test(ch)) {
      // Match vendor/zen lexer.rs:221 — consume digits, `_`, and at most one `.` (including trailing `10.`,
      // stopping before `..` range operator), followed by optional lowercase `e[+-]?digits`.
      let end = i + 1
      let fractal = false
      while (end < expression.length) {
        const c = expression[end]!
        if (/[0-9_]/.test(c)) {
          end += 1
          continue
        }
        if (c === ".") {
          if (fractal || expression[end + 1] === ".") break
          fractal = true
          end += 1
          continue
        }
        break
      }
      if (expression[end] === "e") {
        let expEnd = end + 1
        if (expression[expEnd] === "+" || expression[expEnd] === "-") expEnd += 1
        const digitsStart = expEnd
        while (expEnd < expression.length && /[0-9]/.test(expression[expEnd]!)) expEnd += 1
        if (expEnd > digitsStart) end = expEnd
      }
      tokens.push({ kind: "number", value: expression.slice(i, end) })
      i = end
      continue
    }
    if (/[A-Za-z_$]/.test(ch)) {
      let end = i + 1
      while (end < expression.length && /[A-Za-z0-9_$]/.test(expression[end]!)) end += 1
      tokens.push({ kind: "ident", value: expression.slice(i, end) })
      i = end
      continue
    }
    if (ch === "?" && expression[i + 1] === ".") {
      tokens.push({ kind: "punct", value: "?." })
      i += 2
      continue
    }
    if (ch === "." && expression[i + 1] === ".") {
      if (expression[i + 2] === ".") {
        tokens.push({ kind: "punct", value: "..." })
        i += 3
      } else {
        tokens.push({ kind: "punct", value: ".." })
        i += 2
      }
      continue
    }
    if (ch === "*" && expression[i + 1] === "*") {
      tokens.push({ kind: "punct", value: "**" })
      i += 2
      continue
    }
    tokens.push({ kind: "punct", value: ch })
    i += 1
  }
  return tokens
}

type TaintValue =
  | { readonly kind: "root_input" }
  | { readonly kind: "string"; readonly value: string }
  | { readonly kind: "number"; readonly raw: string; readonly num: number }
  | { readonly kind: "object"; readonly fields: Readonly<Record<string, TaintValue>> }
  | { readonly kind: "array"; readonly elements: readonly TaintValue[] }
  | { readonly kind: "union"; readonly members: readonly TaintValue[] }
  | { readonly kind: "other" }

const OTHER: TaintValue = { kind: "other" }
const ROOT_INPUT: TaintValue = { kind: "root_input" }

function unionTaint(left: TaintValue, right: TaintValue): TaintValue {
  if (left.kind === "other" && right.kind === "other") return OTHER
  const flat = (value: TaintValue): readonly TaintValue[] => (value.kind === "union" ? value.members : [value])
  return { kind: "union", members: [...flat(left), ...flat(right)] }
}

function mayBeRootInput(value: TaintValue): boolean {
  if (value.kind === "root_input") return true
  if (value.kind === "union") return value.members.some(mayBeRootInput)
  return false
}

function stringLiteralsOf(value: TaintValue): readonly string[] {
  if (value.kind === "string") return [value.value]
  if (value.kind === "number") return [value.raw]
  if (value.kind === "union") return value.members.flatMap(stringLiteralsOf)
  return []
}

function templateStringsOf(value: TaintValue): readonly string[] | null {
  if (value.kind === "string") return [value.value]
  if (value.kind === "number") {
    const cleaned = value.raw.replace(/_/g, "")
    return [cleaned.endsWith(".") ? cleaned.slice(0, -1) : cleaned]
  }
  if (value.kind === "union") {
    const out: string[] = []
    for (const member of value.members) {
      const part = templateStringsOf(member)
      if (part === null) return null
      out.push(...part)
    }
    return out
  }
  return null
}

function accessProperty(receiver: TaintValue, prop: string): TaintValue {
  if (receiver.kind === "union") {
    let acc: TaintValue | undefined
    for (const member of receiver.members) {
      const next = accessProperty(member, prop)
      acc = acc === undefined ? next : unionTaint(acc, next)
    }
    return acc ?? OTHER
  }
  if (receiver.kind === "object" && Object.hasOwn(receiver.fields, prop)) {
    return receiver.fields[prop]!
  }
  return OTHER
}

function accessIndex(receiver: TaintValue, key: TaintValue): TaintValue {
  if (receiver.kind === "union") {
    let acc: TaintValue | undefined
    for (const member of receiver.members) {
      const next = accessIndex(member, key)
      acc = acc === undefined ? next : unionTaint(acc, next)
    }
    return acc ?? OTHER
  }
  if (receiver.kind === "array") {
    if (receiver.elements.length === 0) return OTHER
    return receiver.elements.reduce((acc, el) => unionTaint(acc, el))
  }
  if (receiver.kind === "object") {
    const keys = stringLiteralsOf(key)
    if (keys.length === 0) return OTHER
    let acc: TaintValue | undefined
    for (const prop of keys) {
      if (!Object.hasOwn(receiver.fields, prop)) continue
      const next = receiver.fields[prop]!
      acc = acc === undefined ? next : unionTaint(acc, next)
    }
    return acc ?? OTHER
  }
  return OTHER
}

/**
 * Whether a Zen expression accesses the unsupported `input.recordVersion` slot — parsed with a
 * lightweight recursive-descent expression analyzer so direct, parenthesized, optional-chained,
 * conditional, array-indexed, object-wrapped, and string-concatenated accesses on `input`
 * (`input.recordVersion`, `(true ? input : input).recordVersion`, `[input][0].recordVersion`,
 * `({ wrapped: input }).wrapped.recordVersion`, `input["record" + "Version"]`) are refused, while
 * string literals (`"input.recordVersion"`), array literals (`input.draft.kind in ["recordVersion"]`),
 * and nested domain properties (`((input).record).recordVersion`) are never misclassified.
 */
export function referencesRecordVersion(expression: string): boolean {
  let found = false

  const analyzeTokens = (tokens: readonly ZenToken[]): TaintValue => {
    let pos = 0

    const peek = (offset = 0): ZenToken | undefined => tokens[pos + offset]
    const eat = (): ZenToken | undefined => {
      const token = tokens[pos]
      if (token !== undefined) pos += 1
      return token
    }
    const isPunct = (token: ZenToken | undefined, value: string): boolean => token?.kind === "punct" && token.value === value
    const isIdent = (token: ZenToken | undefined, value: string): boolean => token?.kind === "ident" && token.value === value

    const decimalScale = (raw: string): number => {
      const cleaned = raw.replace(/_/g, "")
      if (cleaned.includes("e") || cleaned.includes("E")) return -1
      const dot = cleaned.indexOf(".")
      return dot < 0 ? 0 : cleaned.length - dot - 1
    }

    const formatNumberResult = (n: number, scale: number): TaintValue => {
      if (!Number.isFinite(n)) return OTHER
      if (scale >= 0 && scale <= 12 && Number.isSafeInteger(Math.round(n * 10 ** scale))) {
        return { kind: "number", raw: scale === 0 ? String(Math.round(n)) : n.toFixed(scale), num: n }
      }
      return { kind: "number", raw: String(n), num: n }
    }

    // Exact port of vendor/zen/core/expression/src/parser/{standard.rs,constants.rs}:
    // 10 or/||, 15 and/&&, 20 comparisons/in/not in, 25 .. range, 30 +/-, 60 */ / %, 70 ** (right), 80 ??.
    type BinOp = { readonly kind: string; readonly prec: number; readonly assoc: "left" | "right"; readonly consume: number }

    const peekBinaryOp = (): BinOp | null => {
      const t0 = peek()
      const t1 = peek(1)
      const t2 = peek(2)
      if (isPunct(t0, "|") && isPunct(t1, "|")) return { kind: "or", prec: 10, assoc: "left", consume: 2 }
      if (isIdent(t0, "or")) return { kind: "or", prec: 10, assoc: "left", consume: 1 }
      if (isPunct(t0, "&") && isPunct(t1, "&")) return { kind: "and", prec: 15, assoc: "left", consume: 2 }
      if (isIdent(t0, "and")) return { kind: "and", prec: 15, assoc: "left", consume: 1 }
      if ((isPunct(t0, "=") && isPunct(t1, "=")) || (isPunct(t0, "!") && isPunct(t1, "="))) {
        return { kind: "cmp", prec: 20, assoc: "left", consume: isPunct(t2, "=") ? 3 : 2 }
      }
      if ((isPunct(t0, "<") && isPunct(t1, "=")) || (isPunct(t0, ">") && isPunct(t1, "="))) {
        return { kind: "cmp", prec: 20, assoc: "left", consume: 2 }
      }
      if (isPunct(t0, "<") || isPunct(t0, ">") || isIdent(t0, "in")) return { kind: "cmp", prec: 20, assoc: "left", consume: 1 }
      if (isIdent(t0, "not") && isIdent(t1, "in")) return { kind: "cmp", prec: 20, assoc: "left", consume: 2 }
      if (isPunct(t0, "..")) return { kind: "range", prec: 25, assoc: "left", consume: 1 }
      if (isPunct(t0, "+")) return { kind: "+", prec: 30, assoc: "left", consume: 1 }
      if (isPunct(t0, "-")) return { kind: "-", prec: 30, assoc: "left", consume: 1 }
      if (isPunct(t0, "*")) return { kind: "*", prec: 60, assoc: "left", consume: 1 }
      if (isPunct(t0, "/")) return { kind: "/", prec: 60, assoc: "left", consume: 1 }
      if (isPunct(t0, "%")) return { kind: "%", prec: 60, assoc: "left", consume: 1 }
      if (isPunct(t0, "**")) return { kind: "**", prec: 70, assoc: "right", consume: 1 }
      if (isPunct(t0, "?") && isPunct(t1, "?")) return { kind: "??", prec: 80, assoc: "left", consume: 2 }
      return null
    }

    const evalBinary = (op: string, left: TaintValue, right: TaintValue): TaintValue => {
      if (op === "or" || op === "and" || op === "??") return unionTaint(left, right)
      if (op === "+" && left.kind === "string" && right.kind === "string") {
        return { kind: "string", value: left.value + right.value }
      }
      if (left.kind === "number" && right.kind === "number") {
        if (op === "+" || op === "-") {
          const n = op === "+" ? left.num + right.num : left.num - right.num
          const s1 = decimalScale(left.raw)
          const s2 = decimalScale(right.raw)
          return formatNumberResult(n, s1 >= 0 && s2 >= 0 ? Math.max(s1, s2) : -1)
        }
        if (op === "*") {
          const s1 = decimalScale(left.raw)
          const s2 = decimalScale(right.raw)
          return formatNumberResult(left.num * right.num, s1 >= 0 && s2 >= 0 ? s1 + s2 : -1)
        }
        if (op === "/") return formatNumberResult(left.num / right.num, -1)
        if (op === "%") return formatNumberResult(left.num % right.num, -1)
        if (op === "**") {
          const s1 = decimalScale(left.raw)
          const scale = s1 === 0 && Number.isInteger(right.num) && right.num >= 0 ? 0 : -1
          return formatNumberResult(left.num ** right.num, scale)
        }
      }
      return OTHER
    }

    const parseExpr = (): TaintValue => parseBinary(0)

    const parseBinary = (minPrec: number): TaintValue => {
      let left = parseUnary()
      while (pos < tokens.length) {
        const op = peekBinaryOp()
        if (op === null || op.prec < minPrec) break
        for (let k = 0; k < op.consume; k += 1) eat()
        const right = parseBinary(op.assoc === "left" ? op.prec + 1 : op.prec)
        left = evalBinary(op.kind, left, right)
      }
      if (minPrec === 0 && isPunct(peek(), "?")) {
        eat()
        const consequent = parseBinary(0)
        if (isPunct(peek(), ":")) {
          eat()
          const alternate = parseBinary(0)
          return unionTaint(consequent, alternate)
        }
        return consequent
      }
      return left
    }

    const parseUnary = (): TaintValue => {
      const t0 = peek()
      if (isPunct(t0, "!") || isIdent(t0, "not")) {
        eat()
        parseBinary(50)
        return OTHER
      }
      if (isPunct(t0, "-") || isPunct(t0, "+")) {
        const op = eat()!
        const opChar = op.kind === "punct" ? op.value : ""
        const operand = parseBinary(200)
        if (operand.kind === "number") {
          const n = opChar === "-" ? -operand.num : operand.num
          const raw = opChar === "-" ? (operand.raw.startsWith("-") ? operand.raw.slice(1) : `-${operand.raw}`) : operand.raw
          return Number.isFinite(n) ? { kind: "number", raw, num: n } : OTHER
        }
        return OTHER
      }
      return parsePostfix()
    }

    const parsePostfix = (): TaintValue => {
      let value = parsePrimary()
      while (pos < tokens.length) {
        const t0 = peek()
        if (isPunct(t0, ".") || isPunct(t0, "?.")) {
          eat()
          if (isPunct(peek(), "[")) {
            eat()
            const key = parseExpr()
            if (isPunct(peek(), "]")) eat()
            if (mayBeRootInput(value) && stringLiteralsOf(key).includes("recordVersion")) found = true
            value = accessIndex(value, key)
            continue
          }
          const member = peek()
          if (member?.kind === "ident") {
            eat()
            if (mayBeRootInput(value) && member.value === "recordVersion") found = true
            value = accessProperty(value, member.value)
            continue
          }
          value = OTHER
          continue
        }
        if (isPunct(t0, "[")) {
          eat()
          const key = parseExpr()
          if (isPunct(peek(), "]")) eat()
          if (mayBeRootInput(value) && stringLiteralsOf(key).includes("recordVersion")) found = true
          value = accessIndex(value, key)
          continue
        }
        if (isPunct(t0, "(")) {
          eat()
          while (pos < tokens.length && !isPunct(peek(), ")")) {
            parseExpr()
            if (isPunct(peek(), ",")) eat()
            else break
          }
          if (isPunct(peek(), ")")) eat()
          value = OTHER
          continue
        }
        break
      }
      return value
    }

    const parsePrimary = (): TaintValue => {
      const t0 = peek()
      if (t0 === undefined) return OTHER
      if (isPunct(t0, "(")) {
        eat()
        const startPos = pos
        const inner = parseExpr()
        const sawRange = tokens.slice(startPos, pos).some((tok) => tok.kind === "punct" && tok.value === "..")
        if (isPunct(peek(), ")")) {
          eat()
        } else if (sawRange && isPunct(peek(), "]")) {
          eat()
          return OTHER
        }
        return inner
      }
      if (isPunct(t0, "[")) {
        eat()
        const elements: TaintValue[] = []
        const startPos = pos
        while (pos < tokens.length && !isPunct(peek(), "]") && !isPunct(peek(), ")")) {
          elements.push(parseExpr())
          if (isPunct(peek(), ",")) eat()
          else break
        }
        const sawRange = tokens.slice(startPos, pos).some((tok) => tok.kind === "punct" && tok.value === "..")
        if (isPunct(peek(), "]")) {
          eat()
        } else if (sawRange && isPunct(peek(), ")")) {
          eat()
          return OTHER
        }
        return { kind: "array", elements }
      }
      if (isPunct(t0, "{")) {
        eat()
        const fields: Record<string, TaintValue> = Object.create(null) as Record<string, TaintValue>
        while (pos < tokens.length && !isPunct(peek(), "}")) {
          const keyToken = peek()
          if (keyToken === undefined) break
          if (isPunct(keyToken, "[")) {
            eat()
            const computedKey = parseExpr()
            if (isPunct(peek(), "]")) eat()
            if (isPunct(peek(), ":")) {
              eat()
              const fieldVal = parseExpr()
              for (const keyName of stringLiteralsOf(computedKey)) {
                if (keyName !== "__proto__" && keyName !== "constructor" && keyName !== "prototype") {
                  fields[keyName] = fieldVal
                }
              }
            }
          } else if (keyToken.kind === "ident" || keyToken.kind === "string" || keyToken.kind === "number") {
            const keyName = keyToken.value
            eat()
            if (isPunct(peek(), ":")) {
              eat()
              const fieldVal = parseExpr()
              if (keyName !== "__proto__" && keyName !== "constructor" && keyName !== "prototype") {
                fields[keyName] = fieldVal
              }
            } else if (keyToken.kind === "ident") {
              const fieldVal = keyToken.value === "input" ? ROOT_INPUT : OTHER
              if (keyName !== "__proto__" && keyName !== "constructor" && keyName !== "prototype") {
                fields[keyName] = fieldVal
              }
            }
          } else {
            eat()
          }
          if (isPunct(peek(), ",")) eat()
          else break
        }
        if (isPunct(peek(), "}")) eat()
        return { kind: "object", fields }
      }
      if (t0.kind === "template") {
        eat()
        let variants: string[] | null = [""]
        for (const part of t0.parts) {
          if (part.kind === "text") {
            if (variants !== null) variants = variants.map((prefix) => prefix + part.value)
          } else {
            const val = analyzeTokens(part.tokens)
            const strs = templateStringsOf(val)
            if (strs === null || variants === null) {
              variants = null
            } else {
              const next: string[] = []
              for (const prefix of variants) {
                for (const suffix of strs) next.push(prefix + suffix)
              }
              variants = next
            }
          }
        }
        if (variants === null) return OTHER
        if (variants.length === 1) return { kind: "string", value: variants[0]! }
        return { kind: "union", members: variants.map((value) => ({ kind: "string" as const, value })) }
      }
      if (t0.kind === "string") {
        eat()
        return { kind: "string", value: t0.value }
      }
      if (t0.kind === "number") {
        eat()
        const num = Number(t0.value.replace(/_/g, ""))
        return { kind: "number", raw: t0.value, num }
      }
      if (t0.kind === "ident") {
        eat()
        return t0.value === "input" ? ROOT_INPUT : OTHER
      }
      eat()
      return OTHER
    }

    let result: TaintValue = OTHER
    while (pos < tokens.length) {
      const before = pos
      result = parseExpr()
      if (pos === before) pos += 1
    }
    return result
  }

  analyzeTokens(tokenizeZen(expression))
  return found
}

const NUMBER_FORMATS = new Set(["integer", "decimal", "currency", "percent"])

/**
 * What the renderer cannot draw although the capability catalog does not name it: a data binding
 * read it has no reader for, a control input it has no data for, a catalog island it was not given,
 * a swipe-only dialog, an unformatted numeric field (ambiguous integer vs exact decimal), or an
 * expression referencing `input.recordVersion` (unsupported by the current WASM ABI).
 */
export function unsupported(ux: WidgetUx): string[] {
  const out: string[] = []
  for (const binding of ux.dataBindings) {
    const query = binding.query
    if (query.kind === "object_list" && query.filters.length === 0 && query.sort.length === 0) continue
    if (query.kind === "object_runtime_detail") continue
    out.push(`read:${query.kind}${query.kind === "object_list" ? ":filtered" : ""} (${binding.bindingId})`)
  }
  for (const binding of ux.actionBindings) {
    for (const [member, expression] of Object.entries(binding.inputMapping)) {
      if (referencesRecordVersion(expression)) {
        out.push(`expr:recordVersion (action:${binding.bindingId}.${member})`)
      }
    }
  }
  for (const field of ux.fields) {
    if (field.control === "number" && (field.format === undefined || !NUMBER_FORMATS.has(field.format))) {
      out.push(`field:number:unformatted (${field.fieldId})`)
    }
    for (const condition of [field.visibleWhen, field.requiredWhen, field.readOnlyWhen]) {
      if (condition !== undefined && referencesRecordVersion(condition.expression)) {
        out.push(`expr:recordVersion (field:${field.fieldId})`)
      }
    }
  }
  for (const placement of ux.actionPlacements) {
    const extra = placement as unknown as {
      readonly enabledWhen?: { readonly expression?: string }
      readonly selectionInputMapping?: Readonly<Record<string, string>>
    }
    if (typeof extra.enabledWhen?.expression === "string" && referencesRecordVersion(extra.enabledWhen.expression)) {
      out.push(`expr:recordVersion (placement:${placement.placementId})`)
    }
    for (const [member, expression] of Object.entries(extra.selectionInputMapping ?? {})) {
      if (referencesRecordVersion(expression)) {
        out.push(`expr:recordVersion (placement:${placement.placementId}.${member})`)
      }
    }
  }
  for (const item of ux.navigationItems) {
    const extra = item as unknown as { readonly visibleWhen?: { readonly expression?: string } }
    if (typeof extra.visibleWhen?.expression === "string" && referencesRecordVersion(extra.visibleWhen.expression)) {
      out.push(`expr:recordVersion (navigation:${item.itemId})`)
    }
  }
  for (const page of ux.pages) {
    for (const node of nodesOf(page.root)) {
      if (node.component.kind === "control" && node.component.input === "choice") out.push(`control:choice (${node.nodeId})`)
      if (node.component.kind === "catalog" && node.component.entry.widget !== undefined) out.push(`catalog:pinned-widget (${node.nodeId})`)
      if (node.component.kind === "dialog") {
        const methods = node.component.dismissalMethods ?? []
        const supported = methods.some((method) => method === "escape" || method === "scrim" || method === "visible_close")
        if (!supported) {
          out.push(methods.includes("swipe") ? `dialog:swipe-only (${node.nodeId})` : `dialog:no-dismissal (${node.nodeId})`)
        }
      }
    }
  }
  return out
}

/** What `ux` requires that the renderer declaring `declared` lacks; empty means it may draw it. */
export function missingCapabilities(ux: WidgetUx, declared: ReadonlySet<string> = RENDERER_CAPABILITIES): string[] {
  return [...requiredCapabilities(ux).filter((capability) => !declared.has(capability)), ...unsupported(ux)]
}
