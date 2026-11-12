import { fileURLToPath } from "node:url"
import tailwindcss from "@tailwindcss/vite"
import react from "@vitejs/plugin-react"
import { defineConfig, type Plugin } from "vite"

/**
 * The chat widget (MCP Apps): one self-contained HTML file, dist/widgets/norn.html. The hosts'
 * sandbox allows inline script and style and no network, so the script and stylesheet are inlined
 * and the web font is dropped (the theme's system-font fallback applies). The Worker serves this
 * file as each `ui://norn/<view>.html` resource.
 */
function singleFile(): Plugin {
  return {
    name: "norn-widget-single-file",
    enforce: "post",
    generateBundle(_options, bundle) {
      const html = Object.values(bundle).find((output) => output.type === "asset" && output.fileName.endsWith(".html"))
      if (html === undefined || html.type !== "asset") throw new Error("the widget build emitted no HTML")
      let page = String(html.source)
      for (const [name, output] of Object.entries(bundle)) {
        if (output.type === "chunk" && output.isEntry) {
          const code = output.code.replaceAll("</script", "<\\/script")
          page = page.replace(new RegExp(`<script[^>]*src="[^"]*${output.fileName.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}"[^>]*></script>`), () => `<script type="module">${code}</script>`)
          delete bundle[name]
        } else if (output.type === "asset" && output.fileName.endsWith(".css")) {
          // The sandbox loads no fonts: drop @font-face rules rather than reference unreachable files.
          const css = String(output.source).replace(/@font-face\s*\{[^}]*\}/g, "").replaceAll("</style", "<\\/style")
          page = page.replace(new RegExp(`<link[^>]*href="[^"]*${output.fileName.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}"[^>]*>`), () => `<style>${css}</style>`)
          delete bundle[name]
        } else if (output.type === "asset" && /\.(woff2?|ttf|otf)$/.test(output.fileName)) {
          delete bundle[name]
        }
      }
      if (/<script[^>]*\ssrc=|<link[^>]*stylesheet/.test(page)) throw new Error("the widget HTML still references an external script or stylesheet")
      // However a host loads the page, the browser must read UTF-8 before any text (an ellipsis, ≤).
      const charset = page.search(/<meta charset="utf-8"/i)
      if (charset < 0 || new TextEncoder().encode(page.slice(0, charset)).length > 900) throw new Error("the widget HTML must declare UTF-8 first")
      html.source = page
      html.fileName = "norn.html"
    },
  }
}

export default defineConfig({
  root: fileURLToPath(new URL(".", import.meta.url)),
  plugins: [react(), tailwindcss(), singleFile()],
  resolve: { alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) } },
  base: "./",
  // The widget is one file; Studio's public/ icons stay with the app.
  publicDir: false,
  build: {
    outDir: "dist/widgets",
    emptyOutDir: true,
    assetsInlineLimit: 0,
    cssCodeSplit: false,
    modulePreload: false,
    chunkSizeWarningLimit: 4096,
    rollupOptions: { input: fileURLToPath(new URL("./widget.html", import.meta.url)), output: { inlineDynamicImports: true } },
  },
})
