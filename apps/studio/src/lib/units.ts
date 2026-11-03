import type { Metric, ObjectType, Question } from "./transport.ts"

/**
 * The unit of each operand, in `totals` order, read from the declarations the
 * question was admitted with: the selected metric (or a ratio's numerator and
 * denominator), the stored property each one sums, and that property's
 * declared unit. An operand whose unit is not declared reads as `undefined`.
 */
export function operandUnits(question: Question, metrics: readonly Metric[], objectType: ObjectType): readonly (string | undefined)[] {
  const named = (name: string | null | undefined) => metrics.find((metric) => metric.name === name)
  const selected = named(question.aggregate[0]?.metric)
  if (selected === undefined) return []
  const operands = selected.type === "ratio"
    ? [named(selected.type_params.numerator), named(selected.type_params.denominator)]
    : [selected]
  return operands.map((operand) => {
    const property = objectType.properties?.find(({ id }) => id === operand?.type_params.expr)
    const type = property?.type
    return type !== undefined && "unit" in type ? type.unit?.code : undefined
  })
}

/** An exact amount beside its unit: `294 g`. */
export const measured = (amount: string, unit: string | undefined): string => unit === undefined ? amount : `${amount} ${unit}`
