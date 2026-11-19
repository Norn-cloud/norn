// One evaluator (D105): the engine package (this crate without its `checker` feature,
// NORN_WASM_ENGINE=1) and the checker package answer every evaluation byte for byte the same.
//
// Usage: node engine-parity.mjs <checker-pkg-dir> <engine-pkg-dir>
//
// The corpus is every guard and assignment of the page machines engine-corpus.json names, plus its own
// expressions, each over every input (and the raw, malformed inputs and profiles), so a machine
// added to the list is covered with no new code.
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const [checkerDir, engineDir] = process.argv.slice(2);
if (!checkerDir || !engineDir) {
  console.error("usage: node engine-parity.mjs <checker-pkg-dir> <engine-pkg-dir>");
  process.exit(2);
}
const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "../../../..");
const load = async (dir, name) => {
  const pkg = await import(pathToFileURL(resolve(dir, `${name}.js`)).href);
  pkg.initSync({ module: readFileSync(resolve(dir, `${name}_bg.wasm`)) });
  return pkg;
};
const checker = await load(checkerDir, "norn_checker_wasm");
const engine = await load(engineDir, "norn_engine_wasm");

const corpus = JSON.parse(readFileSync(resolve(here, "engine-corpus.json"), "utf8"));
const expressions = new Set(corpus.expressions);
for (const path of corpus.machines) {
  const doc = JSON.parse(readFileSync(resolve(repoRoot, path), "utf8"));
  const machines = doc.payload?.ux?.pages?.map((page) => page.machine).filter(Boolean) ?? [doc.machine ?? doc];
  for (const machine of machines) {
    for (const transition of machine.transitions ?? []) {
      if (transition.guard) expressions.add(transition.guard);
      for (const value of Object.values(transition.assign ?? {})) expressions.add(value);
      for (const effect of transition.effects ?? []) for (const value of Object.values(effect.input ?? {})) expressions.add(value);
    }
  }
}
const inputs = [...corpus.inputs.map((input) => JSON.stringify(input)), ...corpus.rawInputs];
let compared = 0;
const differ = [];
for (const expression of expressions) {
  for (const input of inputs) {
    for (const profile of [undefined, ...corpus.profiles]) {
      const a = checker.evaluateExpression(expression, input, profile);
      const b = engine.evaluateExpression(expression, input, profile);
      compared += 1;
      if (a !== b) differ.push({ expression, input, profile, checker: a, engine: b });
    }
  }
}
if (expressions.size < corpus.expressions.length + 10) {
  console.error(`engine parity: only ${expressions.size} expressions; the machines in engine-corpus.json contributed too few`);
  process.exit(1);
}
if (differ.length > 0) {
  console.error(JSON.stringify(differ.slice(0, 5), null, 2));
  console.error(`engine parity: ${differ.length} of ${compared} evaluations differ`);
  process.exit(1);
}
console.log(`engine parity: ${compared} evaluations of ${expressions.size} expressions agree`);
