import { defineConfig } from "vite";

// The daemon serves ui/dist at `/` in production. In dev, proxy the API, overlay
// previews and vendor files to it so the same relative URLs work.
const daemon = process.env.KUMA_DAEMON_URL ?? "http://127.0.0.1:7455";

export default defineConfig({
  server: {
    port: 5173,
    proxy: {
      "/api": { target: daemon, ws: true },
      "/overlay-preview": daemon,
      "/overlays": daemon,
      "/vendor": daemon,
      "/file": daemon,
    },
  },
  build: { outDir: "dist", emptyOutDir: true, sourcemap: true },
});
