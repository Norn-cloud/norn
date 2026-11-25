/**
 * The registry's shared value types (`registry/core/artifacts/{core,std}.value_types.entity.json`)
 * that a closed schema can read: what a `Norn.ref` resolves to when a law, guard,
 * precondition or action input compares or computes with it. The checker still
 * resolves every `ref` against the registry it admits; this table only types
 * expressions in the authoring shell, and `test/authoring-shell.test.ts` keeps it
 * equal to the committed artifacts. A text `format` is dropped: an expression
 * reads the text.
 */
import type { PropertyType } from "../builder/entity.ts"

export const REGISTRY_VALUE_TYPES: Readonly<Record<string, PropertyType>> = {
  "core.display_name": {"kind": "text"},
  "core.code": {"kind": "text"},
  "core.document_number": {"kind": "text"},
  "core.email_address": {"kind": "text"},
  "core.phone_number": {"kind": "text"},
  "core.money_amount": {"kind": "decimal", "scale": 6},
  "core.quantity": {"kind": "decimal", "scale": 6},
  "core.national_id": {"kind": "text"},
  "core.bank_account_number": {"kind": "text"},
  "iso8601.date": {"kind": "date"},
  "iso8601.timestamp": {"kind": "timestamp", "timezone": "utc"},
  "ucum.unit_code": {"kind": "text"},
  "qudt.unit_id": {"kind": "text"},
  "gs1.gtin": {"kind": "text"},
  "gs1.gln": {"kind": "text"},
  "lei.lei": {"kind": "text"},
  "unlocode.location_code": {"kind": "text"},
  "isco08.occupation_code": {"kind": "text"},
  "esco.occupation_uri": {"kind": "text"},
}

/** The value type `name` resolves to, or `undefined` for a local or unknown one. */
export function registryValueType(name: string): PropertyType | undefined {
  return Object.hasOwn(REGISTRY_VALUE_TYPES, name) ? REGISTRY_VALUE_TYPES[name] : undefined
}
