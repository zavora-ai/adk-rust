/// <reference types="vitest/config" />
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The web home is served by adk-surface at /ui/ and embedded from ../assets/web (spec 3.3 §6.1).
export default defineConfig({
  base: "/ui/",
  plugins: [react()],
  build: {
    outDir: "../assets/web",
    emptyOutDir: true,
    sourcemap: false,
    // Nothing inlined as data: URIs, so the page's content security policy stays strict.
    assetsInlineLimit: 0,
  },
  server: {
    // `npm run dev` against a running `adk-rust up` or `serve` on the default port.
    proxy: { "/api": "http://127.0.0.1:8080" },
  },
  test: {
    environment: "jsdom",
    include: ["tests/**/*.test.{ts,tsx}"],
    setupFiles: ["tests/setup.ts"],
  },
});
