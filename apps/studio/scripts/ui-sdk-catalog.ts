/**
 * The pinned UI SDK catalog (#1762, milestone B): what `@ui-sdk/core` at the exact version Studio
 * pins says about itself, written to `crates/norn-ui-checker/catalog/ui-sdk.json` for the Ring-1
 * checker. Nothing here is copied by hand:
 *
 * - every exported symbol, its kind and its stability tag (information only), and each primitive's
 *   options, dismissal reasons, parts, state and api come from the package's own `.d.ts`, read with
 *   the TypeScript compiler's type checker;
 * - each primitive's component name, agent actions and controlled state come from UI SDK itself:
 *   one instance of each primitive is made with the kernel's `watchers` hook installed, which is
 *   how `page/agent` learns them. The fallback verb rule is read from `page/agent`'s source with
 *   the TypeScript parser.
 *
 * `bun scripts/ui-sdk-catalog.ts --write` regenerates it; `test/ui-sdk-catalog.test.ts` is the
 * drift gate (regenerating must give the checked-in bytes). The gate makes an upgrade's changes
 * visible for the bump lane; it does not restrict what a widget may use.
 */
import { createHash } from "node:crypto"
import { readFileSync, readdirSync, writeFileSync } from "node:fs"
import { createRequire } from "node:module"
import { dirname, join, relative, resolve } from "node:path"
import type * as TS from "typescript"

const STUDIO = resolve(import.meta.dir, "..")
export const CATALOG_PATH = resolve(STUDIO, "../../crates/norn-ui-checker/catalog/ui-sdk.json")
/** Every exported symbol with its stability tag: information for the bump lane, not read by the checker. */
export const SYMBOLS_PATH = resolve(STUDIO, "../../crates/norn-ui-checker/catalog/ui-sdk.symbols.json")
/** The typed view `tools/norn-ts` authors widgets against: each primitive's bindable options and callbacks, as `const`. */
export const AUTHORING_PATH = resolve(STUDIO, "../../tools/norn-ts/generated/ui-sdk.ts")
/** Primitives whose items are added at run time take these exported option types (a toast's duration, priority, dismissible). */
const ITEM_OPTIONS: Record<string, string> = { toast: "ToastOptions" }
const PRESSED_PARTS = ["trigger", "close", "button", "action"]
const PACKAGE = "@ui-sdk/core"
const require = createRequire(join(STUDIO, "package.json"))
const ts = require("typescript") as typeof TS
const STABILITY = ["stable", "experimental", "beta", "internal", "deprecated"] as const
type Json = null | boolean | number | string | Json[] | { [key: string]: Json }

/** The version Studio pins, which must be the one installed. */
export function pinnedVersion(): string {
  const pinned = (JSON.parse(readFileSync(join(STUDIO, "package.json"), "utf8")) as { dependencies: Record<string, string> }).dependencies[PACKAGE]
  if (pinned === undefined || !/^\d+\.\d+\.\d+(-[0-9A-Za-z.]+)?$/.test(pinned)) throw new Error(`${PACKAGE} must be pinned to an exact version in apps/studio/package.json`)
  return pinned
}

const packageDir = (): string => dirname(require.resolve(`${PACKAGE}/package.json`))

function walk(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => (entry.isDirectory() ? walk(join(dir, entry.name)) : [join(dir, entry.name)]))
}

/** sha256 over every shipped `.d.ts` and the package manifest, by sorted relative path. */
function typesSha256(root: string): string {
  const files = walk(root).filter((file) => file.endsWith(".d.ts") || relative(root, file) === "package.json").sort()
  const hash = createHash("sha256")
  for (const file of files) hash.update(`${relative(root, file)}\0${createHash("sha256").update(readFileSync(file)).digest("hex")}\n`)
  return hash.digest("hex")
}

function stabilityOf(node: TS.Node | undefined): string {
  for (let at = node; at !== undefined; at = at.parent) {
    for (const tag of ts.getJSDocTags(at)) {
      const name = tag.tagName.text
      if ((STABILITY as readonly string[]).includes(name)) return name
    }
    if (ts.isTypeAliasDeclaration(at) || ts.isInterfaceDeclaration(at) || ts.isVariableStatement(at) || ts.isFunctionDeclaration(at) || ts.isClassDeclaration(at)) break
  }
  return "untagged"
}

function defaultOf(node: TS.Node | undefined): string | undefined {
  if (node === undefined) return undefined
  const tag = ts.getJSDocTags(node).find((candidate) => candidate.tagName.text === "defaultValue")
  return tag === undefined ? undefined : (ts.getTextOfJSDocComment(tag.comment) ?? "").trim()
}

const sortObject = (entries: Iterable<[string, Json]>): { [key: string]: Json } => Object.fromEntries([...entries].sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)))
const sortValues = (values: Json[]): Json[] => [...values].sort((a, b) => (JSON.stringify(a) < JSON.stringify(b) ? -1 : 1))

/** What a JSON document may set an option to, from the option's declared type. */
function optionType(checker: TS.TypeChecker, type: TS.Type): { [key: string]: Json } {
  const members = type.isUnion() ? type.types : [type]
  const literals: Json[] = []
  const kinds = new Set<string>()
  for (const member of members) {
    const flags = member.flags
    if (flags & (ts.TypeFlags.Undefined | ts.TypeFlags.Void)) continue
    if (member.isStringLiteral() || member.isNumberLiteral()) literals.push(member.value)
    else if (flags & ts.TypeFlags.BooleanLiteral) literals.push(checker.typeToString(member) === "true")
    else if (flags & ts.TypeFlags.Null) literals.push(null)
    else if (flags & ts.TypeFlags.String) kinds.add("string")
    else if (flags & ts.TypeFlags.Number) kinds.add("number")
    else if (flags & ts.TypeFlags.Boolean) kinds.add("boolean")
    else if (member.getCallSignatures().length > 0) {
      if (members.length === 1 || members.every((other) => other === member || other.flags & (ts.TypeFlags.Undefined | ts.TypeFlags.Void))) {
        const first = member.getCallSignatures()[0]?.getParameters()[0]
        const declared = first?.valueDeclaration
        const argument = first === undefined || declared === undefined ? undefined : optionType(checker, checker.getNonNullableType(checker.getTypeOfSymbolAtLocation(first, declared)))
        return argument === undefined ? { type: "callback" } : { type: "callback", argument }
      }
      kinds.add("callback")
    }
    else kinds.add("other")
  }
  // `boolean` arrives as the two literals: name it once when both are there and nothing else is.
  const bools = literals.filter((value) => typeof value === "boolean")
  if (bools.length === 2 && literals.length === 2 && kinds.size === 0) return { type: "boolean" }
  if (kinds.size === 0) return { type: "enum", values: sortValues(literals) }
  if (kinds.size === 1 && literals.length === 0) return { type: [...kinds][0] ?? "other" }
  if (kinds.has("callback") || kinds.has("other")) return { type: "other", ts: checker.typeToString(type, undefined, ts.TypeFormatFlags.NoTruncation) }
  return { type: "union", kinds: [...kinds].sort(), values: sortValues(literals) }
}

function stringLiterals(type: TS.Type | undefined): string[] {
  if (type === undefined) return []
  return (type.isUnion() ? type.types : [type]).filter((member) => member.isStringLiteral()).map((member) => (member as TS.StringLiteralType).value).sort()
}

/** The literal `reason` values a callback option's `argument`th parameter carries. */
function callbackReasons(checker: TS.TypeChecker, options: TS.Type, name: string, argument: number, at: TS.Node): string[] {
  const property = options.getProperty(name)
  if (property === undefined) return []
  const callback = checker.getNonNullableType(checker.getTypeOfSymbolAtLocation(property, at))
  const parameter = callback.getCallSignatures()[0]?.getParameters()[argument]
  if (parameter === undefined) return []
  const details = checker.getTypeOfSymbolAtLocation(parameter, at)
  const reason = details.getProperty("reason")
  return reason === undefined ? [] : stringLiterals(checker.getTypeOfSymbolAtLocation(reason, at))
}

const memberNames = (checker: TS.TypeChecker, type: TS.Type | undefined): string[] => (type === undefined ? [] : checker.getPropertiesOfType(type).map((symbol) => symbol.name).sort())

/** `page/agent`'s fallback rule for a primitive that declares no agent actions, read with the TypeScript parser. */
function agentVerbRule(root: string): { verbs: RegExp; synonyms: Record<string, string[]> } {
  const path = join(root, "dist/page/agent/agent.js")
  const source = ts.createSourceFile(path, readFileSync(path, "utf8"), ts.ScriptTarget.Latest, true, ts.ScriptKind.JS)
  let verbs: RegExp | undefined
  const synonyms: Record<string, string[]> = {}
  for (const statement of source.statements) {
    if (!ts.isVariableStatement(statement)) continue
    for (const declaration of statement.declarationList.declarations) {
      if (!ts.isIdentifier(declaration.name) || declaration.initializer === undefined) continue
      const init = declaration.initializer
      if (declaration.name.text === "verbs" && ts.isRegularExpressionLiteral(init)) {
        const text = init.text
        verbs = new RegExp(text.slice(1, text.lastIndexOf("/")), text.slice(text.lastIndexOf("/") + 1))
      }
      if (declaration.name.text === "synonyms" && ts.isObjectLiteralExpression(init)) {
        for (const property of init.properties) {
          if (!ts.isPropertyAssignment(property) || !ts.isArrayLiteralExpression(property.initializer)) continue
          const key = ts.isIdentifier(property.name) || ts.isStringLiteral(property.name) ? property.name.text : undefined
          if (key !== undefined) synonyms[key] = property.initializer.elements.filter(ts.isStringLiteral).map((element) => element.text)
        }
      }
    }
  }
  if (verbs === undefined) throw new Error("page/agent no longer declares its `verbs` rule: update the catalog generator")
  return { verbs, synonyms }
}

/**
 * The foundation behaviours (`<area>/<module>` under `dist/foundation`, the kernel aside: it is
 * how every primitive is built, not a behaviour) a module reaches through its static imports, read
 * with the TypeScript parser: what a primitive is made of, as UI SDK ships it.
 */
function foundationBehaviors(root: string, entry: string, cache: Map<string, Set<string>>): string[] {
  const foundation = join(root, "dist/foundation")
  const seen = new Set<string>()
  const pending = [entry]
  const found = new Set<string>()
  while (pending.length > 0) {
    const file = pending.pop() as string
    if (seen.has(file)) continue
    seen.add(file)
    const rel = relative(foundation, file)
    const [area, module] = rel.split("/")
    if (!rel.startsWith("..") && area !== undefined && module !== undefined && area !== "kernel" && module !== "index.js") found.add(`${area}/${module.replace(/\.js$/, "")}`)
    let imports = cache.get(file)
    if (imports === undefined) {
      const source = ts.createSourceFile(file, readFileSync(file, "utf8"), ts.ScriptTarget.Latest, false, ts.ScriptKind.JS)
      imports = new Set<string>()
      for (const statement of source.statements) {
        const specifier = (ts.isImportDeclaration(statement) || ts.isExportDeclaration(statement)) ? statement.moduleSpecifier : undefined
        if (specifier !== undefined && ts.isStringLiteral(specifier) && specifier.text.startsWith(".")) imports.add(resolve(dirname(file), specifier.text))
      }
      cache.set(file, imports)
    }
    pending.push(...imports)
  }
  return [...found].sort()
}

type Runtime = { component: string; file: string; agent: { actions?: string[] } | undefined; controlled: string[] | undefined; state: string[]; api: string[] }

/** Make one instance of every exported primitive with the kernel's watcher hook, as `page/agent` does. */
async function runtimePrimitives(root: string, exports: Record<string, unknown>): Promise<Map<string, Runtime>> {
  const { watchers } = (await import(join(root, "dist/foundation/kernel/watchers.js"))) as { watchers: { add?: unknown } }
  // An adapter keeps an instance once it renders; the watcher hears it then.
  const { keep } = (await import(join(root, "dist/foundation/kernel/keep.js"))) as { keep: (instance: unknown) => void }
  let seen: { agent: Runtime["agent"]; controlled: string[] | undefined } | undefined
  watchers.add = (_instance: unknown, _parts: unknown, definition: { agent?: { actions?: string[] }; controlled?: string[] }) => {
    seen = { agent: definition.agent, controlled: definition.controlled }
    return () => {}
  }
  const found = new Map<string, Runtime>()
  try {
    for (const [subpath, entry] of Object.entries(exports)) {
      const target = typeof entry === "object" && entry !== null ? (entry as { default?: unknown }).default : undefined
      if (typeof target !== "string" || !target.endsWith(".js")) continue
      const module = (await import(join(root, target))) as Record<string, unknown>
      for (const [name, value] of Object.entries(module)) {
        if (typeof value !== "function" || !("options" in value) || !("slots" in value)) continue
        seen = undefined
        const instance = (value as (options: { id: string }) => { name: string; api: Record<string, unknown>; get: () => object })({ id: "catalog" })
        keep(instance)
        if (seen === undefined) throw new Error(`${subpath} ${name}: the kernel did not report its definition`)
        const reported: { agent: Runtime["agent"]; controlled: string[] | undefined } = seen
        found.set(`${subpath}#${name}`, {
          component: instance.name,
          file: join(root, target),
          agent: reported.agent,
          controlled: reported.controlled,
          state: Object.keys(instance.get()).sort(),
          api: Object.keys(instance.api).filter((key) => typeof instance.api[key] === "function").sort(),
        })
      }
    }
  } finally {
    delete watchers.add
  }
  return found
}

/** Every foundation behaviour UI SDK ships: each module under `dist/foundation` but the kernel's. */
function foundationModules(root: string): string[] {
  const foundation = join(root, "dist/foundation")
  return walk(foundation).map((file) => relative(foundation, file)).filter((rel) => rel.endsWith(".js") && !rel.startsWith("kernel/") && !rel.endsWith("/index.js"))
    .map((rel) => rel.replace(/\.js$/, "")).sort()
}

/** Every option of `type` with what a document may set it to, its stability and its documented default. */
function describeOptions(checker: TS.TypeChecker, type: TS.Type, at: TS.Node): { [key: string]: Json } {
  return sortObject(checker.getPropertiesOfType(type).map((property) => {
    const declared = property.declarations?.[0]
    const described: { [key: string]: Json } = { ...optionType(checker, checker.getTypeOfSymbolAtLocation(property, at)), stability: stabilityOf(declared) }
    const fallback = defaultOf(declared)
    if (fallback !== undefined) described["default"] = fallback
    return [property.name, described]
  }))
}

/** Generate the exact bytes of the catalog and of the symbol list. */
export async function buildCatalog(): Promise<{ catalog: string; symbols: string; authoring: string }> {
  const version = pinnedVersion()
  const root = packageDir()
  const manifest = JSON.parse(readFileSync(join(root, "package.json"), "utf8")) as { version: string; exports: Record<string, unknown> }
  if (manifest.version !== version) throw new Error(`installed ${PACKAGE} is ${manifest.version}, Studio pins ${version}: run bun install in apps/studio`)
  const subpaths = Object.entries(manifest.exports).flatMap(([subpath, entry]) => {
    const types = typeof entry === "object" && entry !== null ? (entry as { types?: unknown }).types : undefined
    return typeof types === "string" ? [[subpath, join(root, types)] as const] : []
  })
  const program = ts.createProgram(subpaths.map(([, file]) => file), {
    strict: true, noEmit: true, skipLibCheck: true, target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext,
    moduleResolution: ts.ModuleResolutionKind.Bundler, lib: ["lib.es2023.d.ts", "lib.dom.d.ts", "lib.dom.iterable.d.ts"], types: [],
  })
  const checker = program.getTypeChecker()
  const runtime = await runtimePrimitives(root, manifest.exports)
  const rule = agentVerbRule(root)

  const symbols = new Map<string, { modules: Set<string>; kind: string; stability: string }>()
  const primitives = new Map<string, { [key: string]: Json }>()
  const itemTypes = new Map<string, TS.Symbol>()
  const imports = new Map<string, Set<string>>()
  let dismissReasons: string[] = []
  for (const [subpath, file] of subpaths) {
    const source = program.getSourceFile(file)
    const moduleSymbol = source === undefined ? undefined : checker.getSymbolAtLocation(source)
    if (source === undefined || moduleSymbol === undefined) throw new Error(`${subpath}: no declarations`)
    for (const exported of checker.getExportsOfModule(moduleSymbol)) {
      const symbol = exported.flags & ts.SymbolFlags.Alias ? checker.getAliasedSymbol(exported) : exported
      const declaration = symbol.declarations?.[0]
      const kind = symbol.flags & ts.SymbolFlags.Function ? "function" : symbol.flags & ts.SymbolFlags.Class ? "class" : symbol.flags & ts.SymbolFlags.Value ? "value" : "type"
      const known = symbols.get(exported.name) ?? { modules: new Set<string>(), kind, stability: stabilityOf(declaration) }
      known.modules.add(subpath)
      symbols.set(exported.name, known)
      if (exported.name === "DismissReason") dismissReasons = stringLiterals(checker.getDeclaredTypeOfSymbol(symbol))
      if (Object.values(ITEM_OPTIONS).includes(exported.name)) itemTypes.set(exported.name, symbol)
      const live = runtime.get(`${subpath}#${exported.name}`)
      if (live === undefined || declaration === undefined) continue
      const instanceType = checker.getTypeOfSymbolAtLocation(symbol, declaration).getCallSignatures()[0]?.getReturnType()
      const typesSymbol = instanceType?.getProperty("types")
      if (typesSymbol === undefined) throw new Error(`${subpath} ${exported.name}: a primitive without declared types`)
      const types = checker.getTypeOfSymbolAtLocation(typesSymbol, declaration)
      const part = (name: string): TS.Type | undefined => {
        const member = types.getProperty(name)
        return member === undefined ? undefined : checker.getTypeOfSymbolAtLocation(member, declaration)
      }
      const optionsType = part("options")
      if (optionsType === undefined) throw new Error(`${subpath} ${exported.name}: no options type`)
      const options = describeOptions(checker, optionsType, declaration)
      const attempts = callbackReasons(checker, optionsType, "onDismissAttempt", 0, declaration)
      const closes = callbackReasons(checker, optionsType, "onOpenChange", 1, declaration)
      const parts = memberNames(checker, part("slots"))
      const declared = live.agent?.actions
      const actions = live.api.filter((name) => (declared ? declared.includes(name) : rule.verbs.test(name) && !(rule.synonyms[name] ?? []).some((of) => live.api.includes(of))))
      // An agent presses a trigger, close or button part, or a primitive that runs an `action` when pressed.
      const pressed = optionsType.getProperty("action") !== undefined && actions.length === 0
      if (pressed || parts.some((slot) => PRESSED_PARTS.includes(slot))) actions.push("press")
      const reports = live.controlled ?? ["open", "value", "checked"].filter((key) => live.state.includes(key))
      const entry: { [key: string]: Json } = primitives.get(live.component) ?? {
        exports: [], modules: [], stability: stabilityOf(declaration), options,
        dismissAttempts: attempts, dismissReasons: attempts.filter((reason) => closes.length === 0 || closes.includes(reason)),
        parts, state: memberNames(checker, part("state")), api: memberNames(checker, part("api")),
        behaviors: foundationBehaviors(root, live.file, imports),
        agent: { component: live.component, actions, state: [...reports, ...(live.state.includes("pending") ? ["pending", "error"] : [])] },
      }
      for (const [key, value] of [["exports", exported.name], ["modules", subpath]] as const) {
        const list = entry[key] as string[]
        if (!list.includes(value)) list.push(value)
        list.sort()
      }
      primitives.set(live.component, entry)
    }
  }
  if (dismissReasons.length === 0) throw new Error("the kernel's DismissReason union is not exported")
  for (const [component, typeName] of Object.entries(ITEM_OPTIONS)) {
    const entry = primitives.get(component)
    const symbol = itemTypes.get(typeName)
    const at = symbol?.declarations?.[0]
    if (entry === undefined || symbol === undefined || at === undefined) throw new Error(`${component}: its item options ${typeName} are not exported`)
    const itemType = checker.getDeclaredTypeOfSymbol(symbol)
    entry["itemOptions"] = describeOptions(checker, itemType, at)
    entry["itemDismissReasons"] = callbackReasons(checker, itemType, "onDismiss", 0, at)
  }
  const head = { catalogVersion: "1", package: PACKAGE, version, typesSha256: typesSha256(root), generator: { typescript: ts.version } }
  const json = (value: Json): string => `${JSON.stringify(value, null, 2)}\n`
  const authoring = sortObject([...primitives].map(([component, entry]) => {
    const options = { ...(entry["options"] as Record<string, { type: string }>), ...((entry["itemOptions"] ?? {}) as Record<string, { type: string }>) }
    const names = (keep: (type: string) => boolean): string[] => Object.keys(options).filter((name) => keep(options[name]?.type ?? "other")).sort()
    return [component, { props: names((type) => type !== "callback" && type !== "other"), callbacks: names((type) => type === "callback") }]
  }))
  return {
    authoring: `// Generated by apps/studio/scripts/ui-sdk-catalog.ts from ${PACKAGE}@${version}; do not edit.\n// The bindable options and callbacks of each UI SDK primitive, typed for widget authoring.\nexport const UI_SDK = ${JSON.stringify({ version, components: authoring }, null, 2)} as const\n`,
    catalog: json({ ...head, dismissReasons, behaviors: foundationModules(root), primitives: sortObject(primitives) }),
    symbols: json({ ...head, symbols: sortObject([...symbols].map(([name, symbol]) => [name, { kind: symbol.kind, modules: [...symbol.modules].sort(), stability: symbol.stability }])) }),
  }
}

if (import.meta.main) {
  const built = await buildCatalog()
  for (const [path, bytes] of [[CATALOG_PATH, built.catalog], [SYMBOLS_PATH, built.symbols], [AUTHORING_PATH, built.authoring]] as const) {
    if (process.argv.includes("--write")) writeFileSync(path, bytes)
    else if (readFileSync(path, "utf8") !== bytes) {
      console.error(`${relative(process.cwd(), path)} is stale: run bun scripts/ui-sdk-catalog.ts --write`)
      process.exit(1)
    }
  }
}
