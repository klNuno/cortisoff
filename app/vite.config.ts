import { fileURLToPath, URL } from "node:url";

import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import tailwindcss from "@tailwindcss/vite";

// The dashboard is served to a webview on this machine only, so the port is
// fixed and the host is not: a Tauri dev build looks for exactly this URL.
export default defineConfig({
  plugins: [svelte(), tailwindcss()],
  resolve: {
    alias: {
      $lib: fileURLToPath(new URL("./src/lib", import.meta.url)),
    },
  },
  clearScreen: false,
  server: {
    port: 1421,
    strictPort: true,
  },
  build: {
    target: "esnext",
    // WebView2 and WKWebView both ship a current engine, so there is nothing
    // to down-level and a smaller bundle starts faster.
    minify: "esbuild",
    sourcemap: false,
  },
});
