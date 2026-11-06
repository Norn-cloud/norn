// A request body read under a byte bound, never buffered past it: the stream is read
// chunk by chunk and cancelled as soon as the running total exceeds `max`. A
// Content-Length above the bound refuses before a byte is read. Returns the body as
// UTF-8 text, or `null` when it is too large.

/** @param {Request} request @param {number} max bytes @returns {Promise<string | null>} */
export async function boundedText(request, max) {
  const declared = Number(request.headers.get("content-length"));
  if (Number.isFinite(declared) && declared > max) return null;
  if (request.body === null) return "";
  const reader = request.body.getReader();
  const chunks = [];
  let total = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    total += value.byteLength;
    if (total > max) {
      // Stop the upload: nothing past the bound is read or kept.
      await reader.cancel("request body over the bound").catch(() => {});
      return null;
    }
    chunks.push(value);
  }
  const bytes = new Uint8Array(total);
  let at = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, at);
    at += chunk.byteLength;
  }
  return new TextDecoder("utf-8", { fatal: false }).decode(bytes);
}
