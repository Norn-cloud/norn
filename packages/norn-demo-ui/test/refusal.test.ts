import { expect, test } from "bun:test"
import type { Schemas } from "../../norn-sdk/src/index.ts"
import { renderRefusalDocument, type RefusalResponse } from "../src/refusal.ts"

type Refusal = Schemas["Refusal"]

const read = (source: string): RefusalResponse => ({ refusal: JSON.parse(source) as Refusal, source })

// Synthetic responses in the `norn/refusal@2` wire shape; decimals keep their scale.
const omittedLoss = read(`{"format":"norn/refusal@2","code":"decision.invariant_violation","category":"precondition",
  "message":"a declared precondition does not hold","explanation":"Input fine mass must equal output plus recorded loss.",
  "retryable":false,"reason":{"law":"fine_mass_conservation","subject":"demo.MeltMeasurement/m-1",
  "pin":{"kind":"invariant","owner":{"kind":"interface","name":"demo.ConservedFineMass"},"invariantId":"fine_mass_conservation","version":"sha256:4f2c"},
  "values":{"input_fine_g":100.000,"output_fine_g":99.200,"loss_fine_g":0.000,"metal":"gold"}}}`)

const concealed = read(`{"format":"norn/refusal@2","code":"authz.denied","category":"permission","message":"the action is not permitted",
  "explanation":"The server has not proven authority to disclose this diagnostic.","retryable":false,"reason":null}`)

test("operands keep the exact decimal text of the response", () => {
  const html = renderRefusalDocument(omittedLoss)
  expect(html).toContain('<code class="exact">100.000</code>')
  expect(html).toContain('<code class="exact">99.200</code>')
  expect(html).toContain('<code class="exact">0.000</code>')
  expect(html).toContain("<code>gold</code>")
  expect(html).not.toContain(">99.2<")
})

test("the declaration pin, reason and code are shown as disclosed", () => {
  const html = renderRefusalDocument(omittedLoss)
  for (const text of ["decision.invariant_violation", "fine_mass_conservation", "demo.ConservedFineMass", "sha256:4f2c", "demo.MeltMeasurement/m-1", "precondition"]) {
    expect(html).toContain(text)
  }
  expect(html).toContain("Not marked retryable by the server.")
})

test("a refused action is shown as a refused commitment with no effects", () => {
  for (const response of [omittedLoss, concealed]) {
    const html = renderRefusalDocument(response)
    expect(html).toContain('aria-current="step">Refused</li>')
    expect(html).toContain("A refused action commits no effects")
    expect(html).toContain("TXN-ATOMIC-1")
    expect(html).toContain("A repair hint is not exposed by the server.")
  }
})

test("an undisclosed reason stays absent", () => {
  const html = renderRefusalDocument(concealed)
  expect(html).toContain("The server did not disclose the governing law or its values for this refusal.")
  expect(html).not.toContain("Values at refusal")
  expect(html).not.toContain("Definition digest")
  expect(html).toContain("The server has not proven authority to disclose this diagnostic.")
})

test("law and built-in pins name their declaration", () => {
  const law = read(omittedLoss.source.replace(/"pin":\{[^}]*\{[^}]*\}[^}]*\}/, '"pin":{"kind":"law","artifactId":"demo.fine_mass_conservation.law","lawId":"demo.fine_mass_conservation","version":"sha256:91ab"}'))
  expect(renderRefusalDocument(law)).toContain("<code>demo.fine_mass_conservation.law</code>")
  const builtin = read(omittedLoss.source.replace(/"pin":\{[^}]*\{[^}]*\}[^}]*\}/, '"pin":{"kind":"builtin","id":"lifecycle.undeclared_write","version":"1"}'))
  expect(renderRefusalDocument(builtin)).toContain("Built-in rule")
  const unpinned = read(omittedLoss.source.replace(/"pin":\{[^}]*\{[^}]*\}[^}]*\},/, ""))
  expect(renderRefusalDocument(unpinned)).toContain("The server did not pin a declaration for this reason.")
})

test("server text never becomes markup", () => {
  const hostile = '</code><script>alert("x")</script>'
  const encoded = JSON.stringify(hostile)
  const response = read(`{"format":"norn/refusal@2","code":${encoded},"category":"invalid","message":${encoded},"explanation":${encoded},
    "retryable":true,"reason":{"law":${encoded},"subject":${encoded},"pin":null,"values":{${encoded}:${encoded}}}}`)
  const html = renderRefusalDocument(response)
  expect(html).not.toContain("<script>")
  expect(html).toContain("&lt;script&gt;")
  expect(html).toContain("The server marks this refusal retryable.")
})

test("a source that is not the same response refuses to render", () => {
  expect(() => renderRefusalDocument({ refusal: omittedLoss.refusal, source: concealed.source })).toThrow()
  expect(() => renderRefusalDocument({ refusal: omittedLoss.refusal, source: omittedLoss.source.replace('"metal":"gold"', '"purity":"0.999"') })).toThrow()
  expect(() => renderRefusalDocument({ refusal: omittedLoss.refusal, source: '{"reason":{"values":{"input_fine_g":100.000' })).toThrow()
  expect(() => renderRefusalDocument({ refusal: omittedLoss.refusal, source: omittedLoss.source.replace("99.200", "99.300") })).toThrow()
  expect(() => renderRefusalDocument({ refusal: omittedLoss.refusal, source: omittedLoss.source.replace("sha256:4f2c", "sha256:0000") })).toThrow()
  expect(() => renderRefusalDocument({ refusal: omittedLoss.refusal, source: `${omittedLoss.source} {"extra":1}` })).toThrow()
  expect(() => renderRefusalDocument({ refusal: { ...concealed.refusal, message: "edited" }, source: concealed.source })).toThrow()
})

test("an equal response in another member order renders", () => {
  const reordered = `{"reason":null,"retryable":false,"explanation":"The server has not proven authority to disclose this diagnostic.",
    "message":"the action is not permitted","category":"permission","code":"authz.denied","format":"norn/refusal@2"}`
  expect(renderRefusalDocument({ refusal: concealed.refusal, source: reordered })).toContain("authz.denied")
})
