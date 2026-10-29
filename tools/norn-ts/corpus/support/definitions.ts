// Top-level definitions and data consts shared by the value-provenance fixtures.
import { Effect, Schema } from "effect"
import { Tool } from "effect/unstable/ai"
import { Norn } from "@norn/authoring"

export const Shape = Schema.Struct({ id: Schema.String })

export const lookup = Tool.make("lookup", { parameters: Shape, success: Shape })

export const otherFlow = Norn.flow(
  { id: "other", version: "1" },
  Effect.gen(function* () {
    return { ok: true }
  }),
)

export const sink = Norn.fn<unknown, { readonly ok: boolean }>({ id: "sink", version: "1" })

export const limits = { maxAttempts: 3, floor: -2, labels: ["a", "b"], none: null } as const

export const wrapped = { shape: Shape }

export const shared = { code: "x", weight: 2 } as const

export const twice = { first: shared, second: shared, list: [shared, shared] }
