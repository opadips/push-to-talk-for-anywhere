import { svelte } from "@sveltejs/vite-plugin-svelte";
import { defineConfig } from "vite";

// Tauri expects a fixed dev port (tauri.conf.json -> build.devUrl).
export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  build: {
    target: "esnext",
    minify: "es2021",
    sourcemap: Boolean(process.env.TAURI_DEBUG),
  },
});
