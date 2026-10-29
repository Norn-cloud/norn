/**
 * Closure-free MappingIR builder (`norn.mapping/1`): the lowering target of
 * `Norn.mapping`. Mirrors `MappingIr` in `crates/norn-ir/src/mapping.rs`: a
 * pinned source-schema binding, the interfaces the mapping reports, and one
 * object map per logical source dataset. Every transform (`subject`,
 * `sourceRecord`, property values, link keys, lifecycle states/transitions)
 * is ZEN Expression text over `input`, typed by the Rust checker against the
 * bound source schema; `onDelete` names the lifecycle state an observed
 * delete lands in.
 *
 * The source binding's `sha256` is the envelope hash of the bound
 * `source_schema` artifact as the Rust checker computes it (`ArtifactRef`);
 * it is a pin the author writes, never recomputed here, and the checker
 * refuses a mismatch (`SOURCE_HASH_MISMATCH`).
 */
import type { Json } from "../canonical.ts"
import { type BuildDiagnostic, Diagnostics, type SourceLocation, located } from "./diagnostics.ts"
import { type EnvelopeOptions, envelope } from "./envelope.ts"

export interface SourceBinding {
  readonly schemaId: string
  readonly version: string
  readonly sha256: string
}

/** A pinned entity artifact (object type or interface) a map targets or reports. */
export interface ArtifactPin {
  readonly artifactId: string
  readonly version: string
}

export type GapReason = "not_in_source" | "defaulted" | "deferred" | "derived_elsewhere"

export interface ObjectMapDecl {
  readonly id: string
  readonly source: { readonly dataset: string; readonly nestedIn?: { readonly dataset: string; readonly field: string } }
  readonly target: { readonly entity: ArtifactPin; readonly interface: ArtifactPin }
  readonly when?: string
  readonly subject: string
  readonly sourceRecord: string
  readonly properties?: ReadonlyArray<{ readonly property: string; readonly value: string }>
  readonly links?: ReadonlyArray<{ readonly link: string; readonly targetMap: string; readonly key: string }>
  readonly lifecycles?: ReadonlyArray<{ readonly property: string; readonly state?: string; readonly transition?: string; readonly onDelete?: string }>
  readonly gaps?: ReadonlyArray<{ readonly member: string; readonly reason: GapReason; readonly note?: string }>
  readonly ignoredFields?: ReadonlyArray<{ readonly field: string; readonly reason: string }>
}

/** API terms are access evidence, independent of artifact redistribution provenance. */
export interface VendorMappingContract {
  readonly system: string
  readonly versions: readonly [string, ...string[]]
  readonly apiTerms: { readonly uri: string; readonly sha256: string }
}

export type MappingSourceContract =
  | ({ readonly kind: "vendor" } & VendorMappingContract)
  | { readonly kind: "generic" }

export interface MappingDef {
  readonly _tag: "Mapping"
  /** The artifact id (`erpnext.sales_order`); the namespace is its first segment. */
  readonly id: string
  readonly title: string
  readonly version: string
  readonly source: SourceBinding
  readonly sourceContract?: MappingSourceContract
  readonly interfaces: ReadonlyArray<ArtifactPin>
  readonly maps: ReadonlyArray<ObjectMapDecl>
  readonly at?: SourceLocation
}

export function mapping(meta: Omit<MappingDef, "_tag" | "at"> & { readonly at?: SourceLocation | undefined }): MappingDef {
  const { at, ...rest } = meta
  return located({ _tag: "Mapping" as const, ...rest, interfaces: [...rest.interfaces], maps: [...rest.maps] }, at)
}

export type MappingBuildResult =
  | { readonly ok: true; readonly artifact: Json }
  | { readonly ok: false; readonly diagnostics: ReadonlyArray<BuildDiagnostic> }

const MAPPING_ID = /^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$/
const omitEmpty = <T>(list: ReadonlyArray<T> | undefined): ReadonlyArray<T> | undefined => (list === undefined || list.length === 0 ? undefined : list)

/**
 * Lower one mapping to its `mapping` artifact. Dependencies are derived: the
 * bound source schema, then every reported interface and map target, each
 * once, in first-use order.
 */
export function lowerMapping(def: MappingDef, options: EnvelopeOptions): MappingBuildResult {
  const diagnostics = new Diagnostics()
  if (!MAPPING_ID.test(def.id)) diagnostics.report("NORN-FLOW-LITERAL", `mapping id ${JSON.stringify(def.id)} must be dotted lower-case segments (erpnext.sales_order)`, def.at)
  if (!diagnostics.empty) return { ok: false, diagnostics: diagnostics.items }
  const dependencies: Json[] = [{ artifactId: def.source.schemaId, kind: "source_schema", version: def.source.version, required: true }]
  const pinned = new Map([[def.source.schemaId, def.source.version]])
  const depend = (pin: ArtifactPin): void => {
    const known = pinned.get(pin.artifactId)
    if (known !== undefined) {
      // One artifact, one pin: deduplicating two versions would silently drop one of them.
      if (known !== pin.version) diagnostics.report("NORN-FLOW-LITERAL", `${pin.artifactId} is pinned at both ${known} and ${pin.version}; a mapping binds one version of each artifact`, def.at)
      return
    }
    pinned.set(pin.artifactId, pin.version)
    dependencies.push({ artifactId: pin.artifactId, kind: "entity", version: pin.version, required: true })
  }
  for (const pin of def.interfaces) depend(pin)
  for (const m of def.maps) {
    depend(m.target.interface)
    depend(m.target.entity)
  }
  if (!diagnostics.empty) return { ok: false, diagnostics: diagnostics.items }
  const payload = {
    format: "norn.mapping/1",
    extensions: def.sourceContract === undefined ? undefined : def.sourceContract.kind === "generic"
      ? { "norn.mapping.generic/1.0.0": { parity: "unqualified" } }
      : { "norn.mapping.vendor/1.0.0": { system: def.sourceContract.system, versions: [...def.sourceContract.versions], apiTerms: def.sourceContract.apiTerms } },
    source: { schemaId: def.source.schemaId, version: def.source.version, sha256: def.source.sha256 },
    interfaces: def.interfaces.map((pin) => pin.artifactId),
    maps: def.maps.map((m) => ({
      id: m.id,
      source: m.source,
      target: { entity: m.target.entity.artifactId, interface: m.target.interface.artifactId },
      when: m.when,
      subject: m.subject,
      sourceRecord: m.sourceRecord,
      properties: omitEmpty(m.properties),
      links: omitEmpty(m.links),
      lifecycles: omitEmpty(m.lifecycles),
      gaps: omitEmpty(m.gaps),
      ignoredFields: omitEmpty(m.ignoredFields),
    })),
  }
  const artifact = envelope(
    { kind: "mapping", artifactId: def.id, namespace: def.id.slice(0, def.id.indexOf(".")), name: def.title, version: def.version, dependencies },
    payload,
    options,
  )
  return { ok: true, artifact }
}
