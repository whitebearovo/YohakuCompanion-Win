import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri dev server conventions: fixed port, no auto-open.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
  build: {
    target: "chrome120",
    outDir: "dist",
  },
});
