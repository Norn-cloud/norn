/**
 * The widget page for one view: the built page (dist/widgets/norn.html) naming the view and
 * Studio's origin in two meta tags, placed after the charset declaration so that declaration stays
 * the first thing a browser reads, however the host loads the page (srcdoc, a blob, a sandbox
 * origin). Null when the page is not the built widget or declares no UTF-8 charset early.
 */
export const WIDGET_MARKER = '<meta name="norn-widget" content="1"'

/** Browsers look for the charset declaration in the first 1024 bytes. */
const CHARSET_WINDOW = 1024

export function viewPage(html: string, view: string, studioOrigin: string): string | null {
  if (!html.includes(WIDGET_MARKER)) return null
  const charset = /<meta charset="utf-8"\s*\/?>/i.exec(html)
  if (charset === null || new TextEncoder().encode(html.slice(0, charset.index + charset[0].length)).length > CHARSET_WINDOW) return null
  const at = charset.index + charset[0].length
  const attribute = (value: string) => value.replaceAll("&", "&amp;").replaceAll('"', "&quot;").replaceAll("<", "&lt;")
  return `${html.slice(0, at)}<meta name="norn-view" content="${attribute(view)}"><meta name="norn-studio" content="${attribute(studioOrigin)}">${html.slice(at)}`
}
