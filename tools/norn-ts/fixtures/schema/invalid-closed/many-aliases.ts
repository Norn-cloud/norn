// expect: NORN-FLOW-UNBOUNDED
// Expansion is reserved cumulatively: each export of this ~30k-node shared schema is within the
// per-schema bound, but five of them exceed the batch budget before any is materialized.
import { Schema } from "effect"

const S0 = Schema.Struct({ v: Schema.String })
const S1 = Schema.Struct({ a: S0, b: S0, c: S0 })
const S2 = Schema.Struct({ a: S1, b: S1, c: S1 })
const S3 = Schema.Struct({ a: S2, b: S2, c: S2 })
const S4 = Schema.Struct({ a: S3, b: S3, c: S3 })
const S5 = Schema.Struct({ a: S4, b: S4, c: S4 })
const S6 = Schema.Struct({ a: S5, b: S5, c: S5 })
const S7 = Schema.Struct({ a: S6, b: S6, c: S6 })
const S8 = Schema.Struct({ a: S7, b: S7, c: S7 })
const S9 = Schema.Struct({ a: S8, b: S8, c: S8 })

export const Input1 = S9
export const Input2 = S9
export const Input3 = S9
export const Input4 = S9
export const Input5 = S9
