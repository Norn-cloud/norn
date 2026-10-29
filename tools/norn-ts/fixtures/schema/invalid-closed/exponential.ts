// expect: NORN-FLOW-UNBOUNDED
// Shared definitions are counted at every use: 3^12 expanded nodes from a few lines are refused before projection.
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
const S10 = Schema.Struct({ a: S9, b: S9, c: S9 })
const S11 = Schema.Struct({ a: S10, b: S10, c: S10 })
const S12 = Schema.Struct({ a: S11, b: S11, c: S11 })

export const Input = Schema.Struct({ root: S12 })
