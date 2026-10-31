/** Independent structural validation of literal Norn.markings modules. */
import { createHash } from "node:crypto"
import ts from "typescript"

export const MARKINGS_TRANSLATION_FRAGMENT = "norn-ts/markings-literal/1" as const
export const TRANSLATION_LIMITS = Object.freeze({ textUnits: 32_768, tokens: 4_096, nodes: 4_096, depth: 32 })

export interface TranslationContext {
  readonly orgId: string
  readonly environmentId: string
  readonly sourcePath: string
}

export type TranslationValidation =
  | { readonly status: "checked"; readonly fragment: typeof MARKINGS_TRANSLATION_FRAGMENT; readonly evidence: "structural"; readonly binding: string; readonly artifacts: 1 }
  | { readonly status: "mismatch"; readonly fragment: typeof MARKINGS_TRANSLATION_FRAGMENT; readonly evidence: "refuted"; readonly binding: string; readonly path: string }
  | { readonly status: "unsupported" | "unknown"; readonly evidence: "unknown"; readonly reason: string }

type Value = null | boolean | number | string | Value[] | { [key: string]: Value }
type RecordValue = { [key: string]: Value }
class Declined extends Error {
  constructor(readonly status: "unsupported" | "unknown", reason: string) { super(reason) }
}
function requireFragment(condition: unknown, reason: string): asserts condition {
  if (!condition) throw new Declined("unsupported", reason)
}

/** Bounds precede both parsers. These are work caps, not a wall-clock guarantee. */
function preflight(text: string): void {
  if (text.length > TRANSLATION_LIMITS.textUnits) throw new Declined("unknown", "text budget exceeded")
  const scanner = ts.createScanner(ts.ScriptTarget.ES2022, false, ts.LanguageVariant.Standard, text)
  let depth = 0, tokens = 0
  for (let token = scanner.scan(); token !== ts.SyntaxKind.EndOfFileToken; token = scanner.scan()) {
    requireFragment(token !== ts.SyntaxKind.ShebangTrivia, "source directives are unsupported")
    requireFragment(token !== ts.SyntaxKind.SingleLineCommentTrivia || !scanner.getTokenText().startsWith("///"), "triple-slash directives are unsupported")
    // Inspect actual comments, including after BOM/Unicode whitespace, without
    // counting trivia or mistaking directive-like string/block-comment data.
    if (token >= ts.SyntaxKind.FirstTriviaToken && token <= ts.SyntaxKind.LastTriviaToken) continue
    if (++tokens > TRANSLATION_LIMITS.tokens) throw new Declined("unknown", "token budget exceeded")
    if ([ts.SyntaxKind.OpenBraceToken, ts.SyntaxKind.OpenBracketToken, ts.SyntaxKind.OpenParenToken].includes(token)) depth++
    if ([ts.SyntaxKind.CloseBraceToken, ts.SyntaxKind.CloseBracketToken, ts.SyntaxKind.CloseParenToken].includes(token)) depth--
    if (depth > TRANSLATION_LIMITS.depth) throw new Declined("unknown", "nesting budget exceeded")
    requireFragment(token !== ts.SyntaxKind.NoSubstitutionTemplateLiteral && token !== ts.SyntaxKind.TemplateHead, "template syntax is outside this fragment")
  }
}

/** A closed in-memory parser host: no module resolution, library or filesystem. */
function parse(text: string): ts.SourceFile {
  const name = "translation-input.ts"
  const source = ts.createSourceFile(name, text, ts.ScriptTarget.ES2022, true, ts.ScriptKind.TS)
  const host: ts.CompilerHost = {
    getSourceFile: (file) => file === name ? source : undefined,
    getDefaultLibFileName: () => "", writeFile: () => { throw new Error("parser cannot emit") },
    getCurrentDirectory: () => "", getCanonicalFileName: (file) => file,
    useCaseSensitiveFileNames: () => true, getNewLine: () => "\n",
    fileExists: (file) => file === name, readFile: (file) => file === name ? text : undefined,
  }
  const program = ts.createProgram([name], { noLib: true, noResolve: true }, host)
  requireFragment(program.getSyntacticDiagnostics(source).length === 0, "malformed syntax")
  const pending: Array<{ node: ts.Node; depth: number }> = [{ node: source, depth: 0 }]
  let nodes = 0
  while (pending.length > 0) {
    const { node, depth } = pending.pop()!
    if (++nodes > TRANSLATION_LIMITS.nodes || depth > TRANSLATION_LIMITS.depth) throw new Declined("unknown", "AST budget exceeded")
    ts.forEachChild(node, (child) => { pending.push({ node: child, depth: depth + 1 }) })
  }
  return source
}

/** Read data directly from syntax, never via evaluation or production lowering. */
function literal(node: ts.Expression, depth = 0): Value {
  if (depth > TRANSLATION_LIMITS.depth) throw new Declined("unknown", "literal budget exceeded")
  if (ts.isStringLiteral(node)) return node.text
  if (node.kind === ts.SyntaxKind.TrueKeyword) return true
  if (node.kind === ts.SyntaxKind.FalseKeyword) return false
  if (node.kind === ts.SyntaxKind.NullKeyword) return null
  if (ts.isNumericLiteral(node)) {
    const raw = node.getText()
    requireFragment(/^(0|[1-9][0-9]*)$/.test(raw) && Number.isSafeInteger(Number(raw)), "only lossless unsigned plain integer literals are supported")
    return Number(raw)
  }
  if (ts.isArrayLiteralExpression(node)) return node.elements.map((item) => literal(item, depth + 1))
  requireFragment(ts.isObjectLiteralExpression(node), "expected closed literal data")
  const out: RecordValue = Object.create(null) as RecordValue
  for (const member of node.properties) {
    requireFragment(ts.isPropertyAssignment(member) && (ts.isIdentifier(member.name) || ts.isStringLiteral(member.name)), "only explicit literal members are supported")
    // The pinned parser retains these invalid grammar tokens on assignments,
    // although PropertyAssignment's public type omits the recovery fields.
    const recovered = member as ts.PropertyAssignment & Pick<ts.PropertyDeclaration, "modifiers" | "questionToken" | "exclamationToken">
    requireFragment(!recovered.modifiers?.length && !recovered.questionToken && !recovered.exclamationToken, "decorated literal members are unsupported")
    const key = member.name.text
    requireFragment(key !== "__proto__" && !Object.hasOwn(out, key), "duplicate or nonportable member name")
    out[key] = literal(member.initializer, depth + 1)
  }
  return out
}

function record(value: Value, keys: readonly string[]): RecordValue {
  requireFragment(value !== null && typeof value === "object" && !Array.isArray(value), "expected a record")
  requireFragment(Object.keys(value).length === keys.length && keys.every((key) => Object.hasOwn(value, key)), "record has missing or extra fields")
  return value
}
function string(value: Value | undefined): string {
  requireFragment(typeof value === "string", "expected a string literal")
  return value
}

/** The entire module must be one canonical intrinsic import and one catalog. */
function sourceMeaning(source: ts.SourceFile, context: TranslationContext): Value {
  // Pinned parser metadata includes all recognized reference/AMD/TS/JSX
  // pragmas and error-suppression comments, not just public reference arrays.
  const directives = source as ts.SourceFile & { readonly pragmas: ReadonlyMap<string, unknown>; readonly commentDirectives?: readonly unknown[] }
  requireFragment(directives.pragmas.size === 0 && !directives.commentDirectives?.length, "source directives are unsupported")
  requireFragment(source.statements.length === 2, "expected exactly an import and one declaration")
  const [importNode, statement] = source.statements
  requireFragment(importNode && ts.isImportDeclaration(importNode), "missing intrinsic import")
  requireFragment(!importNode.modifiers?.length && !importNode.attributes && ts.isStringLiteral(importNode.moduleSpecifier) && importNode.moduleSpecifier.text === "@norn/authoring", "only the canonical intrinsic module is supported")
  const clause = importNode.importClause
  requireFragment(clause && clause.phaseModifier === undefined && !clause.isTypeOnly && !clause.name && clause.namedBindings && ts.isNamedImports(clause.namedBindings) && clause.namedBindings.elements.length === 1, "expected a single named Norn import")
  const binding = clause.namedBindings.elements[0]!
  requireFragment(binding.name.text === "Norn" && !binding.propertyName && !binding.isTypeOnly, "import aliases are unsupported")
  requireFragment(statement && ts.isVariableStatement(statement) && statement.modifiers?.length === 1 && statement.modifiers[0]!.kind === ts.SyntaxKind.ExportKeyword, "expected an exported const declaration")
  requireFragment((statement.declarationList.flags & ts.NodeFlags.BlockScoped) === ts.NodeFlags.Const && statement.declarationList.declarations.length === 1, "expected a single const binding")
  const declaration = statement.declarationList.declarations[0]!
  requireFragment(ts.isIdentifier(declaration.name) && !declaration.type && !declaration.exclamationToken, "typed or destructured bindings are unsupported")
  // Identifier nodes can retain names whose module binding errors are only
  // reported by the binder. Scan the decoded name so escapes cannot hide them;
  // this pilot conservatively excludes contextual keywords as well.
  const name = declaration.name.text
  const nameScanner = ts.createScanner(ts.ScriptTarget.ES2022, false, ts.LanguageVariant.Standard, name)
  requireFragment(nameScanner.scan() === ts.SyntaxKind.Identifier && nameScanner.scan() === ts.SyntaxKind.EndOfFileToken && !["eval", "arguments", "Norn"].includes(name), "reserved binding names are unsupported")
  const call = declaration.initializer
  requireFragment(call && ts.isCallExpression(call) && !call.questionDotToken && !call.typeArguments && call.arguments.length === 1, "expected one literal catalog argument")
  const callee = call.expression
  requireFragment(ts.isPropertyAccessExpression(callee) && !callee.questionDotToken && ts.isIdentifier(callee.expression) && callee.expression.text === "Norn" && ts.isIdentifier(callee.name) && callee.name.text === "markings", "unsupported definition root")
  const meta = record(literal(call.arguments[0]!), ["id", "title", "version", "markings"])
  const id = string(meta.id), title = string(meta.title), version = string(meta.version)
  requireFragment(Array.isArray(meta.markings), "expected literal markings list")
  const ids = new Set<string>()
  for (const entry of meta.markings) {
    const marking = record(entry, ["id", "label", "severity", "category", "mask"])
    const markingId = string(marking.id)
    requireFragment(!ids.has(markingId), "duplicate marking id")
    ids.add(markingId)
    string(marking.label)
    string(marking.category)
    requireFragment(["low", "moderate", "high", "critical"].includes(string(marking.severity)), "unsupported severity")
    const mask = marking.mask
    requireFragment(mask !== null && typeof mask === "object" && !Array.isArray(mask), "expected a mask record")
    const kind = string(mask.kind)
    if (kind === "last-n") {
      record(mask, ["kind", "count"])
      requireFragment(typeof mask.count === "number" && mask.count > 0 && mask.count <= 4_294_967_295, "last-n count must be a positive u32")
    } else {
      record(mask, ["kind"])
      requireFragment(kind === "redacted" || kind === "null", "unsupported mask kind")
    }
  }
  // This is the independent denotation of the declared fragment, not a call
  // to envelope(), lowerMarkings(), compileDefinitions() or their helpers.
  return [{
    irVersion: "0.1", artifactId: id, kind: "entity", namespace: id.split(".")[0]!,
    name: title, version, lifecycle: "draft", scope: { orgId: context.orgId, environmentId: context.environmentId },
    dependencies: [], provenance: { source: "authoring_sdk", sourcePath: context.sourcePath },
    payload: { entityVersion: "2.0", markings: meta.markings },
  }]
}

/** JSON syntax first, then raw literal inspection so duplicate keys/numbers survive. */
function candidateMeaning(text: string): Value {
  try { JSON.parse(text) } catch { throw new Declined("unsupported", "candidate must be JSON text") }
  const source = parse(`(${text})`)
  const statement = source.statements[0]
  requireFragment(source.statements.length === 1 && statement && ts.isExpressionStatement(statement) && ts.isParenthesizedExpression(statement.expression), "invalid candidate root")
  return literal(statement.expression.expression)
}

function difference(expected: Value, candidate: Value, path = ""): string | undefined {
  if (expected === candidate) return undefined
  if (expected === null || candidate === null || typeof expected !== "object" || typeof candidate !== "object") return path
  if (Array.isArray(expected)) {
    if (!Array.isArray(candidate) || expected.length !== candidate.length) return path
    for (let i = 0; i < expected.length; i++) {
      const mismatch = difference(expected[i]!, candidate[i]!, `${path}/${i}`)
      if (mismatch !== undefined) return mismatch
    }
  } else {
    if (Array.isArray(candidate)) return path
    const keys = Object.keys(expected).sort()
    if (keys.length !== Object.keys(candidate).length || !keys.every((key) => Object.hasOwn(candidate, key))) return path
    for (const key of keys) {
      const mismatch = difference(expected[key]!, candidate[key]!, `${path}/${key.replaceAll("~", "~0").replaceAll("/", "~1")}`)
      if (mismatch !== undefined) return mismatch
    }
  }
  return undefined
}

/**
 * Check complete compileDefinitions().artifacts JSON against exact source text.
 * Context is trusted install-time input; a binding is not an admission receipt.
 */
export function validateMarkingsTranslation(sourceText: string, candidateIrText: string, context: TranslationContext): TranslationValidation {
  try {
    preflight(sourceText)
    preflight(candidateIrText)
    requireFragment(Object.keys(context).length === 3 && ["orgId", "environmentId", "sourcePath"].every((key) => Object.hasOwn(context, key)), "unsupported envelope context")
    for (const value of [context.orgId, context.environmentId, context.sourcePath]) {
      requireFragment(typeof value === "string" && value.length > 0, "context requires nonempty strings")
      if (value.length > 4_096) throw new Declined("unknown", "context budget exceeded")
    }
    const expected = sourceMeaning(parse(sourceText), context)
    const candidate = candidateMeaning(candidateIrText)
    // JSON tuple framing binds exact UTF-16 strings (including lone surrogates),
    // candidate text as supplied, context and the checker fragment identity.
    const binding = createHash("sha256").update(JSON.stringify([MARKINGS_TRANSLATION_FRAGMENT, sourceText, candidateIrText, context.orgId, context.environmentId, context.sourcePath])).digest("hex")
    const path = difference(expected, candidate)
    return path === undefined
      ? { status: "checked", fragment: MARKINGS_TRANSLATION_FRAGMENT, evidence: "structural", binding, artifacts: 1 }
      : { status: "mismatch", fragment: MARKINGS_TRANSLATION_FRAGMENT, evidence: "refuted", binding, path }
  } catch (error) {
    if (error instanceof Declined) return { status: error.status, evidence: "unknown", reason: error.message }
    return { status: "unknown", evidence: "unknown", reason: "parser/checker failure" }
  }
}
