/**
 * The admitted Widget UX document as `<NornAppRenderer>` reads it (`WidgetRoot.ux`,
 * `crates/norn-ir/src/widget_ux.rs`). These are the members the renderer draws or refuses; the
 * checker admitted the document, so nothing here re-validates it. `test/ux-schema.test.ts` holds
 * the closed kinds below to `crates/norn-ir/schema/widget.schema.json`, so a kind the IR adds fails
 * there until the renderer draws it or refuses it (`capabilities.ts`).
 */
import type { Machine, ValueType } from "./machine/machine.ts"

/** Locale-keyed user-facing text, as older contracts carry it. */
export type LocaleText = Readonly<Record<string, string>>

/** A source message (UI-TEXT-1): its stable id, its source text and its optional context. */
export interface Message { readonly msg: { readonly id: string; readonly source: string; readonly context?: string } }

/** User-facing text: a source message, or locale-keyed text. */
export type Text = Message | LocaleText

/**
 * A host's translations: the text of message `id` in `locale`, or `undefined` when it has none (the
 * source text is drawn then). The translations themselves are the host's (a later checked artifact).
 */
export type Translate = (id: string, locale: string) => string | undefined

export interface ArtifactRef { readonly id: string; readonly version: string }
export interface PropertyRef { readonly bindingId: string; readonly propertyId: string }

export type Component =
  | { readonly kind: "section"; readonly section: { readonly sectionId: string; readonly kind: "section" | "tab"; readonly label: Text; readonly fields: readonly PropertyRef[]; readonly order: number } }
  | { readonly kind: "list"; readonly dataBindingId: string; readonly fields: readonly PropertyRef[]; readonly selection: "none" | "single" | "multiple" }
  | { readonly kind: "detail"; readonly dataBindingId: string }
  | { readonly kind: "dialog"; readonly bindingId?: string; readonly blocking: boolean; readonly dismissalMethods?: readonly string[] }
  | { readonly kind: "form"; readonly formId: string }
  | { readonly kind: "child_grid"; readonly formId: string }
  | { readonly kind: "field"; readonly fieldId: string }
  | { readonly kind: "action"; readonly placementId: string }
  | { readonly kind: "navigation"; readonly itemId: string }
  | { readonly kind: "catalog"; readonly entry: { readonly entryId: string; readonly widget?: ArtifactRef }; readonly props?: Readonly<Record<string, string>>; readonly heldIn?: readonly string[] }
  | { readonly kind: "control"; readonly label: Text; readonly input: "trigger" | "choice" | "text" }
  | { readonly kind: "composition"; readonly widget: ArtifactRef; readonly pageId: string }

export type ComponentKind = Component["kind"]

export interface WidgetNode {
  readonly nodeId: string
  readonly semanticId?: string
  readonly component: Component
  readonly behaviors?: readonly string[]
  readonly children: readonly WidgetNode[]
}

export type ViewKind = "collection" | "record" | "form" | "question" | "dashboard" | "run" | "decision_inbox" | "lineage" | "lifecycle"

export interface WidgetPage {
  readonly pageId: string
  readonly name: string
  readonly contextParams: readonly { readonly paramId: string; readonly type: ValueType; readonly required: boolean; readonly runtimeKeyOf?: ArtifactRef }[]
  readonly root: WidgetNode
  readonly view?: { readonly kind: ViewKind; readonly subject: unknown }
  readonly machine?: Machine
}

export type DataQuery =
  | { readonly kind: "object_list"; readonly filters: readonly unknown[]; readonly sort: readonly unknown[]; readonly page: { readonly size: number; readonly maxSize: number } }
  | { readonly kind: "object_detail"; readonly key: readonly unknown[] }
  | { readonly kind: "link_traversal"; readonly sourceBindingId: string; readonly linkId: string }
  | { readonly kind: "object_runtime_detail"; readonly pageId: string; readonly parameterId: string }

export interface DataBinding { readonly bindingId: string; readonly object: ArtifactRef; readonly query: DataQuery }

/** A read of the object a page's context names (UI-CONTEXT-READ-1): its type and key are two of the page's parameters. */
export interface ContextRead { readonly bindingId: string; readonly pageId: string; readonly typeParameterId: string; readonly keyParameterId: string }

export interface ActionBinding {
  readonly bindingId: string
  readonly action: ArtifactRef
  readonly inputMapping: Readonly<Record<string, string>>
  readonly inputs: { readonly recordBindingId?: string; readonly draftFormId?: string; readonly selectionBindingId?: string; readonly contextPageId?: string }
  readonly requiredCapability: string
}

export type Severity = "safe" | "confirm" | "destructive" | "irreversible"

export interface ActionPlacement {
  readonly placementId: string
  readonly bindingId: string
  readonly label: Text
  readonly confirmation: { readonly severity: Severity; readonly strength: "none" | "click" | "typed"; readonly message?: Text }
}

export interface FieldPresentation {
  readonly fieldId: string
  readonly target: { readonly kind: string; readonly property?: PropertyRef }
  readonly label: Text
}

export interface NavigationItem { readonly itemId: string; readonly label: Text }

export type Trust = "native" | "contract_checked" | "opaque_island"

export interface CatalogEntry {
  readonly entryId: string
  readonly label: Text
  readonly trust: Trust
  readonly framework?: string
  readonly props?: readonly { readonly fieldId: string; readonly type: ValueType; readonly required: boolean }[]
  readonly events?: readonly { readonly eventId: string; readonly payload?: readonly { readonly fieldId: string; readonly type: ValueType; readonly required: boolean }[] }[]
  readonly machine?: Machine
}

export interface WidgetUx {
  readonly uxVersion: "0.1"
  readonly app?: { readonly appId: string; readonly entryPageId: string }
  readonly pages: readonly WidgetPage[]
  readonly dataBindings: readonly DataBinding[]
  readonly actionBindings: readonly ActionBinding[]
  readonly fields: readonly FieldPresentation[]
  readonly forms: readonly unknown[]
  readonly actionPlacements: readonly ActionPlacement[]
  readonly navigationItems: readonly NavigationItem[]
  readonly overrides: readonly unknown[]
  readonly catalog?: readonly CatalogEntry[]
  readonly contextReads?: readonly ContextRead[]
}

/** The entry prefix that names a UI SDK primitive at the catalog's pinned version. */
export const UI_SDK_PREFIX = "ui-sdk:"

/** Whether `label` is a source message: a `msg` object holding a string `id` and `source`. */
function isMessage(label: Text): label is Message {
  const message = (label as { readonly msg?: unknown }).msg
  if (typeof message !== "object" || message === null || Array.isArray(message)) return false
  const { id, source } = message as { readonly id?: unknown; readonly source?: unknown }
  return typeof id === "string" && typeof source === "string"
}

/**
 * The text of `label` in `locale`: a message through the host's translations (`translate`), else its
 * source text; locale-keyed text in `locale`, else English, else any locale's.
 */
export function text(label: Text, locale: string, translate?: Translate): string {
  if (isMessage(label)) return translate?.(label.msg.id, locale) ?? label.msg.source
  const locales = label as LocaleText
  return locales[locale] ?? locales.en ?? Object.values(locales)[0] ?? ""
}

/** Every node of a tree, parents before children. */
export function nodesOf(root: WidgetNode): WidgetNode[] {
  return [root, ...root.children.flatMap(nodesOf)]
}
