use crate::error::SkillResult;
use crate::index::{load_skill_index, load_skill_index_with_extras};
use crate::model::{SelectionPolicy, SkillIndex, SkillMatch};
use crate::select::select_skills;
use adk_core::{Content, Part};
use adk_plugin::{Plugin, PluginConfig, PluginManager};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct SkillInjectorConfig {
    pub policy: SelectionPolicy,
    pub max_injected_chars: usize,
    /// Optional global skills directory (e.g. `~/.config/adk/skills/`).
    /// Skills here are included in the index but project-local skills
    /// take precedence when names collide.
    pub global_skills_dir: Option<PathBuf>,
    /// Additional directories to scan for skills.
    pub extra_paths: Vec<PathBuf>,
}

impl Default for SkillInjectorConfig {
    fn default() -> Self {
        Self {
            policy: SelectionPolicy::default(),
            max_injected_chars: 2000,
            global_skills_dir: None,
            extra_paths: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SkillInjector {
    index: Arc<SkillIndex>,
    config: SkillInjectorConfig,
}

impl SkillInjector {
    pub fn from_root(root: impl AsRef<Path>, config: SkillInjectorConfig) -> SkillResult<Self> {
        let mut extra_dirs: Vec<PathBuf> = config.extra_paths.clone();
        if let Some(ref global) = config.global_skills_dir {
            extra_dirs.push(global.clone());
        }
        let index = if extra_dirs.is_empty() {
            load_skill_index(root)?
        } else {
            load_skill_index_with_extras(root, &extra_dirs)?
        };
        Ok(Self { index: Arc::new(index), config })
    }

    pub fn from_index(index: SkillIndex, config: SkillInjectorConfig) -> Self {
        Self { index: Arc::new(index), config }
    }

    pub fn index(&self) -> &SkillIndex {
        self.index.as_ref()
    }

    pub fn policy(&self) -> &SelectionPolicy {
        &self.config.policy
    }

    pub fn max_injected_chars(&self) -> usize {
        self.config.max_injected_chars
    }

    pub fn build_plugin(&self, name: impl Into<String>) -> Plugin {
        let plugin_name = name.into();
        let index = self.index.clone();
        let policy = self.config.policy.clone();
        let max_injected_chars = self.config.max_injected_chars;

        Plugin::new(PluginConfig {
            name: plugin_name,
            on_user_message: Some(Box::new(move |_ctx, mut content| {
                let index = index.clone();
                let policy = policy.clone();
                Box::pin(async move {
                    let injected = apply_skill_injection(
                        &mut content,
                        index.as_ref(),
                        &policy,
                        max_injected_chars,
                    );
                    Ok(if injected.is_some() { Some(content) } else { None })
                })
            })),
            ..Default::default()
        })
    }

    pub fn build_plugin_manager(&self, name: impl Into<String>) -> PluginManager {
        PluginManager::new(vec![self.build_plugin(name)])
    }
}

/// Selects the top-scoring skill for a query and returns its formatted prompt block.
///
/// Returns `None` if no skill meets the selection criteria. The prompt block is
/// truncated to `max_injected_chars` and wrapped in a `[skill:<name>]` section
/// suitable for prepending to a user message.
pub fn select_skill_prompt_block(
    index: &SkillIndex,
    query: &str,
    policy: &SelectionPolicy,
    max_injected_chars: usize,
) -> Option<(SkillMatch, String)> {
    let top = select_skills(index, query, policy).into_iter().next()?;
    let matched = index.find_by_id(&top.skill.id)?;
    let prompt_block = matched.engineer_prompt_block(max_injected_chars);
    Some((top, prompt_block))
}

/// Injects the best-matching skill prompt into a user [`Content`] message.
///
/// Extracts the text from `content`, selects the top skill from `index`, and
/// prepends its prompt block to the first text part. Returns the matched skill
/// on success, or `None` if the content is not a user message, the index is
/// empty, or no skill meets the selection threshold.
pub fn apply_skill_injection(
    content: &mut Content,
    index: &SkillIndex,
    policy: &SelectionPolicy,
    max_injected_chars: usize,
) -> Option<SkillMatch> {
    if content.role != "user" || index.is_empty() {
        return None;
    }

    let original_text = extract_text(content);
    if original_text.trim().is_empty() {
        return None;
    }

    let (top, prompt_block) =
        select_skill_prompt_block(index, &original_text, policy, max_injected_chars)?;
    let injected_text = format!("{prompt_block}\n\n{original_text}");

    if let Some(Part::Text { text }) =
        content.parts.iter_mut().find(|part| matches!(part, Part::Text { .. }))
    {
        *text = injected_text;
    } else {
        content.parts.insert(0, Part::Text { text: injected_text });
    }

    Some(top)
}

fn extract_text(content: &Content) -> String {
    content
        .parts
        .iter()
        .filter_map(|p| match p {
            Part::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::load_skill_index;
    use std::fs;

    #[test]
    fn injects_top_skill_into_user_message() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join(".skills")).unwrap();

        fs::write(
            root.join(".skills/search.md"),
            "---\nname: search\ndescription: Search code\n---\nUse rg first.",
        )
        .unwrap();

        let index = load_skill_index(root).unwrap();
        let policy = SelectionPolicy { top_k: 1, min_score: 0.1, ..SelectionPolicy::default() };

        let mut content = Content::new("user").with_text("Please search this repository quickly");
        let matched = apply_skill_injection(&mut content, &index, &policy, 1000);

        assert!(matched.is_some());
        let injected = content.parts[0].text().unwrap();
        assert!(injected.contains("[skill:search]"));
        assert!(injected.contains("Use rg first."));
    }
}
