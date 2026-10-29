// Schema features without a qualified EntityIR projection are rejected, never approximated.
import { Schema } from "effect"

export const Positive = Schema.Finite.check(Schema.isGreaterThan(0))
export const Created = Schema.Date
export const Amount = Schema.Number
export const Parsed = Schema.NumberFromString
interface Tree {
  readonly children: ReadonlyArray<Tree>
}
export const TreeSchema: Schema.Codec<Tree> = Schema.Struct({
  children: Schema.Array(Schema.suspend((): Schema.Codec<Tree> => TreeSchema)),
})
