// T7-EFX-4 (#2787): Effect Schema syntax -> the checker's closed value schema
// (Function I/O, action input/output, RuleIR contracts). Golden: closed.schemas.json.
// Unknown keys: every record is closed (additionalProperties: false). Missing vs
// null: optionalKey drops the member from `required`; NullOr has no closed form.
import { Schema, pipe } from "effect"
import { String as Text } from "effect/Schema"

export const Line = Schema.Struct({
  sku: Schema.String.pipe(Schema.brand("Sku")),
  quantity: Schema.Int,
  ratio: Schema.Finite,
  note: Schema.optionalKey(Schema.String),
  kind: Schema.Literals(["goods", "service"]),
  tags: Schema.Array(Schema.String),
})

export const Shipped = Schema.TaggedStruct("Shipped", { at: Schema.String })

export const Channel = Schema.Union([Schema.Literal("web"), Schema.Literal("pos")])

// A const alias exports the same schema; pipe(...) is the same as .pipe(...);
// a shorthand member reads its binding.
export const LineAlias = Line
export const Code = pipe(Schema.String, Schema.brand("Code"))
const sku = Schema.String
export const Short = Schema.Struct({ sku })
// A bare imported Schema value.
export const Plain = Text
