/**
 * Widget UX authoring (`WidgetRoot.ux`, UI-MACHINE-1, #1762): a page, its component tree and its
 * interaction machine written as typed TypeScript and lowered to a `widget` artifact the Rust
 * checker admits. The types follow `crates/norn-ir/src/widget_ux.rs`; UI SDK primitives, their
 * options and their callbacks are typed from the checker's own pinned catalog
 * (`crates/norn-ui-checker/catalog/ui-sdk.json`), never copied by hand, so a UI SDK bump that
 * renames an option fails here at compile time. The builder grants nothing: the server decides
 * every call a machine names, and the checker proves the machine's laws (L1–L6).
 */
import type { Json } from "../canonical.ts"
import { UI_SDK } from "../../generated/ui-sdk.ts"
import { type EnvelopeOptions, envelope } from "./envelope.ts"
import type { LocaleText } from "./labels.ts"

type Catalog = typeof UI_SDK.components
/** A UI SDK primitive at the version the checker's catalog pins (`ui-sdk:<component>`). */
export type UiSdkComponent = keyof Catalog & string
/** The options of `C` a page may bind: every option that is not a callback or a structure. */
export type UiSdkProp<C extends UiSdkComponent> = Catalog[C]["props"][number]
/** The callbacks of `C`, which a page raises as `user` events that name the node (`emits`). */
export type UiSdkCallback<C extends UiSdkComponent> = Catalog[C]["callbacks"][number]

/** The pinned UI SDK version the checker's catalog names. */
export const UI_SDK_VERSION: string = UI_SDK.version

export type ValueType = "text" | "integer" | "number" | "boolean" | "date" | "timestamp" | "duration" | "json"
export type CallOutcome =
  | "accepted" | "refused" | "pending_approval" | "answer_moved" | "network_failure" | "not_served"
  | "stale" | "subscription_refused" | "timeout" | "cancelled"
export type RefusalCategory = "visibility" | "permission" | "precondition" | "conflict" | "invalid" | "limit" | "internal"
export type StateRole = "idle" | "pending" | "error" | "terminal"
/** A ZEN expression over `input.fields` and `input.event`. */
export type Zen = string

export interface ArtifactRef { readonly id: string; readonly version: string }
export interface PropertyRef { readonly bindingId: string; readonly propertyId: string }

export interface MachineField {
  readonly fieldId: string
  readonly type: ValueType
  readonly required: boolean
  readonly initial?: Json
}

export type EventSource =
  | { readonly kind: "page_opened" }
  | { readonly kind: "user"; readonly nodeId: string; readonly emits?: string }
  | { readonly kind: "outcome"; readonly outcome: CallOutcome; readonly category?: RefusalCategory; readonly code?: string }
  | { readonly kind: "subscription"; readonly bindingId: string }
  | { readonly kind: "timer"; readonly afterMs: number }

export interface MachineEvent { readonly eventId: string; readonly source: EventSource; readonly payload?: ReadonlyArray<MachineField> }

export type MachineCall =
  | { readonly kind: "action"; readonly bindingId: string }
  | { readonly kind: "read"; readonly bindingId: string }
  | { readonly kind: "operation"; readonly operationId: string }

export interface MachineState {
  readonly stateId: string
  readonly role: StateRole
  readonly awaits?: MachineCall
  readonly deadlineMs?: number
}

export type MachineEffect =
  | { readonly kind: "start"; readonly call: MachineCall; readonly input?: Readonly<Record<string, Zen>> }
  | { readonly kind: "cancel" }
  | { readonly kind: "navigate"; readonly itemId: string }

export interface MachineTransition {
  readonly transitionId: string
  readonly from: string
  readonly on: string
  readonly target: string
  readonly guard?: Zen
  readonly assign?: Readonly<Record<string, Zen>>
  readonly effects?: ReadonlyArray<MachineEffect>
}

export interface Machine {
  readonly initialStateId: string
  readonly fields?: ReadonlyArray<MachineField>
  readonly events: ReadonlyArray<MachineEvent>
  readonly states: ReadonlyArray<MachineState>
  readonly transitions: ReadonlyArray<MachineTransition>
}

export type Component =
  | { readonly kind: "section"; readonly section: { readonly sectionId: string; readonly kind: "section" | "tab"; readonly label: LocaleText; readonly fields: ReadonlyArray<PropertyRef>; readonly order: number } }
  | { readonly kind: "list"; readonly dataBindingId: string; readonly fields: ReadonlyArray<PropertyRef>; readonly selection: "none" | "single" | "multiple" }
  | { readonly kind: "detail"; readonly dataBindingId: string }
  | { readonly kind: "dialog"; readonly bindingId?: string; readonly blocking: boolean; readonly dismissalMethods?: ReadonlyArray<"visible_close" | "scrim" | "swipe" | "escape"> }
  | { readonly kind: "form"; readonly formId: string }
  | { readonly kind: "field"; readonly fieldId: string }
  | { readonly kind: "action"; readonly placementId: string }
  | { readonly kind: "navigation"; readonly itemId: string }
  | { readonly kind: "control"; readonly label: LocaleText; readonly input: "trigger" | "choice" | "text" }
  | { readonly kind: "catalog"; readonly entry: { readonly entryId: string; readonly widget?: ArtifactRef }; readonly props?: Readonly<Record<string, Zen>>; readonly heldIn?: ReadonlyArray<string> }

export interface WidgetNode {
  readonly nodeId: string
  readonly semanticId?: string
  readonly component: Component
  /** UI SDK foundation behaviours (`<area>/<module>`), checked against the pinned catalog. */
  readonly behaviors?: ReadonlyArray<string>
  readonly children?: ReadonlyArray<WidgetNode>
}

/** A UI SDK primitive placed on a page: its bindable options as ZEN, and the states it is held open in. */
export function uiSdk<C extends UiSdkComponent>(
  nodeId: string,
  component: C,
  options: { readonly props?: Partial<Record<UiSdkProp<C>, Zen>>; readonly heldIn?: ReadonlyArray<string>; readonly behaviors?: ReadonlyArray<string>; readonly children?: ReadonlyArray<WidgetNode> } = {},
): WidgetNode {
  return {
    nodeId,
    component: {
      kind: "catalog",
      entry: { entryId: `ui-sdk:${component}` },
      ...(options.props === undefined ? {} : { props: options.props as Readonly<Record<string, Zen>> }),
      ...(options.heldIn === undefined ? {} : { heldIn: options.heldIn }),
    },
    ...(options.behaviors === undefined ? {} : { behaviors: options.behaviors }),
    ...(options.children === undefined ? {} : { children: options.children }),
  }
}

/** A person's event from a UI SDK node's callback, typed by the primitive's own callbacks. */
export function fromUiSdk<C extends UiSdkComponent>(eventId: string, nodeId: string, _component: C, callback: UiSdkCallback<C>): MachineEvent {
  return { eventId, source: { kind: "user", nodeId, emits: callback } }
}

/** An outcome event; `code` refines a refusal with a code the bound action declares. */
export function outcome(eventId: string, value: CallOutcome, refine: { readonly category?: RefusalCategory; readonly code?: string } = {}): MachineEvent {
  return { eventId, source: { kind: "outcome", outcome: value, ...refine } }
}

export interface Page {
  readonly pageId: string
  readonly name: string
  readonly root: WidgetNode
  readonly machine?: Machine
  readonly view?: Json
}

export interface ActionBinding {
  readonly bindingId: string
  readonly action: ArtifactRef
  readonly inputMapping?: Readonly<Record<string, Zen>>
  readonly requiredCapability: string
}

export interface WidgetDef {
  readonly _tag: "Widget"
  readonly id: string
  readonly title: string
  readonly version: string
  readonly pages: ReadonlyArray<Page>
  readonly actionBindings?: ReadonlyArray<ActionBinding>
  /** The rest of the contract, as the Rust checker reads it (`dataBindings`, `fields`, `forms`, …). */
  readonly contract?: Readonly<{
    dataBindings?: ReadonlyArray<Json>
    fields?: ReadonlyArray<Json>
    forms?: ReadonlyArray<Json>
    actionPlacements?: ReadonlyArray<Json>
    navigationItems?: ReadonlyArray<Json>
    overrides?: ReadonlyArray<Json>
    catalog?: ReadonlyArray<Json>
  }>
  /** Exact artifacts the contract names (entities and actions), as required dependencies. */
  readonly dependencies?: ReadonlyArray<{ readonly artifactId: string; readonly version: string; readonly kind: "entity" | "action" | "widget" }>
}

export function widget(def: Omit<WidgetDef, "_tag">): WidgetDef {
  return { _tag: "Widget", ...def }
}

const node = (value: WidgetNode): Json => ({ ...value, children: (value.children ?? []).map(node) }) as unknown as Json

/** Lower a widget definition to its `widget` artifact envelope. */
export function lowerWidget(def: WidgetDef, options: EnvelopeOptions): Json {
  const contract = def.contract ?? {}
  const ux = {
    uxVersion: "0.1",
    pages: def.pages.map((page) => ({
      pageId: page.pageId,
      name: page.name,
      contextParams: [],
      root: node(page.root),
      ...(page.view === undefined ? {} : { view: page.view }),
      ...(page.machine === undefined ? {} : { machine: { machineVersion: "0.1", fields: [], ...page.machine } }),
    })),
    dataBindings: contract.dataBindings ?? [],
    actionBindings: (def.actionBindings ?? []).map((binding) => ({ inputMapping: {}, inputs: {}, ...binding })),
    fields: contract.fields ?? [],
    forms: contract.forms ?? [],
    actionPlacements: contract.actionPlacements ?? [],
    navigationItems: contract.navigationItems ?? [],
    overrides: contract.overrides ?? [],
    ...(contract.catalog === undefined ? {} : { catalog: contract.catalog }),
  }
  return envelope(
    {
      kind: "widget",
      artifactId: def.id,
      namespace: def.id.split(".")[0] ?? def.id,
      name: def.title,
      version: def.version,
      dependencies: (def.dependencies ?? []).map((pin) => ({ ...pin, required: true })),
    },
    { widgetVersion: "0.1", widgetId: def.id, dataDependencies: [], viewStates: [{ stateId: "ready", kind: "ready" }], actionBindings: [], permissions: [], ux },
    options,
  )
}
