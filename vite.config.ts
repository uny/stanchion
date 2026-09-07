import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The dev server is bound to localhost and exists only to serve the WebView.
// It is never exposed on the network.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    host: "127.0.0.1",
    port: 1420,
    strictPort: true,
    // The Vite root is the repository root, so the Cargo target directory sits inside it.
    // Every cargo rebuild writes thousands of files there; none of them are frontend input.
    // The workspace root is the repository root, so that directory is `target/`, not
    // `src-tauri/target/`. Vite merges this list with its own defaults, which cover
    // node_modules and the out dir but nothing of cargo's.
    watch: { ignored: ["**/src-tauri/**", "**/target/**"] },
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    target: "safari15",
  },
});
