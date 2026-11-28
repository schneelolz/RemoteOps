import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";

export default defineConfig({
  plugins: [vue()],
  server: {
    proxy: {
      "/api": {
        target: process.env.REMOTEOPS_ADMIN_API || "http://127.0.0.1:18081",
        changeOrigin: false,
      },
    },
  },
  preview: {
    proxy: {
      "/api": {
        target: process.env.REMOTEOPS_ADMIN_API || "http://127.0.0.1:18081",
        changeOrigin: false,
      },
    },
  },
  build: { sourcemap: false, manifest: true },
});
