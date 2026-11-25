import { fileURLToPath } from "node:url"
import tailwindcss from "@tailwindcss/vite"
import react from "@vitejs/plugin-react"
import { build, defineConfig, type Plugin } from "vite"

/** After the app, build the chat widget (widget.vite.config.ts) into dist/widgets, so every `vite build` ships it. */
const widget = (): Plugin => ({
  name: "norn-widget",
  apply: "build",
  async closeBundle() {
    await build({ configFile: fileURLToPath(new URL("./widget.vite.config.ts", import.meta.url)), logLevel: "warn" })
  },
})

export default defineConfig({
  plugins: [react(), tailwindcss(), widget()],
  // `@norn/react` (packages/norn-react) is imported by path: its React and UI SDK resolve from this
  // app, so the page runs one React and one UI SDK agent registry.
  resolve: { alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) }, dedupe: ["react", "react-dom", "@ui-sdk/core", "@ui-sdk/react"] },
  server: { fs: { allow: ["../.."] } },
  // The contact sheet (sheet.html), the read harness (harness.html, #1374) and the widget state-matrix harness
  // (matrix.html, #1762) build beside the app so a deployed stage serves them too.
  // The app's input stays `index`: its entry chunk is assets/index-<hash>.js, which the deploys' served-build checks read.
  build: {
    rollupOptions: {
      input: {
        index: fileURLToPath(new URL("./index.html", import.meta.url)),
        sheet: fileURLToPath(new URL("./sheet.html", import.meta.url)),
        harness: fileURLToPath(new URL("./harness.html", import.meta.url)),
        matrix: fileURLToPath(new URL("./matrix.html", import.meta.url)),
      },
      output: {
        codeSplitting: {
          groups: [
            { name: "vendor-react", test: /node_modules\/(react|react-dom|scheduler)\//, priority: 30 },
            { name: "vendor-parse5", test: /node_modules\/parse5\//, priority: 20 },
          ],
        },
      },
    },
  },
})
