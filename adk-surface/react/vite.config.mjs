// The package is built with the web home's toolchain (`../web/node_modules`): this config
// imports nothing. React stays the host's. `three`, its one dependency, stays external, so the
// host's bundler puts it in the lazily loaded 3D chunk and shares one copy with the host.
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
    rollupOptions: { external: ["react", "react-dom", "react/jsx-runtime", "react-dom/client", "three", /^three\//] },
  },
};
