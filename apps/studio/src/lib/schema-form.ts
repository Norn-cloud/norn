/**
 * The fields of a "run once" form, read from a tool's input JSON Schema (#772). Rendering only:
 * the Worker and the gateway validate every call, so a field this cannot describe falls back to
 * JSON text rather than guessing.
 */

export type FieldKind = "text" | "enum" | "integer" | "number" | "boolean" | "date" | "datetime" | "json"

export interface FormField {
  readonly name: string
  readonly label: string
  readonly kind: FieldKind
  readonly required: boolean
  readonly description: string | null
  /** For `enum`. */
  readonly options: readonly string[]
}

type Schema = Record<string, unknown>
const isSchema = (value: unknown): value is Schema => typeof value === "object" && value !== null && !Array.isArray(value)

/** A field's own schema with a nullable union (`["string", "null"]` or `anyOf` with null) unwrapped. */
function unwrap(schema: Schema): Schema {
  const type = schema.type
  if (Array.isArray(type)) {
    const rest = type.filter((name) => name !== "null")
    return rest.length === 1 ? { ...schema, type: rest[0] } : schema
  }
  const members = Array.isArray(schema.anyOf) ? schema.anyOf.filter(isSchema) : []
  const nonNull = members.filter((member) => member.type !== "null")
  return nonNull.length === 1 && nonNull[0] !== undefined && members.length === 2 ? nonNull[0] : schema
}

function kindOf(schema: Schema): { kind: FieldKind; options: readonly string[] } {
  const values = Array.isArray(schema.enum) ? schema.enum : schema.const !== undefined ? [schema.const] : null
  if (values !== null) return values.every((value) => typeof value === "string") ? { kind: "enum", options: values as string[] } : { kind: "json", options: [] }
  switch (schema.type) {
    case "string": return { kind: schema.format === "date" ? "date" : schema.format === "date-time" ? "datetime" : "text", options: [] }
    case "integer": return { kind: "integer", options: [] }
    case "number": return { kind: "number", options: [] }
    case "boolean": return { kind: "boolean", options: [] }
    default: return { kind: "json", options: [] }
  }
}

/** The top-level fields of an object input schema, required first; `null` when it is not one. */
export function formFields(inputSchema: unknown): readonly FormField[] | null {
  if (!isSchema(inputSchema) || inputSchema.type !== "object") return null
  const properties = isSchema(inputSchema.properties) ? inputSchema.properties : {}
  const required = new Set(Array.isArray(inputSchema.required) ? inputSchema.required.filter((name) => typeof name === "string") : [])
  return Object.entries(properties)
    .filter((entry): entry is [string, Schema] => isSchema(entry[1]))
    .map(([name, raw]) => {
      const schema = unwrap(raw)
      return { name, label: typeof schema.title === "string" ? schema.title : name, required: required.has(name), description: typeof schema.description === "string" ? schema.description : null, ...kindOf(schema) }
    })
    .sort((a, b) => Number(b.required) - Number(a.required) || a.name.localeCompare(b.name))
}

/** What a field's text becomes in the call's input; `undefined` leaves an empty field out. */
export type Parsed = { readonly ok: true; readonly value: unknown } | { readonly ok: false; readonly error: string }

export function parseField(field: FormField, text: string): Parsed {
  if (text.trim() === "") return field.required ? { ok: false, error: `${field.label} is required.` } : { ok: true, value: undefined }
  switch (field.kind) {
    case "integer": return /^-?\d+$/.test(text.trim()) ? { ok: true, value: Number(text) } : { ok: false, error: `${field.label} must be a whole number.` }
    case "number": return Number.isFinite(Number(text)) ? { ok: true, value: Number(text) } : { ok: false, error: `${field.label} must be a number.` }
    case "boolean": return text === "true" || text === "false" ? { ok: true, value: text === "true" } : { ok: false, error: `${field.label} must be true or false.` }
    case "enum": return field.options.includes(text) ? { ok: true, value: text } : { ok: false, error: `${field.label} must be one of ${field.options.join(", ")}.` }
    case "datetime": return Number.isNaN(Date.parse(text)) ? { ok: false, error: `${field.label} must be a date and time.` } : { ok: true, value: new Date(text).toISOString() }
    case "json":
      try {
        return { ok: true, value: JSON.parse(text) as unknown }
      } catch {
        return { ok: false, error: `${field.label} must be JSON.` }
      }
    default: return { ok: true, value: text }
  }
}

/** The call input from every field's text, or the first field's error. */
export function buildInput(fields: readonly FormField[], texts: Readonly<Record<string, string>>): Parsed {
  const input: Record<string, unknown> = {}
  for (const field of fields) {
    const parsed = parseField(field, texts[field.name] ?? "")
    if (!parsed.ok) return parsed
    if (parsed.value !== undefined) input[field.name] = parsed.value
  }
  return { ok: true, value: input }
}
