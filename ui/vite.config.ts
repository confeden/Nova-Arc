import { defineConfig } from "vite";

// Tauri serves this on a fixed port in development and bundles `dist` for
// release. No analytics, no CDN: everything ships inside the binary.
export default defineConfig({
  clearScreen: false,
  server: { port: 5173, strictPort: true },
  build: { target: "chrome110", emptyOutDir: true },
});
