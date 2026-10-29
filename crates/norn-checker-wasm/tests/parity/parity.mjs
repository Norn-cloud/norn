// Native-vs-wasm parity for the checker ABI (T1-CHK-4, #2527).
//
// Usage: node parity.mjs <pkg-dir> <native-out-dir>
//
// Loads the npm package built by scripts/build-pkg.sh, runs every case of
// cases.json through it, and requires each output to equal, byte for byte,
// the native output tests/parity.rs wrote to <native-out-dir>/<name>.out
// (NORN_CHECKER_WASM_PARITY_OUT). Arguments are built exactly as the native
// test builds them: envelope files joined verbatim into a JSON array.
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const [pkgDir, nativeDir] = process.argv.slice(2);
if (!pkgDir || !nativeDir) {
  console.error("usage: node parity.mjs <pkg-dir> <native-out-dir>");
  process.exit(2);
}

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "../../../..");
const abi = await import(pathToFileURL(resolve(pkgDir, "norn_checker_wasm.js")).href);
abi.initSync({ module: readFileSync(resolve(pkgDir, "norn_checker_wasm_bg.wasm")) });

const read = (path) => readFileSync(join(repoRoot, path), "utf8");
const withInlineLayout = (artifact) => {
  const graph = artifact.payload.graph;
  graph.viewport = { x: 12.5, y: -4, zoom: 1.25 };
  graph.nodes[0].description = "editor-only node description";
  graph.edges[0].targetHandle = "editor-target";
  return artifact;
};
const arrayText = (c, key) => {
  const withLayout = c.inlineLayoutOn === key;
  const artifacts = (c[key] ?? []).map((path) => {
    const source = read(path);
    return withLayout ? JSON.stringify(withInlineLayout(JSON.parse(source))) : source;
  });
  return `[${artifacts.join(",")}]`;
};
const artifactsText = (c) => c.artifactsText ?? arrayText(c, "artifacts");
const evaluationInputText = (c) =>
  c.inputJson ??
  (c.inputJsonBytes !== undefined
    ? `{"context":{"padding":"${"a".repeat(c.inputJsonBytes - 26)}"}}`
    : null) ??
  JSON.stringify({
    selection: Array.from({ length: c.inputSelectionCount ?? 0 }, () => ({ qty: 1 })),
  });
const contextText = (c) => {
  const members = [`"base":${arrayText(c, "base")}`];
  for (const [key, value] of Object.entries(c.context ?? {})) {
    members.push(`${JSON.stringify(key)}:${JSON.stringify(value)}`);
  }
  return `{${members.join(",")}}`;
};

const calls = {
  checkArtifactSet: (c) => abi.checkArtifactSet(artifactsText(c), contextText(c)),
  lifecycleDiagrams: (c) => abi.lifecycleDiagrams(artifactsText(c)),
  checkExpression: (c) =>
    abi.checkExpression(c.expr, artifactsText(c), c.expectedType, c.selfType),
  evaluateExpression: (c) => abi.evaluateExpression(c.expr, evaluationInputText(c), c.profile),
  canonicalRuntimeValue: (c) => abi.canonicalRuntimeValue(c.value),
  functionContract: (c) =>
    abi.functionContract(
      read(c.manifest),
      c.invocation ? read(c.invocation) : undefined,
      c.receipt ? read(c.receipt) : undefined,
    ),
  version: () => abi.version(),
};

const { cases } = JSON.parse(readFileSync(join(here, "cases.json"), "utf8"));
let failures = 0;
for (const c of cases) {
  let output;
  try {
    output = calls[c.call](c);
  } catch (error) {
    output = `error: ${error instanceof Error ? error.message : String(error)}`;
  }
  const native = readFileSync(join(nativeDir, `${c.name}.out`), "utf8");
  if (output === native) {
    console.log(`ok   ${c.name} (${Buffer.byteLength(output)} bytes)`);
  } else {
    failures += 1;
    console.log(`FAIL ${c.name}\n  native: ${native}\n  wasm:   ${output}`);
  }
}
console.log(`${cases.length - failures}/${cases.length} cases byte-identical`);
process.exit(failures === 0 ? 0 : 1);
