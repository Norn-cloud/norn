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
  | { readonly kind: "string"; readonly value: string }
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
      const quote = ch
      let value = ""
      i += 1
      while (i < expression.length && expression[i] !== quote) {
        if (expression[i] === "\\" && i + 1 < expression.length) {
          value += expression[i + 1]!
          i += 2
        } else {
          value += expression[i]!
          i += 1
        }
      }
      if (i < expression.length) i += 1
      tokens.push({ kind: "string", value })
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
    tokens.push({ kind: "punct", value: ch })
    i += 1
  }
  return tokens
}

type TaintValue =
  | { readonly kind: "root_input" }
  | { readonly kind: "string"; readonly value: string }
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
  if (value.kind === "union") return value.members.flatMap(stringLiteralsOf)
  return []
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
  const tokens = tokenizeZen(expression)
  let pos = 0
  let found = false

  const peek = (offset = 0): ZenToken | undefined => tokens[pos + offset]
  const eat = (): ZenToken | undefined => {
    const token = tokens[pos]
    if (token !== undefined) pos += 1
    return token
  }
  const isPunct = (token: ZenToken | undefined, value: string): boolean => token?.kind === "punct" && token.value === value
  const isIdent = (token: ZenToken | undefined, value: string): boolean => token?.kind === "ident" && token.value === value

  const parseExpr = (): TaintValue => parseTernary()

  const parseTernary = (): TaintValue => {
    const cond = parseNullishOr()
    if (isPunct(peek(), "?")) {
      eat()
      const consequent = parseExpr()
      if (isPunct(peek(), ":")) {
        eat()
        const alternate = parseExpr()
        return unionTaint(consequent, alternate)
      }
      return consequent
    }
    return cond
  }

  const parseNullishOr = (): TaintValue => {
    let left = parseAnd()
    while (true) {
      const t0 = peek()
      const t1 = peek(1)
      if ((isPunct(t0, "?") && isPunct(t1, "?")) || (isPunct(t0, "|") && isPunct(t1, "|"))) {
        eat()
        eat()
        left = unionTaint(left, parseAnd())
      } else if (isIdent(t0, "or")) {
        eat()
        left = unionTaint(left, parseAnd())
      } else {
        break
      }
    }
    return left
  }

  const parseAnd = (): TaintValue => {
    let left = parseComparison()
    while (true) {
      const t0 = peek()
      const t1 = peek(1)
      if (isPunct(t0, "&") && isPunct(t1, "&")) {
        eat()
        eat()
        left = unionTaint(left, parseComparison())
      } else if (isIdent(t0, "and")) {
        eat()
        left = unionTaint(left, parseComparison())
      } else {
        break
      }
    }
    return left
  }

  const parseComparison = (): TaintValue => {
    let left = parseAdditive()
    while (true) {
      const t0 = peek()
      const t1 = peek(1)
      if (
        (isPunct(t0, "=") && isPunct(t1, "=")) ||
        (isPunct(t0, "!") && isPunct(t1, "=")) ||
        (isPunct(t0, "<") && isPunct(t1, "=")) ||
        (isPunct(t0, ">") && isPunct(t1, "="))
      ) {
        eat()
        eat()
        if (isPunct(peek(), "=")) eat()
        parseAdditive()
        left = OTHER
      } else if (isPunct(t0, "<") || isPunct(t0, ">") || isIdent(t0, "in")) {
        eat()
        parseAdditive()
        left = OTHER
      } else if (isIdent(t0, "not") && isIdent(t1, "in")) {
        eat()
        eat()
        parseAdditive()
        left = OTHER
      } else {
        break
      }
    }
    return left
  }

  const parseAdditive = (): TaintValue => {
    let left = parseMultiplicative()
    while (isPunct(peek(), "+") || isPunct(peek(), "-")) {
      const op = eat()!
      const right = parseMultiplicative()
      if (op.value === "+" && left.kind === "string" && right.kind === "string") {
        left = { kind: "string", value: left.value + right.value }
      } else {
        left = OTHER
      }
    }
    return left
  }

  const parseMultiplicative = (): TaintValue => {
    let left = parseUnary()
    while (isPunct(peek(), "*") || isPunct(peek(), "/") || isPunct(peek(), "%")) {
      eat()
      parseUnary()
      left = OTHER
    }
    return left
  }

  const parseUnary = (): TaintValue => {
    const t0 = peek()
    if (isPunct(t0, "!") || isPunct(t0, "-") || isPunct(t0, "+") || isIdent(t0, "not")) {
      eat()
      parseUnary()
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
      const inner = parseExpr()
      if (isPunct(peek(), ")")) eat()
      return inner
    }
    if (isPunct(t0, "[")) {
      eat()
      const elements: TaintValue[] = []
      while (pos < tokens.length && !isPunct(peek(), "]")) {
        elements.push(parseExpr())
        if (isPunct(peek(), ",")) eat()
        else break
      }
      if (isPunct(peek(), "]")) eat()
      return { kind: "array", elements }
    }
    if (isPunct(t0, "{")) {
      eat()
      const fields: Record<string, TaintValue> = Object.create(null) as Record<string, TaintValue>
      while (pos < tokens.length && !isPunct(peek(), "}")) {
        const keyToken = peek()
        if (keyToken === undefined) break
        let keyName: string | null = null
        if (keyToken.kind === "ident" || keyToken.kind === "string") {
          keyName = keyToken.value
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
    if (t0.kind === "string") {
      eat()
      return { kind: "string", value: t0.value }
    }
    if (t0.kind === "ident") {
      eat()
      return t0.value === "input" ? ROOT_INPUT : OTHER
    }
    eat()
    return OTHER
  }

  while (pos < tokens.length && !found) {
    const before = pos
    parseExpr()
    if (pos === before) pos += 1
  }
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
