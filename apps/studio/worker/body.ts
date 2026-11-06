/**
 * A request body as text, or `undefined` once it is longer than `limit` bytes.
 * Reading stops at the limit, so an oversized body is never buffered whole.
 */
export async function boundedText(request: Request, limit: number): Promise<string | undefined> {
  if (Number(request.headers.get("content-length") ?? "0") > limit) return undefined
  if (request.body === null) return ""
  const reader = request.body.getReader()
  const chunks: Uint8Array[] = []
  let size = 0
  for (;;) {
    const { done, value } = await reader.read()
    if (done) break
    size += value.byteLength
    if (size > limit) {
      await reader.cancel()
      return undefined
    }
    chunks.push(value)
  }
  const bytes = new Uint8Array(size)
  let offset = 0
  for (const chunk of chunks) {
    bytes.set(chunk, offset)
    offset += chunk.byteLength
  }
  return new TextDecoder().decode(bytes)
}
