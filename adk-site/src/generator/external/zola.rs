//! The Zola layout: `config.toml`, one content file per page with the context in
//! `extra.ctx`, and a Tera page template (spec 3.5 §7.8 "Generator").

use std::path::Path;

use serde_json::json;

use super::{JINJA_PAGE, PageContext, front_matter, write_file, write_starter};
use crate::error::Result;

const CONFIG: &str = r#"# Written by adk-site for Zola. adk-site writes sitemaps, feeds, robots.txt and
# search data itself; Zola's own are left out of the site.
base_url = "BASE_URL"
compile_sass = false
build_search_index = false
generate_feeds = false
"#;

const SECTION: &str = "---\nrender: false\n---\n";

/// Writes the project: the starter files the project lacks, then one page per context.
pub(super) fn write_project(dir: &Path, base_url: &str, pages: &[PageContext]) -> Result<()> {
    write_starter(dir, "config.toml", &CONFIG.replace("BASE_URL", base_url))?;
    write_starter(dir, "content/_index.md", SECTION)?;
    write_starter(dir, "templates/index.html", "")?;
    let template = format!(
        "{{%- set c = page.extra.ctx -%}}{{%- set s = c.site -%}}{{%- set p = c.page -%}}{{%- set t = c.t -%}}\n{JINJA_PAGE}"
    );
    write_starter(dir, "templates/adk-page.html", &template)?;
    for page in pages {
        let matter = json!({
            "title": page.title(),
            "path": format!("{}/{}", super::PAGES_DIR, page.n),
            "template": "adk-page.html",
            "extra": { "ctx": page.value() },
        });
        write_file(dir, &format!("content/adk-{}.md", page.n), &front_matter(&matter)?)?;
    }
    Ok(())
}
