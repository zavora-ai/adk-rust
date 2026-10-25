- **`adk-site` external generators** (features `gen-zola`, `gen-hugo`, `gen-astro`,
  `gen-build-awesome`, spec 3.5 §7.8, Experimental): `ExternalGenerator` builds a site with
  Zola, Hugo, Astro or Build Awesome (Eleventy) run under `adk-sandbox`'s OS enforcer with no
  network, failing when no enforcer is available. The built-in renderer exports each page's
  template context into the generator's layout with a starter template, or into your own
  project with `with_project`; the generator renders the page HTML, and the Markdown and
  structured twins, site files, media and redirects stay the built-in build's. A page the
  generator does not write fails the build.
