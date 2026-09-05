import { defineConfig } from "vite";

export default defineConfig({
  // Tauri expects a fixed port.
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
  },
  build: {
    target: "es2021",
  },
});