/**
 * Closure-free EntityIR v2 builder (T7-EFX-4 #2787, T7-TS-2 #2690).
 *
 * The lowering target of `Norn.type` / `Norn.interfaceType` and of the
 * Effect Schema projection (`src/compiler/schema.ts`): plain data that
 * mirrors `crates/norn-ir/src/entity_v2`. A hand-written definition uses
 * these constructors directly; the syntax compiler emits the same data, so
 * both produce identical artifacts. Nothing here is a second type authority:
 * the builder checks only what it needs to build an envelope (the qualified
 * name), and every consistency rule (enum states, keys, defaults, lifecycle
 * reachability, interface conformance) is the Rust checker's.
 *
 * Wire rules carried here: numeric defaults are exact decimal strings
 * (never JSON numbers, OPEN-DECISIONS D37); `nullable: false` and empty lists
 * are omitted (the canonical form); an absent optional member is omitted,
 * never `null` (#2819).
 */
import type { Json } from "../canonical.ts"
import { type BuildDiagnostic, Diagnostics, type SourceLocation, located } from "./diagnostics.ts"
import { type EnvelopeOptions, envelope } from "./envelope.ts"

// ------------------------------------------------------------ value kinds

export type TextFormat = "email" | "uri" | "uuid"
export type TimezonePolicy = "utc" | "offset"
export interface UnitRef {
  readonly system: "ucum" | "qudt"
  readonly code: string
}
/** The authoritative currency of a monetary decimal: a property path on the holding object. */
export interface CurrencySource {
  readonly links: ReadonlyArray<string>
  readonly property: string
}
export interface EnumValue {
  readonly value: string
  readonly description?: string
}

/** One of the ten EntityIR v2 value kinds (`PropertyType`). */
export type PropertyType =
  | { readonly kind: "text"; readonly format?: TextFormat }
  | { readonly kind: "integer" }
  | { readonly kind: "decimal"; readonly scale: number; readonly unit?: UnitRef; readonly currencySource?: CurrencySource }
  | { readonly kind: "boolean" }
  | { readonly kind: "timestamp"; readonly timezone: TimezonePolicy }
  | { readonly kind: "date" }
  | { readonly kind: "duration" }
  | { readonly kind: "enum"; readonly values: ReadonlyArray<EnumValue> }
  | { readonly kind: "ref"; readonly name: string }

export const text = (format?: TextFormat): PropertyType => (format === undefined ? { kind: "text" } : { kind: "text", format })
export const integer: PropertyType = { kind: "integer" }
export const decimal = (scale: number, unit?: UnitRef): PropertyType =>
  unit === undefined ? { kind: "decimal", scale } : { kind: "decimal", scale, unit: { system: unit.system, code: unit.code } }
/** An exact money amount whose currency is the named property (`money(2, "currency")`), or a linked one. */
export const money = (scale: number, currency: string | CurrencySource): PropertyType => ({
  kind: "decimal",
  scale,
  currencySource: typeof currency === "string" ? { links: [], property: currency } : { links: [...currency.links], property: currency.property },
})
export const boolean: PropertyType = { kind: "boolean" }
export const timestamp = (timezone: TimezonePolicy): PropertyType => ({ kind: "timestamp", timezone })
export const date: PropertyType = { kind: "date" }
export const duration: PropertyType = { kind: "duration" }
export const enumeration = (...values: ReadonlyArray<string | EnumValue>): PropertyType => ({
  kind: "enum",
  values: values.map((v) => (typeof v === "string" ? { value: v } : { ...v })),
})
/** A shared property type by name: a local `propertyTypes` entry or a registry value type (`iso4217.currency_code`). */
export const ref = (name: string): PropertyType => ({ kind: "ref", name })

// -------------------------------------------------------------- members

/**
 * A literal default. Numbers are carried as their exact decimal text
 * (`"19.990"`), so a default never passes through a binary float.
 */
export type DefaultValue = string | boolean | { readonly decimal: string }

export interface PropertyDecl {
  readonly id: string
  readonly description?: string
  readonly type: PropertyType
  readonly nullable?: boolean
  readonly markings?: ReadonlyArray<string>
  readonly default?: DefaultValue
  readonly at?: SourceLocation
}

export interface KeyDecl {
  readonly name: string
  readonly properties: ReadonlyArray<string>
}

export interface TransitionDecl {
  readonly id: string
  readonly description?: string
  readonly from: ReadonlyArray<string>
  readonly to: string
  /** ZEN Expression guards (conjoined). */
  readonly guards?: ReadonlyArray<string>
}

export interface LifecycleDecl {
  readonly property: string
  readonly description?: string
  readonly initial: ReadonlyArray<string>
  readonly terminal?: ReadonlyArray<string>
  readonly transitions: ReadonlyArray<TransitionDecl>
}

export type Multiplicity = "one" | "many"

/** A link to another object type (EntityIR v2 relations are links on the source type). */
export interface LinkDecl {
  readonly id: string
  readonly description?: string
  /** The target object type's qualified name. */
  readonly target: string
  readonly cardinality: { readonly to: Multiplicity; readonly from: Multiplicity; readonly required?: boolean }
  /** The inverse role's id on the target. */
  readonly inverse?: string
  /** Reject cycles within this declaring relation, including inherited members. */
  readonly acyclic?: boolean
}

/** A link requirement on an interface: the widest target and cardinality an implementation may have. */
export type InterfaceLinkDecl = Pick<LinkDecl, "id" | "description" | "target" | "cardinality">

/** A boolean ZEN Expression required of every object type implementing an interface. */
export interface InvariantDecl {
  readonly classification?: "integrity" | "quality"
  readonly id: string
  readonly description?: string
  readonly expression: string
  readonly message: string
}

interface TypeMetaBase {
  readonly markings?: ReadonlyArray<string>
  /** Qualified name: dotted lower-case domain namespace and a PascalCase type name (`commerce.SalesOrder`). */
  readonly name: string
  /** Human-readable title (the envelope `name`). */
  readonly title: string
  readonly version: string
  readonly description?: string
  readonly properties: ReadonlyArray<PropertyDecl>
}

export interface ObjectTypeDef extends TypeMetaBase {
  readonly _tag: "ObjectType"
  readonly extends?: string
  readonly implements: ReadonlyArray<string>
  readonly links: ReadonlyArray<LinkDecl>
  readonly identity: ReadonlyArray<KeyDecl>
  readonly lifecycles: ReadonlyArray<LifecycleDecl>
  readonly at?: SourceLocation
}

export interface InterfaceDef extends TypeMetaBase {
  readonly _tag: "Interface"
  readonly extends: ReadonlyArray<string>
  /** Links every implementing type must declare, at most this wide. */
  readonly links: ReadonlyArray<InterfaceLinkDecl>
  readonly invariants: ReadonlyArray<InvariantDecl>
  readonly at?: SourceLocation
}

export type EntityDef = ObjectTypeDef | InterfaceDef

/** Additive tenant members; the installed type and its identity stay owned by its package. */
export interface ExtensionDef {
  readonly id: string
  readonly namespace: string
  readonly target: string
  readonly title: string
  readonly version: string
  readonly description?: string
  readonly properties?: ReadonlyArray<PropertyDecl>
  readonly links?: ReadonlyArray<LinkDecl>
}

export function extension(meta: ExtensionDef): ExtensionDef { return meta }

/** Lower only declared additions; the Rust checker enforces namespace, shadowing and target rules. */
export function lowerExtension(def: ExtensionDef, options: EnvelopeOptions): EntityBuildResult {
  return { ok: true, artifact: envelope(
    { kind: "entity", artifactId: def.id, namespace: def.namespace, name: def.title, version: def.version },
    { entityVersion: "2.0", objectTypeExtension: {
      namespace: def.namespace, target: def.target, description: def.description,
      properties: omitEmpty((def.properties ?? []).map((p) => propertyJson(p, true))),
      links: omitEmpty(def.links ?? []),
    } }, options,
  ) }
}

type At = { readonly at?: SourceLocation | undefined }


export function property(id: string, type: PropertyType, options: Omit<PropertyDecl, "id" | "type"> = {}): PropertyDecl {
  return { id, type, ...options }
}

export function objectType(
  meta: TypeMetaBase & {
    readonly extends?: string | ObjectTypeDef
    readonly implements?: ReadonlyArray<string | InterfaceDef>
    readonly links?: ReadonlyArray<LinkDecl>
    readonly identity?: ReadonlyArray<KeyDecl>
    readonly lifecycles?: ReadonlyArray<LifecycleDecl>
  } & At,
): ObjectTypeDef {
  const { at, extends: supertype, implements: implemented = [], links = [], identity = [], lifecycles = [], ...rest } = meta
  return located(
    {
      _tag: "ObjectType" as const,
      ...rest,
      ...(supertype === undefined ? {} : { extends: typeof supertype === "string" ? supertype : supertype.name }),
      implements: implemented.map((i) => (typeof i === "string" ? i : i.name)),
      links: [...links],
      identity: [...identity],
      lifecycles: [...lifecycles],
    },
    at,
  )
}

export function interfaceType(
  meta: TypeMetaBase & {
    readonly extends?: ReadonlyArray<string | InterfaceDef>
    readonly links?: ReadonlyArray<InterfaceLinkDecl>
    readonly invariants?: ReadonlyArray<InvariantDecl>
  } & At,
): InterfaceDef {
  const { at, extends: parents = [], links = [], invariants = [], ...rest } = meta
  return located({ _tag: "Interface" as const, ...rest, extends: parents.map((parent) => typeof parent === "string" ? parent : parent.name), links: [...links], invariants: [...invariants] }, at)
}

// ------------------------------------------------------------- lowering

/** EntityIR v2 object type name grammar (`OBJECT_TYPE_NAME_PATTERN`). */
export const OBJECT_TYPE_NAME = /^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)*\.[A-Z][A-Za-z0-9]*$/

export type EntityBuildResult =
  | { readonly ok: true; readonly artifact: Json }
  | { readonly ok: false; readonly diagnostics: ReadonlyArray<BuildDiagnostic> }

const omitEmpty = <T>(list: ReadonlyArray<T>): ReadonlyArray<T> | undefined => (list.length === 0 ? undefined : list)

function defaultJson(value: DefaultValue): Json {
  return typeof value === "object" ? value.decimal : value
}

function propertyJson(p: PropertyDecl, withDefault: boolean): Json {
  return {
    id: p.id,
    markings: omitEmpty(p.markings ?? []),
    description: p.description,
    type: p.type as unknown as Json,
    constraints: p.nullable === true ? { nullable: true } : undefined,
    default: withDefault && p.default !== undefined ? defaultJson(p.default) : undefined,
  } as unknown as Json
}

function cardinalityJson(c: LinkDecl["cardinality"]): Json {
  return { to: c.to, from: c.from, required: c.required === true ? true : undefined } as unknown as Json
}

/** Lower one object type or interface to its `entity` artifact (`<name>.entity` / `<name>.interface`). */
export function lowerEntity(def: EntityDef, options: EnvelopeOptions): EntityBuildResult {
  const diagnostics = new Diagnostics()
  if (!OBJECT_TYPE_NAME.test(def.name)) {
    diagnostics.report("NORN-FLOW-LITERAL", `type name ${JSON.stringify(def.name)} must be a dotted lower-case namespace and a PascalCase name (commerce.SalesOrder)`, def.at)
  }
  if (def._tag === "Interface") {
    for (const p of def.properties) {
      if (p.default !== undefined) diagnostics.report("NORN-FLOW-LITERAL", `interface property ${p.id} cannot declare a default`, p.at ?? def.at)
    }
  }
  if (!diagnostics.empty) return { ok: false, diagnostics: diagnostics.items }
  const namespace = def.name.slice(0, def.name.lastIndexOf("."))
  const payload =
    def._tag === "Interface"
      ? {
          entityVersion: "2.0",
          interface: {
            name: def.name,
            extends: omitEmpty(def.extends),
            markings: omitEmpty(def.markings ?? []),
            description: def.description,
            properties: omitEmpty(def.properties.map((p) => propertyJson(p, false))),
            links: omitEmpty(def.links.map((l) => ({ ...l, cardinality: cardinalityJson(l.cardinality) }))),
            invariants: omitEmpty(def.invariants.map(({ classification, ...invariant }) => ({
              ...invariant,
              ...(classification === undefined || classification === "integrity" ? {} : { classification }),
            }))),
          },
        }
      : {
          entityVersion: "2.0",
          objectType: {
            name: def.name,
            markings: omitEmpty(def.markings ?? []),
            description: def.description,
            extends: def.extends,
            implements: omitEmpty(def.implements),
            properties: omitEmpty(def.properties.map((p) => propertyJson(p, true))),
            links: omitEmpty(
              def.links.map((l) => ({
                ...l,
                acyclic: l.acyclic === true ? true : undefined,
                cardinality: cardinalityJson(l.cardinality),
              })),
            ),
            identity: omitEmpty(def.identity),
            lifecycles: omitEmpty(
              def.lifecycles.map((l) => ({
                ...l,
                terminal: omitEmpty(l.terminal ?? []),
                transitions: omitEmpty(l.transitions.map((t) => ({ ...t, guards: omitEmpty(t.guards ?? []) }))),
              })),
            ),
          },
        }
  const artifact = envelope(
    {
      kind: "entity",
      artifactId: `${def.name}.${def._tag === "Interface" ? "interface" : "entity"}`,
      namespace,
      name: def.title,
      version: def.version,
    },
    payload,
    options,
  )
  return { ok: true, artifact }
}

/** Existing EntityIR v2 marking catalog, shared by runtime read projections. */
export interface MarkingDecl {
  readonly id: string
  readonly label: string
  readonly severity: "low" | "moderate" | "high" | "critical"
  readonly category: string
  readonly mask: { readonly kind: "redacted" | "null" } | { readonly kind: "last-n"; readonly count: number }
}
export interface MarkingsDef {
  readonly _tag: "Markings"
  readonly id: string
  readonly title: string
  readonly version: string
  readonly markings: ReadonlyArray<MarkingDecl>
  readonly at?: SourceLocation
}
export function markings(meta: Omit<MarkingsDef, "_tag">): MarkingsDef {
  return { _tag: "Markings", ...meta }
}
export function lowerMarkings(def: MarkingsDef, options: EnvelopeOptions): EntityBuildResult {
  return { ok: true, artifact: envelope({ kind: "entity", artifactId: def.id, namespace: def.id.split(".")[0]!, name: def.title, version: def.version }, { entityVersion: "2.0", markings: def.markings }, options) }
}
