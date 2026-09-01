//! Remote skill loading: Skill Registry packages as a [`SkillIndex`].
//!
//! Skills fetched from the registry flow through the exact same
//! [`SkillDocument`] construction as filesystem skills (the shared
//! [`parse_skill_markdown`](crate::parse_skill_markdown) path), so
//! [`SkillInjector`](crate::SkillInjector), [`SelectionPolicy`](crate::SelectionPolicy),
//! and [`ContextCoordinator`](crate::ContextCoordinator) work unchanged on a
//! registry-backed index.

use crate::error::{SkillError, SkillResult};
use crate::index::build_document;
use crate::model::{SkillDocument, SkillIndex};
use crate::parser::parse_skill_markdown;
use crate::registry::client::{SkillContent, SkillRegistryClient};
use std::path::PathBuf;

/// How [`load_skill_index_from_registry`] selects skills.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistrySkillSelector {
    /// Semantic search over skill display names and descriptions
    /// (`skills:retrieve`).
    Query {
        /// Query text.
        query: String,
        /// Maximum results (server default 10, max 100).
        top_k: Option<u32>,
    },
    /// Explicit skill IDs or full `projects/*/locations/*/skills/*`
    /// resource names.
    Names(Vec<String>),
}

/// Selection filter for [`load_skill_index_from_registry`].
///
/// Selects skills either by semantic search query or by explicit names, with
/// an optional revision pin.
///
/// # Example
///
/// ```
/// use adk_skill::registry::RegistrySkillFilter;
///
/// let by_query = RegistrySkillFilter::by_query("data analysis").with_top_k(5);
/// let pinned = RegistrySkillFilter::by_names(["report-writer"]).with_revision("3");
/// # let _ = (by_query, pinned);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistrySkillFilter {
    /// The skill selector.
    pub selector: RegistrySkillSelector,
    /// Optional revision ID pin applied to every selected skill.
    ///
    /// Defaults to the latest revision (`skills.get` carries the latest
    /// payload). Whether pinned revision snapshots carry a payload is
    /// undocumented; see
    /// [`fetch_skill_revision_content`](SkillRegistryClient::fetch_skill_revision_content).
    pub revision: Option<String>,
}

impl RegistrySkillFilter {
    /// Selects skills by semantic search query.
    pub fn by_query(query: impl Into<String>) -> Self {
        Self {
            selector: RegistrySkillSelector::Query { query: query.into(), top_k: None },
            revision: None,
        }
    }

    /// Selects skills by explicit IDs or full resource names.
    pub fn by_names<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            selector: RegistrySkillSelector::Names(names.into_iter().map(Into::into).collect()),
            revision: None,
        }
    }

    /// Caps query results (server default 10, max 100). Ignored for
    /// name-based selection.
    #[must_use]
    pub fn with_top_k(mut self, top_k: u32) -> Self {
        if let RegistrySkillSelector::Query { top_k: slot, .. } = &mut self.selector {
            *slot = Some(top_k);
        }
        self
    }

    /// Pins every selected skill to a revision ID (default: latest).
    #[must_use]
    pub fn with_revision(mut self, revision: impl Into<String>) -> Self {
        self.revision = Some(revision.into());
        self
    }
}

/// Loads a [`SkillIndex`] from the Skill Registry.
///
/// Resolves the filter to skill names (via `skills:retrieve` for queries),
/// downloads each skill's verified payload with
/// [`fetch_skill_content`](SkillRegistryClient::fetch_skill_content) (or
/// [`fetch_skill_revision_content`](SkillRegistryClient::fetch_skill_revision_content)
/// when a revision is pinned), and parses the packaged `SKILL.md` through the
/// standard [`parse_skill_markdown`](crate::parse_skill_markdown) path.
///
/// Registry-backed documents carry a virtual `path` of
/// `{resource name}/SKILL.md` and no `last_modified` timestamp; every
/// content-derived field (`id`, `hash`, frontmatter, body) is built exactly
/// as for filesystem skills.
///
/// # Example
///
/// ```no_run
/// use adk_skill::registry::{
///     RegistrySkillFilter, SkillRegistryClient, SkillRegistryConfig,
///     load_skill_index_from_registry,
/// };
///
/// # async fn load() -> adk_skill::SkillResult<()> {
/// let config = SkillRegistryConfig::new("my-project", "us-central1");
/// let client = SkillRegistryClient::new_with_adc(config)?;
/// let index = load_skill_index_from_registry(
///     &client,
///     RegistrySkillFilter::by_query("data analysis").with_top_k(5),
/// )
/// .await?;
/// println!("loaded {} registry skill(s)", index.len());
/// # Ok(())
/// # }
/// ```
///
/// # Errors
///
/// Returns [`SkillError::Registry`] when a registry request fails,
/// [`SkillError::Validation`] when a package has no `SKILL.md` at its root or
/// the file is not UTF-8, and the standard parse errors when the `SKILL.md`
/// frontmatter is invalid.
pub async fn load_skill_index_from_registry(
    client: &SkillRegistryClient,
    filter: RegistrySkillFilter,
) -> SkillResult<SkillIndex> {
    let names: Vec<String> = match &filter.selector {
        RegistrySkillSelector::Query { query, top_k } => client
            .search_skills(query, *top_k)
            .await?
            .into_iter()
            .map(|retrieved| retrieved.skill_name)
            .collect(),
        RegistrySkillSelector::Names(names) => names.clone(),
    };

    let mut skills = Vec::with_capacity(names.len());
    for name in names {
        let content = match &filter.revision {
            Some(revision) => client.fetch_skill_revision_content(&name, revision).await?,
            None => client.fetch_skill_content(&name).await?,
        };
        skills.push(document_from_content(&content)?);
    }

    skills.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
    tracing::debug!(skill.count = skills.len(), "loaded skill index from registry");
    Ok(SkillIndex::new(skills))
}

/// Merges a local and a registry-backed index; local wins on name collision.
///
/// Mirrors the precedence rule of
/// [`load_skill_index_with_extras`](crate::load_skill_index_with_extras),
/// where project-local skills shadow global ones: every local skill is kept,
/// and remote skills are added only when no local skill has the same name.
/// The result is sorted by skill name and path.
///
/// # Example
///
/// ```no_run
/// use adk_skill::load_skill_index;
/// use adk_skill::registry::merge_skill_indexes;
///
/// # async fn merge(remote: adk_skill::SkillIndex) -> adk_skill::SkillResult<()> {
/// let local = load_skill_index(".")?;
/// let merged = merge_skill_indexes(local, remote);
/// # let _ = merged;
/// # Ok(())
/// # }
/// ```
pub fn merge_skill_indexes(local: SkillIndex, remote: SkillIndex) -> SkillIndex {
    let mut skills: Vec<SkillDocument> = local.skills().to_vec();
    for skill in remote.skills() {
        if local.find_by_name(&skill.name).is_none() {
            skills.push(skill.clone());
        }
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
    SkillIndex::new(skills)
}

/// Builds a [`SkillDocument`] from a fetched skill package.
/// Builds a document from a verified registry payload.
///
/// This is shared by eager index loading and progressive, invocation-scoped
/// loading so both paths preserve the same parser and content-hash semantics.
pub(crate) fn document_from_content(content: &SkillContent) -> SkillResult<SkillDocument> {
    let skill_md = content.skill_md().ok_or_else(|| {
        SkillError::Validation(format!(
            "skill `{}` package has no SKILL.md at its root; repackage the skill with SKILL.md at the top level",
            content.skill.name,
        ))
    })?;
    let text = std::str::from_utf8(skill_md).map_err(|_| {
        SkillError::Validation(format!(
            "SKILL.md in skill `{}` is not valid UTF-8",
            content.skill.name,
        ))
    })?;
    let path = PathBuf::from(format!("{}/{}", content.skill.name, SkillContent::SKILL_MD));
    let parsed = parse_skill_markdown(&path, text)?;
    Ok(build_document(parsed, path, text, None))
}
