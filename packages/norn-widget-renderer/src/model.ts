import type { Schemas } from "../../norn-sdk/src/index.ts"
import { asArray, asString, isRecord, type LosslessNumber } from "./json"

type Exact<T> = T extends number ? number | LosslessNumber
  : T extends object ? { [K in keyof T]: Exact<T[K]> } : T

/** The generated WidgetIR UX contract with lossless numeric metadata. */
export type Ux = Exact<Schemas["NornIrWidgetUx"]>
export type UxPage = Exact<Schemas["NornIrWidgetPage"]>
export type UxNode = Exact<Schemas["NornIrWidgetNode"]>
export type UxComponent = Exact<Schemas["NornIrWidgetComponent"]>
export type UxDataBinding = Exact<Schemas["NornIrWidgetDataBinding"]>
export type UxField = Exact<Schemas["NornIrWidgetFieldPresentation"]>
export type UxForm = Exact<Schemas["NornIrWidgetForm"]>
export type UxPlacement = Exact<Schemas["NornIrWidgetActionPlacement"]>
export type UxNavigationItem = Exact<Schemas["NornIrWidgetNavigationItem"]>
export type UxActionBinding = Exact<Schemas["NornIrWidgetUxActionBinding"]>
export type UxPropertyRef = Exact<Schemas["NornIrPropertyRef"]>
export type UxCondition = Exact<Schemas["NornIrZenCondition"]>
export type LocaleText = Exact<Schemas["NornIrLocaleText"]>

/** The admitted artifact fields used by the renderer; The host's richer view model is structurally compatible. */
export interface WidgetArtifact {
  readonly artifactId: string
  readonly kind: string
  readonly version: string
  readonly name?: string | null
  readonly layer?: string
  readonly payload: Record<string, unknown>
}

export interface WidgetDefinitionSet {
  readonly artifacts: readonly WidgetArtifact[]
  /** The admitted definitions revision, when the host knows it; part of every binding's cache identity. */
  readonly version?: number
}

/** One installed UX app: the widget artifact that declares it and its contract. */
export interface InstalledApp {
  readonly artifact: WidgetArtifact
  readonly ux: Ux
  readonly appId: string
  readonly name: string
}

/** Every admitted widget that declares an app. Widgets without `ux` are legacy and not apps. */
export function installedApps(set: WidgetDefinitionSet): InstalledApp[] {
  return set.artifacts.flatMap((a) => {
    if (a.kind !== "widget" || !isRecord(a.payload.ux)) return []
    const ux = a.payload.ux as unknown as Ux
    const appId = ux.app?.appId
    return appId ? [{ artifact: a, ux, appId, name: a.name ?? appId }] : []
  })
}

/** Text for the operator's locale, falling back to English, then to any declared locale. */
export function text(value: LocaleText | undefined, locale = typeof navigator === "undefined" ? "en" : navigator.language): string {
  if (!value) return ""
  const lang = locale.split("-")[0] ?? "en"
  return value[locale] ?? value[lang] ?? value.en ?? Object.values(value)[0] ?? ""
}

/** The object type an artifact reference names (`crm.Customer.entity@0.1.0` → `crm.Customer`), if installed. */
export function objectTypeOf(set: WidgetDefinitionSet, ref: { id: string; version: string }): string | null {
  const artifact = set.artifacts.find((a) => a.artifactId === ref.id && a.version === ref.version)
  if (!artifact || artifact.kind !== "entity") return null
  const objectType = artifact.payload.objectType
  if (isRecord(objectType)) return asString(objectType.name) ?? artifact.artifactId
  // EntityIR v1 represents the object type directly as objectKind.
  return asString(artifact.payload.objectKind) ?? null
}


/** The action id an action artifact reference declares, if installed. */
export function actionIdOf(set: WidgetDefinitionSet, ref: { id: string; version: string }): string | null {
  const artifact = set.artifacts.find((a) => a.kind === "action" && a.artifactId === ref.id && a.version === ref.version)
  return artifact ? (asString(artifact.payload.actionId) ?? null) : null
}

/** A direct input path: a named member of a source, or the record's runtime key (`input.recordKey`). */
export type DirectPath = { readonly namespace: "draft" | "record" | "selection" | "context"; readonly name: string } | { readonly namespace: "recordKey" | "recordVersion" }

/**
 * A pure input path such as `input.draft.orderId` or `input.recordKey`. Mappings that are anything
 * more than a path are ZEN expressions, which only Norn evaluates, so they resolve to `null`.
 */
export function directPath(expression: string): DirectPath | null {
  if (/^\s*input\.recordKey\s*$/.test(expression)) return { namespace: "recordKey" }
  if (/^\s*input\.recordVersion\s*$/.test(expression)) return { namespace: "recordVersion" }
  const m = /^\s*input\.(draft|record|selection|context)\.([A-Za-z_][A-Za-z0-9_]*)\s*$/.exec(expression)
  return m?.[1] && m[2] ? { namespace: m[1] as "draft" | "record" | "selection" | "context", name: m[2] } : null
}

export function byId<T>(items: readonly T[], key: (t: T) => string): Map<string, T> {
  return new Map(items.map((t) => [key(t), t]))
}

/** Node children in rendering order: sections sort among the section slots; everything else keeps its place. */
export function orderedChildren(node: UxNode): UxNode[] {
  const children = [...node.children]
  const slots = children.flatMap((c, i) => (c.component.kind === "section" ? [i] : []))
  const sections = slots.map((i) => children[i] as UxNode).sort((a, b) => sectionOrder(a) - sectionOrder(b))
  slots.forEach((slot, j) => {
    children[slot] = sections[j] as UxNode
  })
  return children
}

function sectionOrder(node: UxNode): number {
  return node.component.kind === "section" ? Number(node.component.section.order) : 0
}

/** Data bindings a page's tree reads, in first-use order. */
export function pageBindings(page: UxPage, ux: Ux): UxDataBinding[] {
  const ids: string[] = []
  const fields = byId(ux.fields, (field) => field.fieldId)
  const navigationItems = byId(ux.navigationItems, (item) => item.itemId)
  const addBinding = (id: string | undefined) => {
    if (id && !ids.includes(id)) ids.push(id)
  }
  const addFieldConditionBindings = (field: UxField) => {
    addBinding(field.optionQueryBindingId)
    for (const condition of [field.visibleWhen, field.requiredWhen, field.readOnlyWhen]) {
      if (!condition) continue
      addBinding(condition.inputs.recordBindingId)
      addBinding(condition.inputs.selectionBindingId)
    }
  }
  const visit = (n: UxNode) => {
    const c = n.component
    if (c.kind === "list" || c.kind === "detail") addBinding(c.dataBindingId)
    if (c.kind === "form") {
      const form = ux.forms.find((form) => form.formId === c.formId)
      if (form?.mode === "edit") addBinding(form.dataBindingId)
    }
    if (c.kind === "field") {
      const field = fields.get(c.fieldId)
      if (field) addFieldConditionBindings(field)
    }
    if (c.kind === "navigation") {
      const condition = navigationItems.get(c.itemId)?.visibleWhen
      addBinding(condition?.inputs.recordBindingId)
      addBinding(condition?.inputs.selectionBindingId)
    }
    if (c.kind === "child_grid") {
      addBinding(c.dataBindingId)
      const form = ux.forms.find((form) => form.formId === c.formId)
      if (form?.mode === "edit") addBinding(form.dataBindingId)
      for (const fieldId of c.fieldIds) {
        const field = fields.get(fieldId)
        if (field) addFieldConditionBindings(field)
      }
    }
    n.children.forEach(visit)
  }
  visit(page.root)
  // Dependencies a binding reads (a traversal's source, record filter values) are resolved by the cell.
  const all = byId(ux.dataBindings, (b) => b.bindingId)
  return ids.flatMap((id) => (all.get(id) ? [all.get(id) as UxDataBinding] : []))
}

export function asLocale(value: unknown): LocaleText | undefined {
  return isRecord(value) ? (value as LocaleText) : undefined
}

export { asArray }
