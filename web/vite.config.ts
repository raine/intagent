import react from "@vitejs/plugin-react"
import { defineConfig } from "vite"

export default defineConfig({
  plugins: [react()],
  build: {
    assetsInlineLimit: 100_000,
    copyPublicDir: false,
    cssCodeSplit: false,
    emptyOutDir: true,
    modulePreload: false,
    outDir: "generated",
    rollupOptions: {
      input: "src/main.tsx",
      output: {
        assetFileNames: (asset) =>
          asset.names.some((name) => name.endsWith(".css"))
            ? "app.css"
            : "[name][extname]",
        entryFileNames: "app.js",
        codeSplitting: false,
      },
    },
  },
  server: {
    proxy: {
      "/api": "http://127.0.0.1:4545",
    },
  },
})
