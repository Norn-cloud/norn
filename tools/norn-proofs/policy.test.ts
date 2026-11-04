import { expect, test } from "bun:test"
import { mkdtemp, rm, writeFile } from "node:fs/promises"
import { join } from "node:path"
import { tmpdir } from "node:os"
import { runProcess, environment, assetPins, diagnosticCommitment, sha } from "./host"
import { decodeTrace, oracleVerdict, parity, verdictText } from "./oracle"
import { integer, parseJob } from "./protocol"
import { checkFinite } from "./core"

const trace = [{ state: { present: false, v: 0 } }, { state: { present: true, v: 1 } }]
const itf = { vars: ["present", "v"], states: trace.map(({ state }, index) =>
  ({ "#meta": { index }, present: state.present, v: { "#bigint": String(state.v) } })) }
const success = "The outcome is: NoError I@00:00\n"
const failure = "State 1: state invariant 0 violated. I@00:00\nThe outcome is: Error I@00:00\n"

test("logical asset commitments survive relocation and detect changed content", async () => {
  const directory = await mkdtemp(join(tmpdir(), "norn-tool-assets-"))
  try {
    const left = join(directory, "checkout-a"), right = join(directory, "checkout-b")
    await writeFile(left, "same tool bytes"); await writeFile(right, "same tool bytes")
    const name = "tool/1.2.3/binary"
    expect(await assetPins([[name, left]])).toEqual(await assetPins([[name, right]]))
    expect(JSON.stringify(await assetPins([[name, left]]))).not.toContain(directory)
    await writeFile(right, "changed tool bytes")
    expect(await assetPins([[name, left]])).not.toEqual(await assetPins([[name, right]]))
    for (const entries of [[["/host/tool", left]], [["../tool", left]], [[name, left], [name, right]]] satisfies [string, string][][]) {
      await expect(assetPins(entries)).rejects.toThrow()
    }
  } finally { await rm(directory, { recursive: true, force: true }) }
})

test("retained verdict markers preserve qualification without diagnostic host paths", () => {
  for (const stdout of [success, failure, success + success, "", "The outcome is: Other\n",
    `/home/example/private.tla\n${failure}diagnostic /tmp/tool.log\n`]) {
    const markers = verdictText(stdout)
    expect(markers).not.toContain("/home/"); expect(markers).not.toContain("/tmp/")
    expect(diagnosticCommitment(stdout)).toEqual({ bytes: Buffer.byteLength(stdout), sha256: sha(stdout) })
    for (const code of [0, 1, 12, 75, 255]) {
      expect(oracleVerdict({ kind: "exited", code, stdout: markers, stderr: "" }, itf).status)
        .toBe(oracleVerdict({ kind: "exited", code, stdout, stderr: "" }, itf).status)
    }
  }
})

test("strict UTF-8 transport preserves BOM bytes for exact diagnostic commitments", async () => {
  const bytes = Buffer.from([239, 187, 191, 97])
  const result = await runProcess({ command: [process.execPath, "-e", "process.stdout.write(Buffer.from([239,187,191,97]))"],
    input: "", env: environment("/nonexistent"), timeoutMs: 2000, maxBytes: 100 })
  expect(result.kind).toBe("exited")
  expect(diagnosticCommitment(result.stdout)).toEqual({ bytes: bytes.length, sha256: sha(bytes) })
})

test("universal success requires a drained nonempty search with exact cardinality and oracle agreement", () => {
  const primary = { status: "success", distinctStates: 78, queuedStates: 0, initialStates: { distinct: 1 } }
  expect(parity(primary, { status: "ok" }, 78).status).toBe("ok")
  for (const change of [{ distinctStates: 77 }, { queuedStates: 1 }, { initialStates: { distinct: 0 } },
    { initialStates: undefined }, { status: "unknown" }]) {
    expect(parity({ ...primary, ...change }, { status: "ok" }, 78).status).toBe("unknown")
  }
  expect(parity(primary, { status: "unknown", reason: "timeout" }, 78).status).toBe("unknown")
})

test("refutations and witnesses require qualified reference exits and equal nonempty trace shapes", () => {
  const primary = { status: "invariant-violation", violation: { trace } }
  const oracle = oracleVerdict({ kind: "exited", code: 12, stdout: failure, stderr: "" }, itf)
  expect(parity(primary, oracle, 78).status).toBe("violation")
  for (const changed of [[], [{ state: { present: false, v: 0 } }],
    [{ state: { present: true, v: 0 } }, trace[1]],
    [{ state: { present: false, other: 0 } }, trace[1]],
    [{ state: { present: false, v: false } }, trace[1]]]) {
    expect(parity({ ...primary, violation: { trace: changed } }, oracle, 78).status).toBe("unknown")
  }
  for (const [code, stdout] of [[1, failure], [12, success], [0, failure], [75, success], [255, success],
    [12, "The outcome is: Error"], [0, success + success]] satisfies [number, string][]) {
    expect(oracleVerdict({ kind: "exited", code, stdout, stderr: "" }, itf).status).toBe("unknown")
  }
  expect(oracleVerdict({ kind: "unknown", reason: "deadline", timedOut: true, stdout: success, stderr: "" }, itf).status).toBe("unknown")
})

test("ITF integers and seed transport refuse rounding, malformed shapes and trailing line terminators", () => {
  expect(decodeTrace(itf)).toEqual(trace)
  for (const bad of [0.5, 2, "2", { "#bigint": "1.25" }, { "#bigint": "+1" },
    { "#bigint": "01" }, { "#bigint": "-0" }, { "#bigint": "2147483648" }, { "#bigint": "1\n" }]) {
    expect(() => decodeTrace({ vars: ["v"], states: [{ "#meta": { index: 0 }, v: bad }] })).toThrow()
  }
  for (const bad of [null, {}, { vars: ["v", "v"], states: [] },
    { vars: ["v"], states: [{ "#meta": { index: false }, v: true }] },
    { vars: ["v"], states: [{ "#meta": { index: 0 }, other: true }] }]) expect(() => decodeTrace(bad)).toThrow()
  expect(integer("18446744073709551615", 0n, 18446744073709551615n)).toBe(18446744073709551615n)
  for (const bad of [null, 1, "01", "1\n", "-0", "18446744073709551616"]) {
    expect(() => integer(bad, 0n, 18446744073709551615n)).toThrow()
  }
})

test("generated job boundary refuses missing or malformed fields visibly", () => {
  const good = { key: "original-0", model: "generated.tla", result: "result.json", modelSha: "a".repeat(64),
    challenge: "b".repeat(64), invariant: "law", step: "step", seed: "18446744073709551615", expectedStates: 78 }
  expect(parseJob(good).seed).toBe("18446744073709551615")
  for (const bad of [null, [], 1, {}, { ...good, seed: 1 }, { ...good, step: "opaque" },
    { ...good, expectedStates: 1.5 }, { ...good, modelSha: `${good.modelSha}\n` }]) expect(() => parseJob(bad)).toThrow()
})

test("process deadline includes killing and reaping a real TERM-ignoring child", async () => {
  const start = performance.now()
  const result = await runProcess({ command: [process.execPath, "-e",
    "process.on('SIGTERM',()=>{}); console.log(process.pid); setInterval(()=>{},1000)"], input: "",
    env: environment("/nonexistent"), timeoutMs: 800, maxBytes: 1024 })
  expect(result.kind).toBe("unknown")
  if (result.kind === "unknown") expect(result.timedOut).toBe(true)
  expect(performance.now() - start).toBeLessThan(1200)
  const pid = Number(result.stdout.trim())
  expect(Number.isSafeInteger(pid) && pid > 0).toBe(true)
  expect(() => process.kill(pid, 0)).toThrow()
})

test("process output budget is bounded and invalid UTF-8 stays Unknown", async () => {
  for (const script of ["process.stdout.write('x'.repeat(4096))", "process.stdout.write(Buffer.from([255]))"]) {
    const result = await runProcess({ command: [process.execPath, "-e", script], input: "", env: environment("/nonexistent"),
      timeoutMs: 2000, maxBytes: 100 })
    expect(result.kind).toBe("unknown")
  }
})

test("real plain TLA checking refuses empty initialization and incorrect cardinality", async () => {
  const source = "---- MODULE generated ----\nEXTENDS Integers\nVARIABLES present, v\nq_init == present = FALSE /\\ v = 0\nq_step == UNCHANGED <<present,v>>\nq_inv == TRUE\n===="
  const options = { source, config: "INIT q_init NEXT q_step INVARIANT q_inv", seed: 1n, expectedStates: 1 }
  expect((await checkFinite(options)).status).toBe("success")
  expect((await checkFinite({ ...options, expectedStates: 2 })).status).toBe("unknown")
  expect((await checkFinite({ ...options, source: source.replace("present = FALSE", "FALSE") })).status).toBe("unknown")
  expect((await checkFinite({ ...options, source: source.replace("v = 0", "v = 128") })).status).toBe("unknown")
})
