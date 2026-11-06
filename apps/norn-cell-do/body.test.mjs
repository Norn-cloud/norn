// node --test apps/norn-cell-do/body.test.mjs
import assert from "node:assert/strict";
import { test } from "node:test";
import { boundedText } from "./body.mjs";

/** A streamed request whose body is `chunks`, recording how many chunks were pulled. */
function streamed(chunks, headers = {}) {
  const pulled = { count: 0, cancelled: false };
  const body = new ReadableStream({
    pull(controller) {
      const next = chunks[pulled.count++];
      if (next === undefined) controller.close();
      else controller.enqueue(new TextEncoder().encode(next));
    },
    cancel() {
      pulled.cancelled = true;
    },
  });
  const request = new Request("https://cell.test/v1/questions:evaluate", { method: "POST", body, headers, duplex: "half" });
  return { request, pulled };
}

test("a body within the bound reads whole, as UTF-8", async () => {
  const { request } = streamed(["{\"question\":", " \"gold é\"}"]);
  assert.equal(await boundedText(request, 64), "{\"question\": \"gold é\"}");
});

test("a body over the bound stops at the first chunk past it, without reading the rest", async () => {
  const { request, pulled } = streamed(["a".repeat(40), "b".repeat(40), "c".repeat(40), "d".repeat(40)]);
  assert.equal(await boundedText(request, 64), null);
  assert.equal(pulled.cancelled, true, "the upload is cancelled");
  assert.ok(pulled.count <= 3, `read ${pulled.count} chunks; the bound was passed on the second`);
});

test("the bound counts bytes, not characters", async () => {
  // Three 2-byte characters are six bytes.
  assert.equal(await boundedText(streamed(["ééé"]).request, 5), null);
  assert.equal(await boundedText(streamed(["ééé"]).request, 6), "ééé");
});

test("a declared Content-Length over the bound refuses before any byte is read", async () => {
  // Node's Request drops a caller-set content-length on a streamed body; an incoming
  // Workers request keeps it. The reader needs only headers and body.
  // highWaterMark 0: the stream pulls only when read, so any pull is the reader's.
  let pulls = 0;
  const body = new ReadableStream({ pull(controller) { pulls += 1; controller.close(); } }, { highWaterMark: 0 });
  const declared = { headers: new Headers({ "content-length": "100000" }), body };
  assert.equal(await boundedText(declared, 64), null);
  assert.equal(pulls, 0);
});

test("an empty body is empty text", async () => {
  assert.equal(await boundedText(new Request("https://cell.test/", { method: "POST" }), 64), "");
});
