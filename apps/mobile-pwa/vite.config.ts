import react from "@vitejs/plugin-react";
import { fileURLToPath } from "node:url";
import { defineConfig } from "vite";

const appEntry = fileURLToPath(new URL("./index.html", import.meta.url));
const serviceWorkerEntry = fileURLToPath(new URL("./src/service-worker.ts", import.meta.url));

export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      "@codex/bridge-protocol": new URL("../../packages/bridge-protocol/src/index.ts", import.meta.url).pathname,
    },
  },
  build: {
    rollupOptions: {
      input: {
        app: appEntry,
        sw: serviceWorkerEntry,
      },
      output: {
        entryFileNames: (chunk) =>
          chunk.name === "sw" ? "sw.js" : "assets/[name]-[hash].js",
        chunkFileNames: "assets/[name]-[hash].js",
        assetFileNames: "assets/[name]-[hash][extname]",
      },
    },
  },
  test: {
    environment: "jsdom",
    globals: true,
    include: ["src/**/*.test.{ts,tsx}"],
    setupFiles: "./src/test/setup.ts",
  },
});
