// expect: NORN-FLOW-SCHEMA-PROJECTION
// Exact decimals are EntityIR property types; closed schemas have no exact decimal yet.
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Input = Schema.Struct({ amount: Norn.decimal(2) })
