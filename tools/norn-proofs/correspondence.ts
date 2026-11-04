import { readdir, readFile } from "./host"
import { join } from "node:path"
import { object, array } from "./protocol"
import { checkFinite } from "./core"

export async function verifyCorrespondence(directory: string,
  run: (source: string, expectedStates: number) => Promise<unknown>) {
  const files = (await readdir(directory)).filter(name => name.endsWith(".json")).sort()
  if (files.length < 16) throw new Error("native property corpus is missing or too small")
  for (const name of files) {
    const value: unknown = JSON.parse(await readFile(join(directory, name), "utf8"))
    if (!object(value) || typeof value.numeric !== "string" || typeof value.boolean !== "string" ||
        typeof value.variable !== "string" || typeof value.number !== "number" || typeof value.flag !== "boolean") throw new Error("malformed native corpus")
    const numeric = await run(value.numeric, 2)
    const trace = object(numeric) && object(numeric.violation) ? numeric.violation.trace : undefined
    const last = array(trace) ? trace[1] : undefined
    if (!object(numeric) || numeric.status !== "invariant-violation" || !array(trace) || trace.length !== 2 ||
        !object(last) || !object(last.state) || last.state[value.variable] !== value.number) throw new Error(`numeric correspondence failed: ${name}`)
    const boolean = await run(value.boolean, value.flag ? 2 : 1)
    if (!object(boolean) || boolean.status !== (value.flag ? "invariant-violation" : "success")) throw new Error(`guard correspondence failed: ${name}`)
  }
  return { nativeCases: files.length, checkedModels: files.length * 2, correspondencePassed: true }
}
if (import.meta.main) {
  const directory = process.argv[2]
  if (!directory) throw new Error("usage: correspondence.ts NATIVE_CORPUS_DIRECTORY")
  const result = await verifyCorrespondence(directory, (source, expectedStates) =>
    checkFinite({ source, expectedStates, config: "INIT q_init NEXT q_step INVARIANT q_inv", seed: 1n }))
  process.stdout.write(`${JSON.stringify(result)}\n`)
}
