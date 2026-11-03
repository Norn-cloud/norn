import { check } from "tla-ts"

const [sourcePath, destination, moduleDirectory, seedText] = process.argv.slice(2)
if (!sourcePath || !destination || !moduleDirectory || !seedText || !/^\d+$/.test(seedText)) {
  throw new Error("usage: check.ts SOURCE RESULT MODULE_DIRECTORY SEED")
}
const libraries = new Map<string, string>()
for (const name of ["Apalache", "Variants"]) {
  libraries.set(name, await Bun.file(`${moduleDirectory}/${name}.tla`).text())
}
try {
  const result = await check({
    source: await Bun.file(sourcePath).text(),
    config: "INIT q_init NEXT q_step INVARIANT q_inv",
    seed: BigInt(seedText), workers: 1,
    loadModule: name => libraries.get(name),
  })
  const complete = result.status !== "success" ||
    (result.queuedStates === 0 && (result.initialStates?.distinct ?? 0) > 0)
  await Bun.write(destination, JSON.stringify({
    status: complete ? result.status : "unknown",
    reason: complete ? undefined : "empty initialization or unfinished state exploration",
    generatedStates: result.generatedStates, distinctStates: result.distinctStates,
    queuedStates: result.queuedStates, initialStates: result.initialStates,
    violation: result.violation, failure: result.failure,
  }, (_key, value: unknown) => typeof value === "bigint" ? value.toString() : value))
} catch (error) {
  await Bun.write(destination, JSON.stringify({status: "unknown", reason: String(error)}))
}
