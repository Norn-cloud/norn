import { readdir, readFile } from "./host"
import { join } from "node:path"
import { object } from "./protocol"
import { checkFinite } from "./core"

const directory = process.argv[2]
if (!directory) throw new Error("usage: correspondence.ts NATIVE_CORPUS_DIRECTORY")
const files = (await readdir(directory)).filter(name => name.endsWith(".json")).sort()
if (files.length < 16) throw new Error("native property corpus is missing or too small")
for (const name of files) {
  const value: unknown = JSON.parse(await readFile(join(directory, name), "utf8"))
  if (!object(value) || typeof value.numeric !== "string" || typeof value.boolean !== "string" ||
      typeof value.variable !== "string" || typeof value.number !== "number" || typeof value.flag !== "boolean") throw new Error("malformed native corpus")
  const options = { config: "INIT q_init NEXT q_step INVARIANT q_inv", seed: 1n, expectedStates: 2 }
  const numeric = await checkFinite({ ...options, source: value.numeric })
  if (numeric.status !== "invariant-violation" || numeric.violation.trace.length !== 2 ||
      numeric.violation.trace[1]?.state[value.variable] !== value.number) throw new Error(`numeric correspondence failed: ${name}`)
  const boolean = await checkFinite({ ...options, source: value.boolean, expectedStates: value.flag ? 2 : 1 })
  if (boolean.status !== (value.flag ? "invariant-violation" : "success")) throw new Error(`guard correspondence failed: ${name}`)
}
process.stdout.write(`${JSON.stringify({ nativeCases: files.length, checkedModels: files.length * 2, correspondencePassed: true })}\n`)
