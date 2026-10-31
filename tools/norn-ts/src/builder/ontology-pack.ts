/** Declarative ontology-pack authoring (norn#56); the Rust ontology checker remains the admission authority. */
import type { Json } from "../canonical.ts"
import { envelope, type EnvelopeOptions } from "./envelope.ts"
import type { SourceLocation } from "./diagnostics.ts"

/** One source-manifest entry: pinned release, licence, redistribution, import mode, importer and checksums. */
export interface OntologySourceSpec {
  readonly sourceId: string
  readonly releaseId: string
  readonly releasedAt: string
  readonly retrievedAt: string
  readonly canonicalUrl: string
  readonly license: string
  readonly redistribution: "reference_only" | "vendored"
  readonly importMode: "import" | "map_only" | "study_only"
  readonly importer?: { readonly name: string; readonly version: string }
  readonly files?: ReadonlyArray<{ readonly path: string; readonly sha256: string }>
}
export interface OntologyPackSpec {
  /** `norn.ontology-pack.*`; also the artifact id. */
  readonly id: string
  readonly title: string
  readonly version: string
  readonly sources: ReadonlyArray<OntologySourceSpec>
  readonly terms: ReadonlyArray<{
    readonly termId: string; readonly label: string; readonly definition: string
    readonly termKind: "class" | "property" | "value_object" | "concept_scheme_root"
    readonly status: "active" | "deprecated" | "superseded"; readonly semanticAuthority: "semantic_only"
    readonly parentTermId?: string; readonly supersededBy?: string
  }>
  readonly mappings: ReadonlyArray<{
    readonly mappingId: string; readonly termId: string; readonly sourceId: string; readonly externalIri: string
    readonly relation: "exact_match" | "close_match" | "broad_match" | "narrow_match" | "related_match"
    readonly justification: string; readonly evidenceRefs: ReadonlyArray<string>; readonly confidenceBasisPoints: number
    readonly reviewedBy: string; readonly reviewedAt: string
    readonly supersededByMappingId?: string; readonly ratifiedBy?: string; readonly ratifiedAt?: string
  }>
  readonly shapes: ReadonlyArray<{
    readonly shapeId: string; readonly termId: string; readonly closed: true
    readonly fields: ReadonlyArray<{
      readonly fieldId: string
      readonly valueType: "string" | "boolean" | "integer" | "decimal" | "date_time" | "iri" | "term_ref" | "external_identifier" | "category_ref"
      readonly targetTermId?: string; readonly minCount: number; readonly maxCount: number
    }>
  }>
}
export interface OntologyPackDef extends OntologyPackSpec { readonly _tag: "OntologyPack"; readonly at?: SourceLocation }
export function ontologyPack(meta: OntologyPackSpec & { readonly at?: SourceLocation }): OntologyPackDef { return { _tag: "OntologyPack", ...meta } }
export function lowerOntologyPack(def: OntologyPackDef, options: EnvelopeOptions): { readonly ok: true; readonly artifact: Json } {
  return { ok: true, artifact: envelope({ kind: "business_pack", artifactId: def.id, namespace: "norn.ontology", name: def.title, version: def.version }, {
    packId: def.id, packVersion: "0.1", packKind: "ontology",
    sources: def.sources, terms: def.terms, mappings: def.mappings, shapes: def.shapes,
  }, options) }
}
