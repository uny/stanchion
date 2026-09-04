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
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    target: "safari15",
  },
});
