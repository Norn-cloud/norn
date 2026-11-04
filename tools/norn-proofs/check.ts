import { checkFinite, encodeResult } from "./core"
import { readFile, writeFile, sha } from "./host"
import { integer } from "./protocol"

const [sourcePath, destination, seedText, expectedText] = process.argv.slice(2)
if (!sourcePath || !destination) throw new Error("usage: check.ts SOURCE RESULT SEED EXPECTED_STATES")
const start = performance.now()
const bytes = await readFile(sourcePath)
const outcome = await checkFinite({ source: new TextDecoder("utf-8", { fatal: true }).decode(bytes),
  config: "INIT q_init NEXT q_step INVARIANT q_inv", seed: integer(seedText, 0n, 18446744073709551615n),
  expectedStates: Number(integer(expectedText, 1n, 2147483647n)) })
await writeFile(destination, encodeResult(outcome, { modelSha256: sha(bytes), elapsedMs: performance.now() - start }))
