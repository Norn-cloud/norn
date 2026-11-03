import type { Bucket } from "./answer.ts"

type Member = Bucket["members"][number]

/** One bucket whose answer changed between two reads, with the records behind the change. */
export interface Restated {
  readonly published: Bucket
  readonly corrected: Bucket
  /** The same bucket read back at the published answer's knowledge cutoff. */
  readonly asOf: Bucket | undefined
  /** Records counted at a different version, or counted in only one of the two reads. */
  readonly records: readonly { readonly id: string; readonly published?: Member; readonly corrected?: Member }[]
}

/** The same value and the same operands, compared as the server's exact text. */
export const sameAnswer = (left: Bucket, right: Bucket): boolean =>
  left.value === right.value && left.totals.join("\0") === right.totals.join("\0")

/** Every bucket whose answer differs between the published and corrected reads, in bucket order. */
export function restated(published: readonly Bucket[], corrected: readonly Bucket[], asOf: readonly Bucket[]): Restated[] {
  return corrected.flatMap((after) => {
    const before = published.find(({ key }) => key === after.key)
    if (before === undefined || sameAnswer(before, after)) return []
    const ids = [...new Set([...before.members, ...after.members].map(({ id }) => id))]
    const records = ids.flatMap((id) => {
      const was = before.members.find((member) => member.id === id)
      const is = after.members.find((member) => member.id === id)
      return was?.version === is?.version ? [] : [{ id, published: was, corrected: is }]
    })
    return [{ published: before, corrected: after, asOf: asOf.find(({ key }) => key === after.key), records }]
  })
}
