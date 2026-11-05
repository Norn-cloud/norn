import type { Bucket } from "./answer.ts"

type Member = Bucket["members"][number]

/** One bucket whose answer changed between two reads, with the records behind the change. */
export interface Restated {
  readonly key: string
  readonly label: string
  /** Absent when the bucket only exists after the correction. */
  readonly published: Bucket | undefined
  /** Absent when the correction removed the bucket's last record. */
  readonly corrected: Bucket | undefined
  /** The same bucket read back at the published answer's knowledge cutoff. */
  readonly asOf: Bucket | undefined
  /** Records counted at a different version, or counted in only one of the two reads. */
  readonly records: readonly { readonly id: string; readonly published?: Member; readonly corrected?: Member }[]
}

/** The same value and the same operands, compared as the server's exact text. */
export const sameAnswer = (left: Bucket, right: Bucket): boolean =>
  left.value === right.value && left.totals.join("\0") === right.totals.join("\0")

/**
 * Every bucket the correction touched: its answer differs between the published
 * and corrected reads, only one of them has it, or a record behind it changed
 * version with the answer unchanged. In published then corrected order.
 */
export function restated(published: readonly Bucket[], corrected: readonly Bucket[], asOf: readonly Bucket[]): Restated[] {
  const keys = [...new Set([...published, ...corrected].map(({ key }) => key))]
  return keys.flatMap((key) => {
    const before = published.find((bucket) => bucket.key === key)
    const after = corrected.find((bucket) => bucket.key === key)
    const ids = [...new Set([...before?.members ?? [], ...after?.members ?? []].map(({ id }) => id))]
    const records = ids.flatMap((id) => {
      const was = before?.members.find((member) => member.id === id)
      const is = after?.members.find((member) => member.id === id)
      return was !== undefined && was.version === is?.version ? [] : [{ id, published: was, corrected: is }]
    })
    // A correction that only changes record versions is still a correction.
    if (before !== undefined && after !== undefined && sameAnswer(before, after) && records.length === 0) return []
    const label = (after ?? before)?.label ?? key
    return [{ key, label, published: before, corrected: after, asOf: asOf.find((bucket) => bucket.key === key), records }]
  })
}
