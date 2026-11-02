/**
 * Replace a local definition directory with an admitted artifact set.
 *
 * Writes one canonical `<artifactId>.json` per artifact and removes obsolete
 * artifact files, including duplicates under other names. Non-artifact files
 * such as `principals.json` are preserved.
 *
 * This module provides the directory operation; the public CLI exposes
 * check, compile and profile. Remote definition installs are provided by
 * `src/remote.ts` through `PUT /v1/defs`.
 */
import { existsSync, lstatSync, mkdirSync, readFileSync, readdirSync, renameSync, rmSync, writeFileSync } from "node:fs"
import { join } from "node:path"
import { type Json, canonicalJson } from "./canonical.ts"
import { artifactId, isArtifact } from "./pack.ts"

export interface DirDeployResult {
  readonly dir: string
  /** Files written (`<artifactId>.json`). */
  readonly written: ReadonlyArray<string>
  /** Artifact files removed: not in the set, or the set's artifact under another name. */
  readonly removed: ReadonlyArray<string>
}

/** Artifact ids may contain only these characters to become file names. */
const FILE_SAFE = /^[A-Za-z0-9_.-]+$/
/** Reserved non-artifact files; an artifact may not take their name. */
const RESERVED = new Set(["principals.json"])

/** The directory cannot be brought to the admitted set safely; nothing was changed. */
export class DeployRefused extends Error {}

/**
 * Install `artifacts` into `dir`. Every check runs before the first change:
 * a target name held by a non-artifact file or a reserved name, and any
 * symlinked JSON entry (which could redirect a write or delete outside the
 * directory), refuse the whole deploy. Files are then replaced atomically
 * (write a sibling, rename over).
 */
export function deployToDir(artifacts: ReadonlyArray<Json>, dir: string): DirDeployResult {
  const ids = artifacts.map((a) => artifactId(a))
  for (const id of ids) if (!FILE_SAFE.test(id) || id.startsWith(".")) throw new DeployRefused(`artifact id ${JSON.stringify(id)} cannot name a file`)
  const targets = new Set(ids.map((id) => `${id}.json`))
  for (const file of targets) if (RESERVED.has(file)) throw new DeployRefused(`artifact file ${file} would replace a reserved norn dev file`)
  mkdirSync(dir, { recursive: true })
  if (lstatSync(dir).isSymbolicLink()) throw new DeployRefused(`${dir} is a symlink`)
  const toRemove: string[] = []
  for (const file of readdirSync(dir).filter((f) => f.endsWith(".json")).sort()) {
    const path = join(dir, file)
    const stat = lstatSync(path)
    if (stat.isSymbolicLink()) throw new DeployRefused(`${file} is a symlink; refusing to write or delete through it`)
    if (!stat.isFile()) throw new DeployRefused(`${file} is not a regular file`)
    let value: unknown
    try {
      value = JSON.parse(readFileSync(path, "utf8"))
    } catch {
      value = undefined
    }
    const artifact = isArtifact(value)
    if (targets.has(file)) {
      if (!artifact) throw new DeployRefused(`${file} exists and is not an artifact envelope; refusing to overwrite it`)
      continue
    }
    if (artifact) toRemove.push(file)
  }
  const written: string[] = []
  artifacts.forEach((artifact, i) => {
    const file = `${ids[i]}.json`
    const temp = join(dir, `.${file}.norn-deploy-${process.pid}`)
    writeFileSync(temp, canonicalJson(artifact), { flag: "wx" })
    renameSync(temp, join(dir, file))
    written.push(file)
  })
  for (const file of toRemove) rmSync(join(dir, file))
  return { dir, written, removed: toRemove }
}

/** The artifacts a dev defs directory currently holds (empty when it does not exist). */
export function installedInDir(dir: string): Json[] {
  if (!existsSync(dir)) return []
  const out: Json[] = []
  for (const file of readdirSync(dir).filter((f) => f.endsWith(".json")).sort()) {
    const value = JSON.parse(readFileSync(join(dir, file), "utf8")) as unknown
    if (isArtifact(value)) out.push(value as Json)
  }
  return out
}
