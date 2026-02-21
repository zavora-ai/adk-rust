use crate::model::{SelectionPolicy, SkillIndex, SkillMatch, SkillSummary};
use std::collections::HashSet;

pub fn select_skills(index: &SkillIndex, query: &str, policy: &SelectionPolicy) -> Vec<SkillMatch> {
    if policy.top_k == 0 {
        return Vec::new();
    }

    let include_tags =
        policy.include_tags.iter().map(|t| t.to_ascii_lowercase()).collect::<HashSet<_>>();
    let exclude_tags =
        policy.exclude_tags.iter().map(|t| t.to_ascii_lowercase()).collect::<HashSet<_>>();

    let query_tokens = tokenize(query);
    if query_tokens.is_empty() && include_tags.is_empty() {
        return Vec::new();
    }

    let mut scored = index
        .skills()
        .iter()
        .filter(|skill| tag_allowed(skill, &include_tags, &exclude_tags))
        .map(|skill| {
            let score = score_skill(&query_tokens, skill);
            SkillMatch { score, skill: SkillSummary::from(skill) }
        })
        .filter(|m| m.score >= policy.min_score)
        .collect::<Vec<_>>();

    scored.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.skill.name.cmp(&b.skill.name))
            .then_with(|| a.skill.path.cmp(&b.skill.path))
    });

    scored.into_iter().take(policy.top_k).collect()
}

fn tag_allowed(
    skill: &crate::model::SkillDocument,
    include: &HashSet<String>,
    exclude: &HashSet<String>,
) -> bool {
    let skill_tags = skill.tags.iter().map(|t| t.to_ascii_lowercase()).collect::<HashSet<_>>();

    if !exclude.is_empty() && !skill_tags.is_disjoint(exclude) {
        return false;
    }

    include.is_empty() || !skill_tags.is_disjoint(include)
}

fn score_skill(query_tokens: &[String], skill: &crate::model::SkillDocument) -> f32 {
    let name_tokens = to_set(&skill.name);
    let description_tokens = to_set(&skill.description);
    let body_tokens = to_set(&skill.body);
    let tags_tokens = skill.tags.iter().flat_map(|t| tokenize(t)).collect::<HashSet<_>>();

    let mut score = 0.0;
    for token in query_tokens {
        if name_tokens.contains(token) {
            score += 4.0;
        }
        if description_tokens.contains(token) {
            score += 2.5;
        }
        if tags_tokens.contains(token) {
            score += 2.0;
        }
        if body_tokens.contains(token) {
            score += 1.0;
        }
    }

    // Small normalization to avoid bias toward huge docs while keeping scoring simple.
    let norm = (body_tokens.len().max(1) as f32).sqrt();
    score / norm.max(1.0)
}

fn tokenize(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();

    for c in input.chars() {
        if c.is_ascii_alphanumeric() {
            current.push(c.to_ascii_lowercase());
        } else if !current.is_empty() {
            tokens.push(current.clone());
            current.clear();
        }
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    tokens
}

fn to_set(input: &str) -> HashSet<String> {
    tokenize(input).into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::load_skill_index;
    use std::fs;

    #[test]
    fn selects_most_relevant_skill() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join(".skills")).unwrap();

        fs::write(
            root.join(".skills/code_search.md"),
            "---\nname: code_search\ndescription: Search Rust code with rg\ntags: [code, search]\n---\nUse rg --files then rg.",
        )
        .unwrap();
        fs::write(
            root.join(".skills/release_notes.md"),
            "---\nname: release_notes\ndescription: Prepare release notes\ntags: [changelog]\n---\nSummarize commits.",
        )
        .unwrap();

        let index = load_skill_index(root).unwrap();
        let policy = SelectionPolicy { top_k: 1, min_score: 0.1, ..SelectionPolicy::default() };
        let matches = select_skills(&index, "search rust codebase", &policy);

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].skill.name, "code_search");
    }

    #[test]
    fn returns_empty_when_unrelated() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join(".skills")).unwrap();

        fs::write(
            root.join(".skills/release.md"),
            "---\nname: release\ndescription: Release process\n---\nBump versions and publish.",
        )
        .unwrap();

        let index = load_skill_index(root).unwrap();
        let policy = SelectionPolicy { top_k: 1, min_score: 2.0, ..SelectionPolicy::default() };
        let matches = select_skills(&index, "quantum entanglement", &policy);
        assert!(matches.is_empty());
    }

    #[test]
    fn include_and_exclude_tags_filter_results() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join(".skills")).unwrap();

        fs::write(
            root.join(".skills/code.md"),
            "---\nname: code\ndescription: code search\ntags: [code, search]\n---\nUse rg.\n",
        )
        .unwrap();
        fs::write(
            root.join(".skills/release.md"),
            "---\nname: release\ndescription: release notes\ntags: [docs]\n---\nSummarize commits.\n",
        )
        .unwrap();

        let index = load_skill_index(root).unwrap();
        let policy = SelectionPolicy {
            top_k: 5,
            min_score: 0.1,
            include_tags: vec!["code".to_string()],
            exclude_tags: vec!["docs".to_string()],
        };

        let matches = select_skills(&index, "search", &policy);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].skill.name, "code");
    }
}
