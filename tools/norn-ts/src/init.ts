import { existsSync, mkdirSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs"
import { basename, join, resolve } from "node:path"
import { TOOL_ROOT } from "./profile.ts"
import { ProjectError, parseProject, type Project } from "./project.ts"

export function projectDependencies(mode: "repository" | "published" = "repository") {
  if (mode === "published") throw new ProjectError("cli.setup.unpublished", "Published distribution is unavailable; run init from the repository.", 3)
  return { "@norn/cli": `file:${TOOL_ROOT}`, "@norn/authoring": `file:${TOOL_ROOT}`, effect: "4.0.0-rc.117", typescript: "6.0.3" }
}

export async function init(args: readonly string[]): Promise<void> {
  try { await initProject(args) }
  catch (error) {
    if (error instanceof ProjectError) throw error
    throw new ProjectError("cli.project.io", `Cannot create project; check paths and permissions. ${String(error)}`, 3)
  }
}

async function initProject(args: readonly string[]): Promise<void> {
  const [directory, ...flags] = args
  if (!directory || directory.startsWith("-")) throw new ProjectError("cli.project.usage", "Usage: norn init <dir> [--namespace NS] [--org ID] [--env ID] [--template starter|empty] [--no-install] [--json]", 2)
  const options = new Map<string, string>()
  for (let i = 0; i < flags.length; i++) {
    const flag = flags[i]
    if (flag === "--json" || flag === "--no-install") continue
    if (!flag || !["--namespace", "--org", "--env", "--template"].includes(flag) || !flags[i + 1] || flags[i + 1]?.startsWith("-")) {
      throw new ProjectError("cli.project.usage", "Unknown option or missing value; use norn init <dir> --template starter|empty.", 2)
    }
    options.set(flag, flags[++i] ?? "")
  }
  const target = resolve(directory)
  const name = basename(target)
  const namespace = options.get("--namespace") ?? name.toLowerCase().replace(/[^a-z0-9_]+/g, "_")
  if (!/^[a-z][a-z0-9_]*$/.test(namespace)) throw new ProjectError("cli.project.invalid_name", "Invalid namespace; use a lowercase letter followed by lowercase letters, digits or underscores.")
  const template = options.get("--template") ?? "starter"
  if (template !== "starter" && template !== "empty") throw new ProjectError("cli.project.usage", "Use --template starter or --template empty.", 2)
  if (existsSync(target) && (!statSync(target).isDirectory() || readdirSync(target).length !== 0)) throw new ProjectError("cli.project.exists", "Target is not an empty directory; choose a new directory.")
  const manifest: Project = { format: "norn-project/1", name, sources: ["src"], scope: { orgId: options.get("--org") ?? "local", environmentId: options.get("--env") ?? "dev" }, dev: { port: 4174, actor: "local:developer" }, cell: ".norn/cell.sqlite" }
  parseProject(manifest)
  const json = (value: unknown) => `${JSON.stringify(value, null, 2)}\n`
  const files: Record<string, string> = {
    "norn.json": json(manifest),
    "package.json": json({ name, private: true, type: "module", dependencies: projectDependencies() }),
    "tsconfig.json": json({ extends: "@norn/cli/tsconfig.authoring.json", include: ["src/**/*.ts"] }),
    "AGENTS.md": readFileSync(join(TOOL_ROOT, "templates/starter/AGENTS.md"), "utf8"),
    ".gitignore": ".norn/\nnode_modules/\n",
  }
  if (template === "starter") files["src/product.ts"] = readFileSync(join(TOOL_ROOT, "templates/starter/src/product.ts"), "utf8").replaceAll("__NAMESPACE__", namespace)
  mkdirSync(join(target, "src"), { recursive: true })
  const written: string[] = []
  try {
    for (const [path, content] of Object.entries(files)) {
      writeFileSync(join(target, path), content, { flag: "wx" })
      written.push(path)
    }
  } catch (error) {
    for (const path of written) rmSync(join(target, path))
    throw new ProjectError("cli.project.io", `Cannot write project; check directory permissions. ${String(error)}`, 3)
  }
  if (!flags.includes("--no-install")) {
    const install = Bun.spawn([process.execPath, "install"], { cwd: target, stdout: flags.includes("--json") ? "ignore" : "inherit", stderr: "inherit" })
    if (await install.exited !== 0) throw new ProjectError("cli.project.install", "Dependency installation failed; retry bun install in the project directory.", 3)
  }
  const result = { created: directory, manifest, files: Object.keys(files), next: `cd ${directory} && bunx norn dev` }
  process.stdout.write(flags.includes("--json") ? json(result) : `created ${directory}/ (norn-project/1, scope org=${manifest.scope.orgId} env=${manifest.scope.environmentId})\n${template === "starter" ? `  src/product.ts   ${namespace}.Product, ${namespace}.product.add\n` : ""}next: ${result.next}\n`)
}
