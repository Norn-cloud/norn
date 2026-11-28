import { existsSync, mkdirSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs"
import { basename, dirname, join, posix, resolve } from "node:path"
import { buildInfo, type BuildInfo } from "./build-info.ts"
import { TOOL_ROOT } from "./profile.ts"
import { ProjectError, parseProject, type DevPrincipal, type Project } from "./project.ts"

/** Whether this tool runs from the Norn repository (a checkout) or from a released package. */
export function distribution(root: string = TOOL_ROOT): "repository" | "published" {
  return existsSync(join(root, "../../crates/norn-checker")) ? "repository" : "published"
}

/**
 * What a new project depends on. From a checkout, this tree. From a released
 * package, the packages of that same release, as its build manifest names
 * them (a version on a registry, or a tarball address).
 */
export function projectDependencies(mode: "repository" | "published" = distribution(), build: BuildInfo | undefined = buildInfo()) {
  const pinned = { effect: "4.0.0-rc.117", typescript: "6.0.3" }
  if (mode === "repository") return { "@norn/cli": `file:${TOOL_ROOT}`, "@norn/authoring": `file:${TOOL_ROOT}`, ...pinned }
  const cli = build?.packages["@norn/cli"]
  const authoring = build?.packages["@norn/authoring"]
  if (!cli || !authoring) throw new ProjectError("cli.setup.unpublished", "Published distribution is unavailable; run init from the repository.", 3)
  return { "@norn/cli": cli, "@norn/authoring": authoring, ...pinned }
}

/** Normalizes and validates one scaffold-relative file path, rejecting traversal, absolute paths, and `.github` segments. */
export function normalizeScaffoldPath(rawPath: string): string {
  const slashed = rawPath.replaceAll("\\", "/")
  if (slashed.length === 0 || slashed.includes("\0") || slashed.startsWith("/") || /^[A-Za-z]:/.test(slashed)) {
    throw new Error(`Refusing to scaffold invalid path "${rawPath}": scaffold paths must be non-empty relative paths.`)
  }
  const normalized = posix.normalize(slashed)
  if (
    normalized.length === 0 ||
    normalized === "." ||
    normalized === ".." ||
    normalized.startsWith("../") ||
    normalized.startsWith("/") ||
    normalized.endsWith("/")
  ) {
    throw new Error(`Refusing to scaffold path outside project root "${rawPath}".`)
  }
  const segments = normalized.split("/")
  for (const segment of segments) {
    if (segment.length === 0 || segment === "." || segment === "..") {
      throw new Error(`Refusing to scaffold invalid path segment in "${rawPath}".`)
    }
    if (segment.toLowerCase() === ".github") {
      throw new Error(`Refusing to scaffold CI file "${rawPath}": downstream Norn apps verify locally/in-browser with zero CI.`)
    }
  }
  return normalized
}

/** Validates and normalizes scaffold files once so guards and disk writes operate on the exact same paths. */
export function validateScaffoldFiles(files: Readonly<Record<string, string>>): Array<readonly [string, string]> {
  const seen = new Set<string>()
  const validated: Array<readonly [string, string]> = []
  for (const [rawPath, content] of Object.entries(files)) {
    const relPath = normalizeScaffoldPath(rawPath)
    if (seen.has(relPath)) {
      throw new Error(`Refusing duplicate scaffold path "${rawPath}" (normalizes to "${relPath}").`)
    }
    seen.add(relPath)
    validated.push([relPath, content])
  }
  return validated
}

/** Fail-closed guard: downstream Norn apps verify locally/in-browser with zero CI and must never scaffold `.github/` files. */
export function assertNoCiScaffoldFiles(files: Readonly<Record<string, string>>): void {
  validateScaffoldFiles(files)
}

export async function init(args: readonly string[]): Promise<void> {
  try { await initProject(args) }
  catch (error) {
    if (error instanceof ProjectError) throw error
    throw new ProjectError("cli.project.io", `Cannot create project; check paths and permissions. ${String(error)}`, 3)
  }
}

/** The registry templates `norn init` lays out from a checkout (`templates/<name>/template.json`). */
const PACK_TEMPLATES = ["three-systems", "supply-lab"] as const
const TEMPLATES = ["starter", "empty", ...PACK_TEMPLATES] as const
type PackTemplateName = (typeof PACK_TEMPLATES)[number]

/** `norn-template/1`: which registry parts a pack template copies, and what `norn seed` replays. */
interface PackProfile {
  readonly core: readonly string[]
  readonly packs: readonly string[]
  /** Packs whose committed synthetic export `norn dev --load` reads. */
  readonly data?: readonly string[]
  /** Named local principals of `norn dev` (DEV-PRINCIPALS-1). */
  readonly principals?: readonly DevPrincipal[]
  /** A `norn-calls/1` document (registry-relative) of governed calls `norn seed` sends, each as its actor. */
  readonly seed?: string
  /**
   * Questions and dashboards (registry-relative files) the project declares as
   * sources (`definitions/questions/<id>.json`), which `norn dev` installs once
   * the packs serve: each question of a question file by its name, the one
   * dashboard of a dashboard file by the template's `id`.
   */
  readonly definitions?: ReadonlyArray<{ readonly kind: "question" | "dashboard"; readonly from: string; readonly id?: string }>
}

interface PackLayout {
  readonly sources: string[]
  readonly files: Record<string, string>
  readonly data: string[]
  readonly principals?: readonly DevPrincipal[]
  readonly seeded: boolean
}

/**
 * A pack template (`three-systems`, `supply-lab`): registry packs as a hosted
 * tenant provisions them, from this checkout's registry
 * (`templates/<name>/template.json` names the parts): the host's trusted base
 * from `registry/core`, each pack's generated artifacts, the packs' committed
 * synthetic exports for `norn dev --load`, the named local principals the
 * packs' access policies need, their questions and dashboards as sources, and
 * the seed `norn seed` replays (`seed/calls.json`).
 * `norn dev` admits artifact envelopes found in source directories, so the
 * project compiles them with its own `src`.
 */
function packTemplate(name: PackTemplateName): PackLayout {
  if (distribution() !== "repository") throw new ProjectError("cli.project.usage", `The ${name} template reads the packs from a Norn checkout; run norn init from the repository.`, 2)
  const registry = join(TOOL_ROOT, "../../registry")
  const profile = JSON.parse(readFileSync(join(TOOL_ROOT, "templates", name, "template.json"), "utf8")) as PackProfile
  const json = (value: unknown) => `${JSON.stringify(value, null, 2)}\n`
  const files: Record<string, string> = {}
  for (const id of profile.core) files[`definitions/core/${id}.json`] = readFileSync(join(registry, "core/artifacts", `${id}.json`), "utf8")
  for (const pack of profile.packs) {
    const dir = join(registry, "packs", pack, "artifacts")
    for (const file of readdirSync(dir).filter((name) => name.endsWith(".json")).sort()) files[`definitions/packs/${pack}/${file}`] = readFileSync(join(dir, file), "utf8")
  }
  const systems = profile.data ?? []
  const data = systems.map((system) => `data/${system}.json`)
  for (const system of systems) files[`data/${system}.json`] = readFileSync(join(registry, "packs", system, "synthetic.json"), "utf8")
  if (profile.seed !== undefined) files["seed/calls.json"] = readFileSync(join(registry, profile.seed), "utf8")
  for (const { kind, from, id } of profile.definitions ?? []) {
    const documents = JSON.parse(readFileSync(join(registry, from), "utf8")) as Array<Record<string, unknown>>
    if (kind === "dashboard" && (documents.length !== 1 || id === undefined)) throw new Error(`${from}: a seeded dashboard file holds one dashboard and the template names its id`)
    for (const payload of documents) {
      const named = kind === "question" ? payload["name"] : id
      if (typeof named !== "string" || named === "") throw new Error(`${from}: a seeded ${kind} has no id`)
      files[`definitions/questions/${named}.json`] = json({ kind, id: named, payload })
    }
  }
  return {
    sources: ["definitions/core", ...profile.packs.map((pack) => `definitions/packs/${pack}`), ...(profile.definitions?.length ? ["definitions/questions"] : [])],
    files, data,
    ...(profile.principals === undefined ? {} : { principals: profile.principals }),
    seeded: profile.seed !== undefined,
  }
}

async function initProject(args: readonly string[]): Promise<void> {
  const [directory, ...flags] = args
  if (!directory || directory.startsWith("-")) throw new ProjectError("cli.project.usage", `Usage: norn init <dir> [--namespace NS] [--org ID] [--env ID] [--template ${TEMPLATES.join("|")}] [--no-install] [--json]`, 2)
  const options = new Map<string, string>()
  for (let i = 0; i < flags.length; i++) {
    const flag = flags[i]
    if (flag === "--json" || flag === "--no-install") continue
    if (!flag || !["--namespace", "--org", "--env", "--template"].includes(flag) || !flags[i + 1] || flags[i + 1]?.startsWith("-")) {
      throw new ProjectError("cli.project.usage", `Unknown option or missing value; use norn init <dir> --template ${TEMPLATES.join("|")}.`, 2)
    }
    options.set(flag, flags[++i] ?? "")
  }
  const target = resolve(directory)
  const name = basename(target)
  const namespace = options.get("--namespace") ?? name.toLowerCase().replace(/[^a-z0-9_]+/g, "_")
  if (!/^[a-z][a-z0-9_]*$/.test(namespace)) throw new ProjectError("cli.project.invalid_name", "Invalid namespace; use a lowercase letter followed by lowercase letters, digits or underscores.")
  const template = options.get("--template") ?? "starter"
  if (!(TEMPLATES as readonly string[]).includes(template)) throw new ProjectError("cli.project.usage", `Use --template ${TEMPLATES.slice(0, -1).join(", ")} or ${TEMPLATES.at(-1)}.`, 2)
  const packs = (PACK_TEMPLATES as readonly string[]).includes(template) ? packTemplate(template as PackTemplateName) : undefined
  if (existsSync(target) && (!statSync(target).isDirectory() || readdirSync(target).length !== 0)) throw new ProjectError("cli.project.exists", "Target is not an empty directory; choose a new directory.")
  const manifest: Project = { format: "norn-project/1", name, sources: ["src", ...(packs?.sources ?? [])], scope: { orgId: options.get("--org") ?? "local", environmentId: options.get("--env") ?? "dev" }, dev: { port: 4174, actor: "local:developer", ...(packs?.principals === undefined ? {} : { principals: packs.principals }) }, cell: ".norn/cell.sqlite" }
  parseProject(manifest)
  const json = (value: unknown) => `${JSON.stringify(value, null, 2)}\n`
  const files: Record<string, string> = {
    "norn.json": json(manifest),
    "package.json": json({ name, private: true, type: "module", dependencies: projectDependencies() }),
    "tsconfig.json": json({ extends: "@norn/cli/tsconfig.authoring.json", include: ["src/**/*.ts"] }),
    "AGENTS.md": readFileSync(join(TOOL_ROOT, "templates/starter/AGENTS.md"), "utf8"),
    ".gitignore": ".norn/\nnode_modules/\n",
  }
  Object.assign(files, packs?.files)
  if (template === "starter") files["src/product.ts"] = readFileSync(join(TOOL_ROOT, "templates/starter/src/product.ts"), "utf8").replaceAll("__NAMESPACE__", namespace)
  const validatedFiles = validateScaffoldFiles(files)
  mkdirSync(join(target, "src"), { recursive: true })
  const written: string[] = []
  try {
    for (const [relPath, content] of validatedFiles) {
      mkdirSync(dirname(join(target, relPath)), { recursive: true })
      writeFileSync(join(target, relPath), content, { flag: "wx" })
      written.push(relPath)
    }
  } catch (error) {
    for (const relPath of written) rmSync(join(target, relPath))
    throw new ProjectError("cli.project.io", `Cannot write project; check directory permissions. ${String(error)}`, 3)
  }
  if (!flags.includes("--no-install")) {
    const install = Bun.spawn([process.execPath, "install"], { cwd: target, stdout: flags.includes("--json") ? "ignore" : "inherit", stderr: "inherit" })
    if (await install.exited !== 0) throw new ProjectError("cli.project.install", "Dependency installation failed; retry bun install in the project directory.", 3)
  }
  const load = packs ? packs.data.map((path) => ` --load ${path}`).join("") : ""
  const seed = packs?.seeded ? " (then, while it serves: bunx norn seed)" : ""
  const result = { created: directory, manifest, files: validatedFiles.map(([relPath]) => relPath), next: `cd ${directory} && bunx norn dev${load}${seed}` }
  process.stdout.write(flags.includes("--json") ? json(result) : `created ${directory}/ (norn-project/1, scope org=${manifest.scope.orgId} env=${manifest.scope.environmentId})\n${template === "starter" ? `  src/product.ts   ${namespace}.Product, ${namespace}.product.add\n` : ""}${packs ? `  definitions/     the packs (${packs.sources.length} sources)\n${packs.data.length > 0 ? "  data/            their synthetic exports, snapshot and incremental rows\n" : ""}${packs.seeded ? "  seed/            the calls norn seed replays\n" : ""}` : ""}next: ${result.next}\n`)
}
