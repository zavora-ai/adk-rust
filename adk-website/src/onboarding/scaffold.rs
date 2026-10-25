//! The site project a new site starts from (spec 3.5 §7.17: "scaffolds the site project
//! (content, theme, charter, data)").
//!
//! | Path | Holds |
//! |---|---|
//! | `site.toml` | Name, domain, languages, the kind's site rules and capabilities |
//! | `theme/` | `theme.toml` with the chosen theme's tokens, `style.css` with its dark scheme |
//! | `charter.draft.json` | Charter version 1 drafted from the kind's template, unsigned |
//! | `capabilities.json`, `public-terms.json` | The kind's capabilities and the charter's public terms |
//! | `content/`, `data/` | The content repository and the site's stores |
//! | `public/index.html` | The holding page served until launch |

use std::path::{Path, PathBuf};

use adk_core::{AdkError, Result};
use adk_harness::website::SiteRef;
use adk_site::content::SiteConfig;
use awp_types::{LanguageTag, SiteKind};
use serde::Serialize;

use super::holding_page;
use crate::kinds::{CharterInputs, template};
use crate::team::SiteTeam;
use crate::themes::{ThemeId, theme};

/// What to scaffold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScaffoldOptions {
    /// The site's name.
    pub name: String,
    /// Its domain.
    pub domain: String,
    /// Its kind.
    pub kind: SiteKind,
    /// Its languages; the first is the default.
    pub languages: Vec<LanguageTag>,
    /// Its theme.
    pub theme: ThemeId,
    /// The budget's currency.
    pub currency: String,
}

#[derive(Serialize)]
struct ThemeFile<'a> {
    name: &'a str,
    tokens: &'a adk_site::theme::DesignTokens,
}

fn io(path: &Path, error: impl std::fmt::Display) -> AdkError {
    AdkError::config(format!("cannot write {}: {error}", path.display()))
}

fn pretty<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    serde_json::to_vec_pretty(value).map_err(|e| AdkError::config(e.to_string()))
}

async fn write(path: PathBuf, contents: impl AsRef<[u8]>) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| io(parent, e))?;
    }
    tokio::fs::write(&path, contents).await.map_err(|e| io(&path, e))
}

/// Writes a new site project into `dir` (spec 3.5 §7.17); refuses a directory that already
/// holds a `site.toml`. Returns the files written, relative to `dir`.
///
/// # Errors
///
/// Returns a refusal for an existing project, or the file system's error.
pub async fn scaffold(dir: &Path, options: &ScaffoldOptions) -> Result<Vec<String>> {
    if tokio::fs::try_exists(dir.join("site.toml")).await.unwrap_or(false) {
        return Err(AdkError::config(format!(
            "{} already holds a site project; choose an empty directory",
            dir.display()
        )));
    }
    let template = template(options.kind);
    let id = uuid::Uuid::new_v4();
    let site =
        SiteRef::new(id, options.domain.replace('.', "-"), format!("https://{}", options.domain));
    let team = SiteTeam::new(site, &template);
    let mut config = SiteConfig::new(&options.name, &options.domain, options.languages.clone());
    template.configure_site(&mut config);
    config.validate().map_err(|e| AdkError::config(e.to_string()))?;
    let inputs = CharterInputs::new(
        id,
        &options.name,
        "owner",
        format!("What {} is for, in the owner's words.", options.name),
        options.languages.clone(),
        &options.currency,
    );
    let charter = template.draft_charter(&team, &inputs);
    let chosen = theme(options.theme);
    let files: Vec<(&str, Vec<u8>)> = vec![
        (
            "site.toml",
            toml::to_string_pretty(&config)
                .map_err(|e| AdkError::config(e.to_string()))?
                .into_bytes(),
        ),
        (
            "theme/theme.toml",
            toml::to_string_pretty(&ThemeFile { name: &chosen.name, tokens: &chosen.tokens })
                .map_err(|e| AdkError::config(e.to_string()))?
                .into_bytes(),
        ),
        ("theme/style.css", chosen.css.clone().into_bytes()),
        ("charter.draft.json", pretty(&charter)?),
        ("capabilities.json", pretty(&template.capabilities())?),
        ("public-terms.json", pretty(&charter.public)?),
        ("content/.keep", Vec::new()),
        ("data/.keep", Vec::new()),
        ("public/index.html", holding_page(&options.name).into_bytes()),
    ];
    let mut written = Vec::new();
    for (path, contents) in files {
        write(dir.join(path), contents).await?;
        written.push(path.to_string());
    }
    Ok(written)
}
