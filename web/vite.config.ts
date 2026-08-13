import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  build: {
    outDir: "dist",
    // The Elysia server serves this directory; nothing else reads it.
    emptyOutDir: true,
  },
  server: {
    // In development Vite serves the app and forwards everything else to the
    // Elysia server, so the browser talks to one origin either way.
    proxy: {
      "/api": {
        target: process.env.ASABORAKE_WEB_TARGET ?? "http://127.0.0.1:3001",
        changeOrigin: true,
      },
    },
  },
});
