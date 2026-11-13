import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

import pkg from "./package.json";

// Tauri expects a fixed port and does not want vite to clear the terminal.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  // Single source of truth for the version shown in the UI: `package.json`.
  // `scripts/check-version.mjs` keeps it in sync with Cargo.toml and
  // tauri.conf.json in CI.
  define: {
    __APP_VERSION__: JSON.stringify(pkg.version),
  },
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
  build: {
    target: "chrome110",
    minify: "esbuild",
    sourcemap: false,
  },
});
