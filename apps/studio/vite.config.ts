import { fileURLToPath } from "node:url"
import tailwindcss from "@tailwindcss/vite"
import react from "@vitejs/plugin-react"
import { defineConfig } from "vite"

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: { alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) } },
  server: { fs: { allow: ["../.."] } },
  // The contact sheet (sheet.html) builds beside the app so a deployed stage serves it too.
  // The app's input stays `index`: its entry chunk is assets/index-<hash>.js, which the deploys' served-build checks read.
  build: { rollupOptions: { input: { index: fileURLToPath(new URL("./index.html", import.meta.url)), sheet: fileURLToPath(new URL("./sheet.html", import.meta.url)) } } },
})
