// expect: NORN-FLOW-SCHEMA-PROJECTION
// Closed schemas have no unions: an explicit null is not approximated as an absent member.
import { Schema } from "effect"

export const Input = Schema.Struct({ note: Schema.NullOr(Schema.String) })
