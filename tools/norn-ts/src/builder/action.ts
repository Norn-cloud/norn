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
import { type EnvelopeOptions, envelope } from "./envelope.ts"
import type { ObjectSchema } from "./schema.ts"
import type { SliceDecl } from "./rule.ts"

/** One ordered, deterministic ZEN transformation of the action input. */
export interface ActionInputDerivation {
  readonly field: string
  readonly expression: string
  /** Caller must supply this field before derivation; defaults to true. */
  readonly inputRequired?: boolean
}

/** Outgoing link to an existing, authorized object loaded by the action's slice. */
export interface ActionCreateLink {
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
  | ({ readonly kind: "fire_transition"; readonly lifecycle: string; readonly transition: string } & EffectTarget)

export type UpsertWriteMode =
  | { readonly kind: "upsert" }
  | { readonly kind: "create_only" }
  | { readonly kind: "draft_only"; readonly lifecycle: string; readonly state: string }

export interface ActionDef {
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
  /** Ordered read slice, with property keys rooted at `from: "input"`. */
  readonly slice: ReadonlyArray<SliceDecl>
  /** Ordered ZEN expressions applied before input validation and preconditions. */
  readonly derive: ReadonlyArray<ActionInputDerivation>
  /** Boolean ZEN Expressions over `input` and the declared `slice`. */
  readonly preconditions: ReadonlyArray<string>
  readonly commands: ReadonlyArray<ActionCommand>
  readonly createLinks?: ReadonlyArray<ActionCreateLink>
  readonly writes: ReadonlyArray<{ readonly entityKind: string; readonly change: WriteChange }>
  readonly effect?: ActionEffect
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
export function fireTransition(type: Pick<ObjectTypeDef, "name" | "version">, objectIdField: string, lifecycle: string, transition: string): ActionEffect {
  return { kind: "fire_transition", ...target(type, objectIdField), lifecycle, transition }
}

/** Upsert the object the input describes. */
export function upsertInputObject(
  type: Pick<ObjectTypeDef, "name" | "version">,
  objectIdField: string,
  writeMode?: UpsertWriteMode,
): ActionEffect {
  return { kind: "upsert_input_object", ...target(type, objectIdField), ...(writeMode ? { writeMode } : {}) }
}

export type ActionBuildResult =
  | { readonly ok: true; readonly artifact: Json }
  | { readonly ok: false; readonly diagnostics: ReadonlyArray<BuildDiagnostic> }

const ACTION_ID = /^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$/
const omitEmpty = <T>(list: ReadonlyArray<T>): ReadonlyArray<T> | undefined => (list.length === 0 ? undefined : list)

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
    inputSchema: def.input,
    outputSchema: def.output,
    slice: omitEmpty(def.slice.map(({ optional, ...entry }) => (optional === true ? { ...entry, optional } : entry))),
    derive: omitEmpty(def.derive.map(({ inputRequired, ...entry }) => (inputRequired === false ? { ...entry, inputRequired } : entry))),
    preconditions: omitEmpty(def.preconditions),
    commands: omitEmpty(def.commands),
    createLinks: omitEmpty(def.createLinks ?? []),
    writes: omitEmpty(def.writes),
    effectProgram: def.effect,
    compensation: def.compensation === undefined ? undefined : { actionId: def.compensation },
  }
  const artifact = envelope(
    { kind: "action", artifactId: `${def.id}.action`, namespace: def.id.slice(0, def.id.indexOf(".")), name: def.title, version: def.version },
    payload,
    options,
  )
  return { ok: true, artifact }
}
