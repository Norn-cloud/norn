/** Minimum declarative SourceSchemaIR authoring; Rust remains admission authority. */
import type { Json } from "../canonical.ts"
import { envelope, type EnvelopeOptions } from "./envelope.ts"
import type { SourceLocation } from "./diagnostics.ts"
export interface SourceSchemaSpec {
  readonly id: string
  readonly title: string
  readonly version: string
  readonly source: { readonly system: string; readonly instance: string; readonly version?: string }
  readonly observedAt: string
  readonly adapter: { readonly name: string; readonly version: string }
  readonly rawMetadata: { readonly uri: string; readonly sha256: string }
  readonly datasets: ReadonlyArray<{ readonly id: string; readonly nativeName: string; readonly namespace?: string; readonly kind: "table" | "view" | "sheet" | "file" | "stream" | "document" | "child_document" }>
  readonly fields: ReadonlyArray<{
    readonly dataset: string; readonly name: string; readonly ordinal: number; readonly nativeType: string
    readonly typeCandidate: { readonly type: "string" | "text" | "integer" | "decimal" | "boolean" | "date" | "datetime" | "time" | "duration" | "json" | "binary" | "reference" | "unknown"; readonly origin: "declared" | "inferred" }
    readonly required?: boolean; readonly markings?: ReadonlyArray<string>
  }>
  /** Only declared sets narrow mapping inputs; candidates remain evidence. */
  readonly codeSets?: ReadonlyArray<{
    readonly id: string; readonly dataset: string; readonly field: string
    readonly origin: "declared" | "observed_candidate"
    readonly values: ReadonlyArray<{ readonly value: string; readonly label?: string }>
  }>
  /** Declared constraints; a `parent_child` one names the parent's collection field a nested map reads. */
  readonly constraints?: ReadonlyArray<{
    readonly id: string; readonly kind: "primary_key" | "unique" | "foreign_key" | "logical_reference" | "parent_child"
    readonly dataset: string; readonly fields: ReadonlyArray<string>
    readonly target?: { readonly dataset: string; readonly fields: ReadonlyArray<string> }
    readonly collection?: string; readonly origin: "declared" | "inferred"
  }>
}
export interface SourceSchemaDef extends SourceSchemaSpec { readonly _tag: "SourceSchema"; readonly at?: SourceLocation }
export function sourceSchema(meta: SourceSchemaSpec & { readonly at?: SourceLocation }): SourceSchemaDef { return { _tag: "SourceSchema", ...meta } }
export function lowerSourceSchema(def: SourceSchemaDef, options: EnvelopeOptions): { readonly ok: true; readonly artifact: Json } {
  return { ok: true, artifact: envelope({ kind: "source_schema", artifactId: def.id, namespace: def.id.split(".")[0]!, name: def.title, version: def.version }, {
    format: "norn.source-schema/1", source: def.source, observedAt: def.observedAt, adapter: def.adapter, rawMetadata: def.rawMetadata,
    ...(def.datasets.length ? { datasets: def.datasets } : {}), ...(def.fields.length ? { fields: def.fields } : {}),
    ...(def.codeSets?.length ? { codeSets: def.codeSets } : {}),
    ...(def.constraints?.length ? { constraints: def.constraints } : {}),
  }, options) }
}
