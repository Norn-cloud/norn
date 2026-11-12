import { Schema } from "effect"
import { Norn } from "@norn/authoring"

/**
 * OBS-EVIDENCE-1 / DATA-INGEST: where an extracted value was read, so lineage
 * can reach past the raw observation to the exact region of the source
 * document. A source evidence object is itself an observation produced by a
 * mapping, never a write authority; a renderer shows any type implementing
 * this interface the same way, whichever pack declares it.
 *
 * The box is in page fractions (0 to 1 from the top-left corner), so it does
 * not depend on the resolution the document was scanned or rendered at.
 * `confidence_bp` is the extractor's own score in basis points; it is not
 * evidence of correctness, and null means the extractor reported none.
 */
export const SourceEvidence = Norn.interfaceType({
  name: "norn.SourceEvidence",
  title: "Source document evidence",
  version: "0.1.0",
  description: "Where one extracted value was read: the document (its URI and the SHA-256 of its exact bytes), the page and box, the text span the extractor read, and which extractor at which version read it, with its own confidence.",
  properties: Schema.Struct({
    subject_property: Schema.String,
    document_uri: Schema.String,
    document_digest: Schema.String,
    media_type: Schema.String,
    page: Schema.Int,
    bbox_x: Norn.decimal(4),
    bbox_y: Norn.decimal(4),
    bbox_w: Norn.decimal(4),
    bbox_h: Norn.decimal(4),
    text: Schema.String,
    extractor: Schema.String,
    extractor_version: Schema.String,
    confidence_bp: Schema.NullOr(Schema.Int),
  }),
})
