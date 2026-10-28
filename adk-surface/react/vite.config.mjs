// The package is built with the web home's toolchain (`../web/node_modules`), so it adds no
// dependency of its own: this config imports nothing. React stays the host's.
export default {
  esbuild: { jsx: "automatic" },
  publicDir: false,
  build: {
    outDir: "dist",
    emptyOutDir: true,
    sourcemap: false,
    assetsInlineLimit: 0,
    cssCodeSplit: false,
    lib: { entry: "src/index.ts", formats: ["es"], fileName: "index", cssFileName: "style" },
    rollupOptions: { external: ["react", "react-dom", "react/jsx-runtime", "react-dom/client"] },
  },
};
