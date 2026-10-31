/** Declarative link-pack authoring (norn#56): checked correspondences between two ontology packs. */
import type { Json } from "../canonical.ts"
import { envelope, type EnvelopeOptions } from "./envelope.ts"
import type { SourceLocation } from "./diagnostics.ts"

export interface PackLinkSpec {
  /** `norn.link.*`; also the artifact id. */
  readonly id: string
  readonly title: string
  readonly version: string
  readonly fromPack: { readonly packId: string; readonly version: string }
  readonly toPack: { readonly packId: string; readonly version: string }
  readonly correspondences: ReadonlyArray<{
    readonly correspondenceId: string; readonly fromTermId: string
    readonly relation: "equivalent" | "subsumed_by" | "subsumes" | "derived"
    readonly toTermIds: ReadonlyArray<string>; readonly justification: string
    readonly evidenceRefs: ReadonlyArray<string>; readonly confidenceBasisPoints: number
    readonly reviewedBy: string; readonly reviewedAt: string
    readonly transformRef?: string; readonly rulingRef?: string
  }>
  readonly residuals?: ReadonlyArray<{ readonly termId: string; readonly reason: "system_specific" | "canon_gap" | "non_conforming"; readonly note: string }>
}
export interface PackLinkDef extends PackLinkSpec { readonly _tag: "PackLink"; readonly at?: SourceLocation }
export function packLink(meta: PackLinkSpec & { readonly at?: SourceLocation }): PackLinkDef { return { _tag: "PackLink", ...meta } }
export function lowerPackLink(def: PackLinkDef, options: EnvelopeOptions): { readonly ok: true; readonly artifact: Json } {
  return { ok: true, artifact: envelope({ kind: "business_pack", artifactId: def.id, namespace: "norn.ontology", name: def.title, version: def.version }, {
    packId: def.id, packVersion: "0.1", packKind: "link", fromPack: def.fromPack, toPack: def.toPack,
    correspondences: def.correspondences, ...(def.residuals?.length ? { residuals: def.residuals } : {}),
  }, options) }
}
