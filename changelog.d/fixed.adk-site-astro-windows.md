- **Astro builds on Windows** (`adk-site` `gen-astro`, spec 3.5 §7.8 "Generator"): the project's
  `node_modules` links the toolchain's packages with Windows symbolic links, as it does on Unix,
  where an Astro build on Windows used to fail with "Astro builds need a Unix host for the
  sandbox". Windows creates symbolic links in Developer Mode or as an administrator.
