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
  resolve: { alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) } },
  server: { fs: { allow: ["../.."] } },
  // The contact sheet (sheet.html) builds beside the app so a deployed stage serves it too.
  // The app's input stays `index`: its entry chunk is assets/index-<hash>.js, which the deploys' served-build checks read.
  build: { rollupOptions: { input: { index: fileURLToPath(new URL("./index.html", import.meta.url)), sheet: fileURLToPath(new URL("./sheet.html", import.meta.url)) } } },
})
