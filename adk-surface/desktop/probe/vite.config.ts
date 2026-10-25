import { resolve } from "node:path";
import { defineConfig } from "vite";

// Two pages, embedded in the shell by `generate_context!` from ../dist (spec 3.3 §5.5): the page
// both windows show while the kernel cannot be reached, and the WebKitGTK probe.
export default defineConfig({
  base: "/",
  build: {
    outDir: "../dist",
    emptyOutDir: true,
    sourcemap: false,
    assetsInlineLimit: 0,
    rollupOptions: {
      input: {
        index: resolve(import.meta.dirname, "index.html"),
        probe: resolve(import.meta.dirname, "probe.html"),
      },
    },
  },
});
