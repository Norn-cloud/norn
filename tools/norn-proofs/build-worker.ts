import { fileURLToPath } from "node:url"
import { basename, dirname } from "node:path"
import { sha, readFile } from "./host"
import type { BunPlugin } from "bun"

const destination = process.argv[2]
if (!destination) throw new Error("usage: build-worker.ts OUTPUT_FILE")
const source = fileURLToPath(new URL("./node_modules/tla-ts/src/index.ts", import.meta.url))
const codegen = fileURLToPath(new URL("./node_modules/tla-ts/src/codegen.ts", import.meta.url))
if (sha(await readFile(source)) !== "f6cd7f8d8d89a20e4b96893e75c7de65477977b13ac15c06ecd427cb8f7cf265" ||
    sha(await readFile(codegen)) !== "34fe59c1d240450b8a1fe212ecebe557acbb16d7e804162a8f952ef1b02f5ba0") {
  throw new Error("unqualified tla-ts source revision")
}
// PROOF-TLA-1: getEntryEvaluator uses its existing AST interpreter when the
// optional generator returns undefined. Worker policy continues to forbid eval.
const interpreter: BunPlugin = {
  name: "tla-ts-worker-interpreter",
  setup(build) {
    build.onResolve({ filter: /^tla-ts$/ }, () => ({ path: source }))
    build.onLoad({ filter: /\/tla-ts\/src\/codegen\.ts$/ }, ({ path }) => {
      if (path !== codegen) throw new Error("unexpected code-generator module")
      return { contents: "export const generateEvaluator = () => undefined", loader: "ts" }
    })
  },
}
const license = await readFile(fileURLToPath(new URL("./node_modules/tla-ts/LICENSE", import.meta.url)), "utf8")
const result = await Bun.build({ entrypoints: [fileURLToPath(new URL("./worker.ts", import.meta.url))],
  outdir: dirname(destination), naming: basename(destination), target: "browser", minify: true, plugins: [interpreter],
  banner: `/* tla-ts 0.0.7, interpreter-only Worker build.\n${license.replaceAll("*/", "* /")}\n*/` })
if (!result.success) throw new Error(result.logs.map(String).join("\n"))
process.stdout.write(`${JSON.stringify({ workerBundleSha256: sha(await readFile(destination)), variant: "tla-ts-0.0.7/AST-interpreter" })}\n`)
