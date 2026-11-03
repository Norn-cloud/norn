import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const ObservedLinkClaim = Norn.type({
  name: "norn.ObservedLinkClaim",
  title: "Observed relation claim",
  version: "0.1.0",
  description: "OBS-LINK-CLAIM-1: an immutable source claim about a canonical relation; existence is evidence, never authored-write authority. MappingIR declares the relation and its endpoints. Resolution and quarantine are projections over these claims.",
  properties: Schema.Struct({
    observation_id: Schema.String,
    source_type: Schema.String,
    source_key: Schema.String,
    role: Schema.String,
    target_type: Schema.NullOr(Schema.String),
    target_key: Schema.NullOr(Schema.String),
    evidence: Schema.String,
  }),
})
