import type { LinkPage, ObjectRecord } from "./transport.ts"

/** The record's own name when its concept declares one, else its identity. */
export const recordName = (record: { readonly id: string; readonly values?: Readonly<Record<string, unknown>> | null }): string => {
  const values = record.values ?? undefined
  const label = values?.["label"]
  if (typeof label === "string" && label !== "") return label
  const displayName = values?.["display_name"]
  if (typeof displayName === "string" && displayName !== "") return displayName
  return record.id
}

/** Whether an object read and its link read are both about the object that was asked for. */
export const readsOf = (objectType: string, id: string, object: ObjectRecord, links: LinkPage): boolean =>
  object.objectType === objectType && object.id === id && links.objectType === objectType && links.id === id
