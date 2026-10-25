# Site generators

`adk-site` builds a site with its built-in renderer by default. With a `gen-*` feature,
`ExternalGenerator` builds it with Zola, Hugo, Astro or Build Awesome (Eleventy) instead, run as
a sandboxed process with no network (spec 3.5 §7.8 "Generator", §7.23 "Themes"). The site it
publishes carries the same Markdown and structured twins, `llms.txt`, `robots.txt`,
`security.txt`, sitemaps, feeds, JSON-LD, media and redirects as a built-in build, so it passes
the same launch gate.

> **Note:** `adk-site` is **Experimental**. Its API may change while spec 3.5 lands.

| Feature | Constructor | Needs |
|---------|-------------|-------|
| `gen-zola` | `ExternalGenerator::zola(binary)` | The `zola` binary |
| `gen-hugo` | `ExternalGenerator::hugo(binary)` | The `hugo` binary |
| `gen-astro` | `ExternalGenerator::astro(node, node_modules)` | Node.js and a `node_modules` holding `astro` |
| `gen-build-awesome` | `ExternalGenerator::build_awesome(node, node_modules)` | Node.js and a `node_modules` holding `@11ty/eleventy` |

```toml
[dependencies]
adk-site = { version = "3.0.0", features = ["gen-hugo"] }
```

## How a build runs

| Step | Method | What happens |
|------|--------|--------------|
| Export | `export` | The built-in renderer builds the site with templates that write each page's template context — `site`, `page` and `t`, the data the built-in templates read — instead of HTML. Every other file of that build is kept. The contexts become a project in the generator's layout |
| Run | `run` | The generator runs under `adk-sandbox`'s OS enforcer (Seatbelt on macOS, bubblewrap on Linux) with no network, reading only its program, its toolchain and the project, and writing only the project and its output. Without an enforcer the build fails |
| Assemble | `assemble` | Each page's HTML comes from the generator; stylesheets, scripts, images and fonts the generator adds are kept where they do not collide; every other file is the built-in build's. A page the generator did not write fails the build |

`ExternalGenerator` implements `SiteGenerator`, so `build` runs all three and a release is built
from its output as from any other build. Nothing is installed during a build: Astro and Build
Awesome run from the `node_modules` directory you pass.

```rust
use adk_site::content::SiteProject;
use adk_site::generator::SiteGenerator;
use adk_site::generator::external::ExternalGenerator;

async fn build(project: &SiteProject, out: &std::path::Path) -> adk_site::Result<()> {
    let hugo = ExternalGenerator::hugo("/opt/tools/hugo").with_project("/srv/site/theme");
    let report = hugo.build(project, out).await?;
    assert_eq!(report.generator.name, "hugo");
    Ok(())
}
```

## Project layouts

| Generator | Project | Page template | Writes each page to |
|-----------|---------|---------------|---------------------|
| Zola | `config.toml`, `content/adk-<n>.md` with the context in `extra.ctx` | `templates/adk-page.html` (Tera) | `adk-pages/<n>/index.html` |
| Hugo | `hugo.toml`, `content/adk-pages/<n>.md` with JSON front matter, the context in `params.ctx` | `layouts/adk-page.html` (Go templates) | `adk-pages/<n>/index.html` |
| Astro | `astro.config.mjs`, `src/adk/pages.json` | `src/pages/adk-pages/[n].astro` | `adk-pages/<n>/index.html` |
| Build Awesome | `eleventy.config.cjs`, `src/_data/adkPages.json` | `src/adk-page.njk` (Nunjucks, autoescaped) | `adk-pages/<n>/index.html` |

Assembly moves each page from `adk-pages/<n>/index.html` to its URL. The starter templates are
the built-in default theme's templates in each generator's language. With
`with_project(dir)`, your project is copied (without `node_modules`, `.git` or build output), the
starter files it lacks are added, and the content is written over it. A page template of your
own reads the same context: it must link the page's twins (`page.markdown`,
`page.machine_view`) and carry its JSON-LD (`page.json_ld`), which the launch gate checks.

Page bodies arrive rendered by adk-site's Markdown renderer, which escapes raw HTML and drops
unsafe URLs, so every generator publishes the same text; the generator owns the layout.

## Tolerances

Every file but the page HTML is byte-identical to the built-in build, and two builds with the same
generator version, project and input give byte-identical page HTML. With the starter templates,
Zola, Hugo and Build Awesome write the same page HTML; Astro differs in whitespace and writes
`<!DOCTYPE html>`.

## Tests

`adk-site/tests/generators.rs` exports the fixture site, assembles it from output each generator
wrote in a real run (recorded under `adk-site/tests/fixtures/generators/`), and runs the full
launch gate. Its `#[ignore]`d tests run the real generators, sandboxed, twice each:

```bash
ADK_SITE_ZOLA=/path/to/zola ADK_SITE_HUGO=/path/to/hugo \
ADK_SITE_NODE="$(command -v node)" ADK_SITE_ASTRO_MODULES=/path/to/astro/node_modules \
ADK_SITE_ELEVENTY_MODULES=/path/to/eleventy/node_modules \
  cargo nextest run -p adk-site --features gen-zola,gen-hugo,gen-astro,gen-build-awesome --run-ignored all --test generators
```

`ADK_SITE_GEN_RECORD=1` records the output again after a change to the fixture site, the
renderer or a starter template.
