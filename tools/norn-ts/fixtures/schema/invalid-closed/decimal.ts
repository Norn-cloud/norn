// expect: NORN-FLOW-SCHEMA-PROJECTION
// Closed exact sites refuse a scale above the bounded arithmetic profile.
import { Schema } from "effect"
import { Norn } from "@norn/authoring"

export const Input = Schema.Struct({ amount: Norn.decimal(19) })
