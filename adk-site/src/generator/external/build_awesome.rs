//! The Build Awesome (Eleventy) layout: `eleventy.config.cjs`, the contexts as global data,
//! and a Nunjucks page template paginated over them, autoescaped (spec 3.5 §7.8
//! "Generator").

use std::path::Path;

use serde_json::Value;

use super::{JINJA_PAGE, PageContext, json_file, write_file, write_starter};
use crate::error::Result;

const CONFIG: &str = r#"// Written by adk-site for Build Awesome (Eleventy).
module.exports = function (eleventyConfig) {
  eleventyConfig.setNunjucksEnvironmentOptions({ autoescape: true });
  return { dir: { input: "src" }, templateFormats: ["njk"] };
};
"#;

const FRONT_MATTER: &str = r#"---
pagination:
  data: adkPages
  size: 1
  alias: c
permalink: "/PAGES_DIR/{{ c.n }}/index.html"
eleventyExcludeFromCollections: true
---
{%- set s = c.site -%}{%- set p = c.page -%}{%- set t = c.t -%}
"#;

/// Writes the project: the starter files the project lacks, then the contexts.
pub(super) fn write_project(dir: &Path, _base_url: &str, pages: &[PageContext]) -> Result<()> {
    write_starter(dir, "eleventy.config.cjs", CONFIG)?;
    let template = format!("{}{JINJA_PAGE}", FRONT_MATTER.replace("PAGES_DIR", super::PAGES_DIR));
    write_starter(dir, "src/adk-page.njk", &template)?;
    let data = Value::Array(pages.iter().map(PageContext::value).collect());
    write_file(dir, "src/_data/adkPages.json", &json_file(&data)?)
}
