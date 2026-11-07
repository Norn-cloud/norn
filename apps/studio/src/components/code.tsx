import { useEffect, useState } from "react"
import type { HighlighterCore } from "shiki/core"
import { cn } from "@/lib/utils"

let highlighter: Promise<HighlighterCore> | undefined

/** Shiki with only JSON and two themes, loaded on first use with the JavaScript regex engine (no WASM). */
const load = () => highlighter ??= Promise.all([import("shiki/core"), import("shiki/engine/javascript")]).then(([{ createHighlighterCore }, { createJavaScriptRegexEngine }]) =>
  createHighlighterCore({
    themes: [import("shiki/themes/github-light.mjs"), import("shiki/themes/github-dark.mjs")],
    langs: [import("shiki/langs/json.mjs")],
    engine: createJavaScriptRegexEngine(),
  }))

/**
 * A server document's text, highlighted. The text is shown as given, plain
 * until the highlighter loads; Shiki escapes it, so the HTML it returns holds
 * no markup from the document.
 */
export function Code({ code, className }: { readonly code: string; readonly className?: string }) {
  const [html, setHtml] = useState<string>()
  useEffect(() => {
    let live = true
    void load().then((shiki) => { if (live) setHtml(shiki.codeToHtml(code, { lang: "json", themes: { light: "github-light", dark: "github-dark" }, defaultColor: false })) })
    return () => { live = false }
  }, [code])
  const frame = cn("max-h-96 overflow-auto rounded-md border bg-background p-3 font-mono text-xs leading-relaxed", className)
  return html === undefined
    ? <pre className={frame}><code>{code}</code></pre>
    : <div className={cn("code", frame)} dangerouslySetInnerHTML={{ __html: html }} />
}
