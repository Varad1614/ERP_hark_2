import { reactRouter } from "@react-router/dev/vite";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [reactRouter()],
  // Pin the preview server to IPv4 loopback. The SPA build prerenders index.html through
  // `vite preview`; inside Docker, `localhost` can resolve to ::1 for the listener but to
  // 127.0.0.1 for the request, which fails with ECONNREFUSED.
  preview: { host: "127.0.0.1" },
  server: {
    port: 5173,
    // Same-origin API in dev so the HttpOnly session cookie works without CORS.
    proxy: {
      "/api": { target: process.env.SUTRA_API_URL ?? "http://localhost:8080", changeOrigin: false },
      "/health": { target: process.env.SUTRA_API_URL ?? "http://localhost:8080" },
    },
  },
});