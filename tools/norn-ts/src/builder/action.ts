/**
 * Closure-free ActionIR builder (T7-TS-2, #2690): the lowering target of
 * `Norn.action`. Mirrors `ActionPayload` and the closed
 * `ActionEffectProgram` enum in `crates/norn-ir`; input and output schemas are
 * closed value schemas (from Effect Schema, `src/compiler/schema.ts`, or
 * written with `S`). The Rust checker owns every consistency rule (the effect
 * target, declared transitions, write sets, preconditions).
 */
import type { Json } from "../canonical.ts"
import { type BuildDiagnostic, Diagnostics, type SourceLocation, located } from "./diagnostics.ts"
import type { ObjectTypeDef } from "./entity.ts"
import { type EnvelopeOptions, type DependencyPin, envelope, pinnedDependencies } from "./envelope.ts"
import type { InputGroup, Labels, ObjectSchema } from "./schema.ts"
import type { PredicateWindow, SliceDecl } from "./rule.ts"
import type { Localized } from "./labels.ts"

/** Fold function for a bounded action aggregate (`AggregateFunction`). */
export type AggregateFunction =
  | { readonly kind: "sum"; readonly property: string }
  | { readonly kind: "count" }

/** One bounded fenced aggregate input read by action preconditions (`ActionAggregateInput`). */
export interface ActionAggregateInput {
  readonly name: string
  readonly objectType: string
  readonly filter?: Readonly<Record<string, string>>
  readonly window?: PredicateWindow
  readonly aggregate: AggregateFunction
  readonly maxItems: number
}

/** Trusted row-level conditions and derivations for one owned collection (`OwnedCollectionAction`). */
export interface OwnedCollectionAction {
  readonly id: string
  readonly preconditions?: ReadonlyArray<string>
  readonly derive?: Readonly<Record<string, string>>
}

/** One ordered, deterministic ZEN transformation of the action input. */
export interface ActionInputDerivation {
  readonly field: string
  readonly expression: string
  /** Caller must supply this field before derivation; defaults to true. */
  readonly inputRequired?: boolean
}

/** Typed proposal link; an explicit source must be written in the same closure. */
export interface ActionCreateLink {
  readonly source?: { readonly objectType: string; readonly idField: string }
  readonly link: string
  readonly targetType: string
  readonly targetIdField: string
}

export type WriteChange = "created" | "modified" | "deleted"

/** A connector command the action enqueues (ERPNext write-back through perform, #3028). */
export interface ActionCommand {
  readonly connector: string
  readonly operation: "submit" | "create" | "update" | "cancel"
  readonly doctype: string
  /** ZEN Expression naming the target record. */
  readonly name?: string
  /** Target field → ZEN Expression. */
  readonly fields?: Readonly<Record<string, string>>
}

/** The object an effect writes, pinned by its entity artifact identity. */
interface EffectTarget {
  readonly objectIdField: string
  readonly objectKind: string
  readonly entityArtifactId: string
  readonly entityArtifactVersion: string
}

/** `ActionEffectProgram`: the closed, declarative authority-bearing operations. */
export type ActionEffect =
  | ({ readonly kind: "upsert_input_object"; readonly writeMode?: UpsertWriteMode } & EffectTarget)
  | ({ readonly kind: "fire_transition"; readonly lifecycle: string; readonly transition: string; readonly acceptReceipt?: ReceiptAcceptance; readonly setFields?: Readonly<Record<string, string>> } & EffectTarget)
  | ({
      readonly kind: "mutate_parent_with_rows"
      readonly parentValuesField: string
      readonly rowOpsField: string
      readonly parentVersionField: string
      readonly parentDeleteField?: string
      readonly collections?: ReadonlyArray<OwnedCollectionAction>
    } & EffectTarget)

/** Host-resolved Function evidence; result values never come from action JSON. */
export interface ReceiptAcceptance {
  readonly referenceField: string
  readonly manifestDigest: string
  readonly digestProperty: string
  readonly resultBindings: Readonly<Record<string, string>>
}

export type UpsertWriteMode =
  | { readonly kind: "upsert" }
  | { readonly kind: "create_only" }
  /** The target must exist; it is patched, never created (declare only a `modified` write). */
  | { readonly kind: "update_only" }
  | { readonly kind: "draft_only"; readonly lifecycle: string; readonly state: string }

export interface ActionDef extends Localized {
  readonly dependencies?: ReadonlyArray<DependencyPin>
  readonly _tag: "Action"
  /** `actionId`; the artifact is `<id>.action` in the id's first namespace segment. */
  readonly id: string
  readonly title: string
  readonly version: string
  readonly description?: string
  readonly scopeRequired: ReadonlyArray<string>
  readonly idempotent: boolean
  readonly ledgered: boolean
  readonly ledgerCategory?: string
  readonly input: ObjectSchema
  readonly output: ObjectSchema
  /** D66: display-only labels for top-level input fields. */
  readonly inputLabels?: Readonly<Record<string, Labels>>
  /** D66: wizard steps over the input, emitted as `inputSchema["x-norn"].groups`. */
  readonly inputGroups?: ReadonlyArray<InputGroup>
  /** Ordered read slice, with property keys rooted at `from: "input"`. */
  readonly slice: ReadonlyArray<SliceDecl>
  /** Bounded fenced aggregate inputs over stored objects. */
  readonly aggregates?: ReadonlyArray<ActionAggregateInput>
  /** Ordered ZEN expressions applied before input validation and preconditions. */
  readonly derive: ReadonlyArray<ActionInputDerivation>
  /** Boolean ZEN Expressions over `input` and the declared `slice`. */
  readonly preconditions: ReadonlyArray<string>
  readonly commands: ReadonlyArray<ActionCommand>
  readonly synchronousRule?: { readonly artifactId: string; readonly version: string }
  readonly createLinks?: ReadonlyArray<ActionCreateLink>
  readonly writes: ReadonlyArray<{ readonly entityKind: string; readonly change: WriteChange }>
  readonly effect?: ActionEffect
  /** ActionIR `access`: the policy artifact that authorizes calls. */
  readonly access?: { readonly artifactId: string; readonly version?: string }
  /**
   * ACTOR-INDEPENDENT-1: protected actor properties of the effect target whose
   * recorded principal must differ from the verified principal calling.
   */
  readonly independentOf?: ReadonlyArray<string>
  /** The compensating action's id. */
  readonly compensation?: string
  readonly at?: SourceLocation
}

type At = { readonly at?: SourceLocation | undefined }

export function action(
  meta: Omit<ActionDef, "_tag" | "at" | "slice" | "derive" | "preconditions" | "commands" | "writes"> & {
    readonly slice?: ReadonlyArray<SliceDecl>
    readonly derive?: ReadonlyArray<ActionInputDerivation>
    readonly preconditions?: ReadonlyArray<string>
    readonly commands?: ReadonlyArray<ActionCommand>
    readonly writes?: ReadonlyArray<{ readonly entityKind: string; readonly change: WriteChange }>
  } & At,
): ActionDef {
  const { at, slice = [], derive = [], preconditions = [], commands = [], writes = [], ...rest } = meta
  return located({ _tag: "Action" as const, ...rest, slice: [...slice], derive: [...derive], preconditions: [...preconditions], commands: [...commands], writes: [...writes] }, at)
}

/** The pinned target of an effect on objects of `type`. */
export function target(type: Pick<ObjectTypeDef, "name" | "version">, objectIdField: string): EffectTarget {
  return { objectIdField, objectKind: type.name, entityArtifactId: `${type.name}.entity`, entityArtifactVersion: type.version }
}

/** Fire one declared transition of `lifecycle` on the object the input names. */
export function fireTransition(
  type: Pick<ObjectTypeDef, "name" | "version">,
  objectIdField: string,
  lifecycle: string,
  transition: string,
  acceptReceipt?: ReceiptAcceptance,
  setFields?: Readonly<Record<string, string>>,
): ActionEffect {
  return {
    kind: "fire_transition",
    ...target(type, objectIdField),
    lifecycle,
    transition,
    ...(acceptReceipt ? { acceptReceipt } : {}),
    ...(setFields && Object.keys(setFields).length > 0 ? { setFields: { ...setFields } } : {}),
  }
}

/** Upsert the object the input describes. */
export function upsertInputObject(
  type: Pick<ObjectTypeDef, "name" | "version">,
  objectIdField: string,
  writeMode?: UpsertWriteMode,
): ActionEffect {
  return { kind: "upsert_input_object", ...target(type, objectIdField), ...(writeMode ? { writeMode } : {}) }
}

/** Atomically mutate a parent object and its declared owned child collections. */
export function mutateParentWithRows(
  type: Pick<ObjectTypeDef, "name" | "version">,
  objectIdField: string,
  options: {
    readonly parentValuesField: string
    readonly rowOpsField: string
    readonly parentVersionField: string
    readonly parentDeleteField?: string
    readonly collections?: ReadonlyArray<OwnedCollectionAction>
  },
): ActionEffect {
  const collections = (options.collections ?? []).map((collection) => ({
    id: collection.id,
    ...(collection.preconditions && collection.preconditions.length > 0 ? { preconditions: [...collection.preconditions] } : {}),
    ...(collection.derive && Object.keys(collection.derive).length > 0 ? { derive: { ...collection.derive } } : {}),
  }))
  return {
    kind: "mutate_parent_with_rows",
    ...target(type, objectIdField),
    parentValuesField: options.parentValuesField,
    rowOpsField: options.rowOpsField,
    parentVersionField: options.parentVersionField,
    ...(options.parentDeleteField !== undefined ? { parentDeleteField: options.parentDeleteField } : {}),
    ...(collections.length > 0 ? { collections } : {}),
  }
}

export type ActionBuildResult =
  | { readonly ok: true; readonly artifact: Json }
  | { readonly ok: false; readonly diagnostics: ReadonlyArray<BuildDiagnostic> }

const ACTION_ID = /^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$/
const omitEmpty = <T>(list: ReadonlyArray<T>): ReadonlyArray<T> | undefined => (list.length === 0 ? undefined : list)

/**
 * The input schema with its D66 display data: each labelled field's title
 * and description, and the groups on the root. The checker owns every rule
 * over them (bounds, pointers, uniqueness); nothing here judges them.
 */
function labelledInput(def: ActionDef): ObjectSchema {
  const labels = def.inputLabels ?? {}
  const properties = Object.fromEntries(
    Object.entries(def.input.properties).map(([field, schema]) => [field, { ...schema, ...(labels[field] ?? {}) }]),
  )
  const groups = def.inputGroups ?? []
  return { ...def.input, properties, ...(groups.length > 0 ? { "x-norn": { groups: groups.map((group) => ({ ...group, fields: [...group.fields] })) } } : {}) }
}

/** Lower one action to its `action` artifact (`<id>.action`). */
export function lowerAction(def: ActionDef, options: EnvelopeOptions): ActionBuildResult {
  const diagnostics = new Diagnostics()
  if (!ACTION_ID.test(def.id)) diagnostics.report("NORN-FLOW-LITERAL", `action id ${JSON.stringify(def.id)} must be dotted lower-case segments (commerce.sales_order.approve)`, def.at)
  if (!diagnostics.empty) return { ok: false, diagnostics: diagnostics.items }
  const payload = {
    actionId: def.id,
    title: def.title,
    description: def.description,
    scopeRequired: def.scopeRequired,
    idempotent: def.idempotent,
    ledgered: def.ledgered,
    ledgerCategory: def.ledgerCategory,
    inputSchema: labelledInput(def),
    outputSchema: def.output,
    slice: omitEmpty(def.slice.map(({ optional, ...entry }) => (optional === true ? { ...entry, optional } : entry))),
    aggregates: omitEmpty(
      (def.aggregates ?? []).map(({ filter, window, ...entry }) => ({
        ...entry,
        ...(filter && Object.keys(filter).length > 0 ? { filter } : {}),
        ...(window ? { window } : {}),
      })),
    ),
    derive: omitEmpty(def.derive.map(({ inputRequired, ...entry }) => (inputRequired === false ? { ...entry, inputRequired } : entry))),
    preconditions: omitEmpty(def.preconditions),
    commands: omitEmpty(def.commands),
    createLinks: omitEmpty(def.createLinks ?? []),
    writes: omitEmpty(def.writes),
    effectProgram: def.effect,
    synchronousRule: def.synchronousRule,
    compensation: def.compensation === undefined ? undefined : { actionId: def.compensation },
    access: def.access,
    independentOf: omitEmpty(def.independentOf ?? []),
  }
  const artifact = envelope(
    { kind: "action", dependencies: [...pinnedDependencies(def.dependencies, "entity"), ...(def.synchronousRule ? [{ ...def.synchronousRule, kind: "rule", required: true }] : [])], artifactId: `${def.id}.action`, namespace: def.id.slice(0, def.id.indexOf(".")), name: def.title, version: def.version },
    payload,
    options,
  )
  return { ok: true, artifact }
}
