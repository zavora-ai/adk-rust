use crate::error::{SkillError, SkillResult};
use crate::model::{ParsedSkill, SkillFrontmatter};
use std::path::Path;

const CONVENTION_FILES: &[&str] =
    &["AGENTS.md", "AGENT.md", "CLAUDE.md", "GEMINI.md", "COPILOT.md", "SKILLS.md", "SOUL.md"];

pub fn parse_skill_markdown(path: &Path, content: &str) -> SkillResult<ParsedSkill> {
    let normalized = content.replace("\r\n", "\n");
    let mut lines = normalized.lines();

    let first = lines.next().unwrap_or_default().trim();
    if first != "---" {
        return Err(SkillError::InvalidFrontmatter {
            path: path.to_path_buf(),
            message: "missing opening frontmatter delimiter (`---`)".to_string(),
        });
    }

    let mut frontmatter_lines = Vec::new();
    let mut found_end = false;
    for line in lines.by_ref() {
        if line.trim() == "---" {
            found_end = true;
            break;
        }
        frontmatter_lines.push(line);
    }

    if !found_end {
        return Err(SkillError::InvalidFrontmatter {
            path: path.to_path_buf(),
            message: "missing closing frontmatter delimiter (`---`)".to_string(),
        });
    }

    let frontmatter_raw = frontmatter_lines.join("\n");
    let fm: SkillFrontmatter = serde_yaml::from_str(&frontmatter_raw)?;

    let name = fm.name.trim().to_string();
    if name.is_empty() {
        return Err(SkillError::MissingField { path: path.to_path_buf(), field: "name" });
    }

    let description = fm.description.trim().to_string();
    if description.is_empty() {
        return Err(SkillError::MissingField { path: path.to_path_buf(), field: "description" });
    }

    let tags = fm
        .tags
        .into_iter()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>();

    let allowed_tools = fm
        .allowed_tools
        .into_iter()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>();

    let references = fm
        .references
        .into_iter()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>();

    let body = lines.collect::<Vec<_>>().join("\n").trim().to_string();

    Ok(ParsedSkill {
        name,
        description,
        version: fm.version,
        license: fm.license,
        compatibility: fm.compatibility,
        tags,
        allowed_tools,
        references,
        trigger: fm.trigger.unwrap_or(false),
        hint: fm.hint,
        metadata: fm.metadata,
        body,
    })
}

pub fn parse_instruction_markdown(path: &Path, content: &str) -> SkillResult<ParsedSkill> {
    if is_skill_file_path(path) {
        return parse_skill_markdown(path, content);
    }

    if is_convention_file(path) {
        return parse_convention_markdown(path, content);
    }

    parse_skill_markdown(path, content)
}

fn parse_convention_markdown(path: &Path, content: &str) -> SkillResult<ParsedSkill> {
    // Convention files often use plain markdown without frontmatter. If frontmatter is present
    // and valid, we still honor it for compatibility.
    if content.lines().next().is_some_and(|line| line.trim() == "---") {
        if let Ok(parsed) = parse_skill_markdown(path, content) {
            return Ok(parsed);
        }
    }

    let normalized = content.replace("\r\n", "\n");
    let body = normalized.trim().to_string();

    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("instruction.md");

    let (name, tags) = match file_name.to_ascii_uppercase().as_str() {
        "AGENTS.MD" | "AGENT.MD" => {
            ("agents".to_string(), vec!["convention".to_string(), "agents-md".to_string()])
        }
        "CLAUDE.MD" => {
            ("claude".to_string(), vec!["convention".to_string(), "claude-md".to_string()])
        }
        "GEMINI.MD" => {
            ("gemini".to_string(), vec!["convention".to_string(), "gemini-md".to_string()])
        }
        "COPILOT.MD" => {
            ("copilot".to_string(), vec!["convention".to_string(), "copilot-md".to_string()])
        }
        "SKILLS.MD" => {
            ("skills".to_string(), vec!["convention".to_string(), "skills-md".to_string()])
        }
        "SOUL.MD" => ("soul".to_string(), vec!["convention".to_string(), "soul-md".to_string()]),
        _ => (
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("instruction")
                .to_ascii_lowercase(),
            vec!["convention".to_string()],
        ),
    };

    let description = extract_convention_description(&body).unwrap_or_else(|| {
        format!(
            "Instructions loaded from {}",
            path.file_name().and_then(|n| n.to_str()).unwrap_or("instruction file")
        )
    });

    Ok(ParsedSkill {
        name,
        description,
        version: None,
        license: None,
        compatibility: None,
        tags,
        allowed_tools: Vec::new(),
        references: Vec::new(),
        trigger: false,
        hint: None,
        metadata: std::collections::HashMap::new(),
        body,
    })
}

fn is_skill_file_path(path: &Path) -> bool {
    path.components()
        .any(|component| component.as_os_str().to_string_lossy().eq_ignore_ascii_case(".skills"))
}

fn is_convention_file(path: &Path) -> bool {
    path.file_name().and_then(|n| n.to_str()).is_some_and(|name| {
        CONVENTION_FILES.iter().any(|candidate| name.eq_ignore_ascii_case(candidate))
    })
}

fn extract_convention_description(body: &str) -> Option<String> {
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Prefer first markdown heading if present.
        if trimmed.starts_with('#') {
            let heading = trimmed.trim_start_matches('#').trim();
            if !heading.is_empty() {
                return Some(heading.to_string());
            }
        }

        return Some(trimmed.chars().take(120).collect::<String>());
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_skill() {
        let content = r#"---
name: repo_search
description: Search the codebase quickly
tags:
  - code
  - search
---
Use ripgrep first.
"#;
        let parsed = parse_skill_markdown(Path::new(".skills/repo_search.md"), content).unwrap();
        assert_eq!(parsed.name, "repo_search");
        assert_eq!(parsed.description, "Search the codebase quickly");
        assert_eq!(parsed.tags, vec!["code", "search"]);
        assert!(parsed.body.contains("Use ripgrep first."));
    }

    #[test]
    fn parses_skill_with_full_spec() {
        let content = r#"---
name: full_spec_agent
description: An agent with everything
version: "1.2.3"
license: MIT
compatibility: "Requires Python 3.10+"
allowed-tools:
  - tool1
references:
  - ref1
trigger: true
hint: "Say something"
metadata:
  custom_key: custom_value
tags: [tag1]
---
Body content.
"#;
        let parsed = parse_skill_markdown(Path::new(".skills/full.md"), content).unwrap();
        assert_eq!(parsed.name, "full_spec_agent");
        assert_eq!(parsed.version, Some("1.2.3".to_string()));
        assert_eq!(parsed.license, Some("MIT".to_string()));
        assert_eq!(parsed.compatibility, Some("Requires Python 3.10+".to_string()));
        assert_eq!(parsed.allowed_tools, vec!["tool1"]);
        assert_eq!(parsed.references, vec!["ref1"]);
        assert!(parsed.trigger);
        assert_eq!(parsed.hint, Some("Say something".to_string()));
        assert_eq!(
            parsed.metadata.get("custom_key").and_then(|v| v.as_str()),
            Some("custom_value")
        );
    }

    #[test]
    fn rejects_missing_required_fields() {
        let content = r#"---
name: ""
description: ""
---
body
"#;
        let err = parse_skill_markdown(Path::new(".skills/bad.md"), content).unwrap_err();
        assert!(matches!(err, SkillError::MissingField { .. }));
    }

    #[test]
    fn parses_agents_md_without_frontmatter() {
        let content = "# Project Agent Instructions\nAlways prefer rg over grep.\n";
        let parsed = parse_instruction_markdown(Path::new("AGENTS.md"), content).unwrap();

        assert_eq!(parsed.name, "agents");
        assert_eq!(parsed.description, "Project Agent Instructions");
        assert!(parsed.tags.contains(&"convention".to_string()));
        assert!(parsed.tags.contains(&"agents-md".to_string()));
        assert!(parsed.body.contains("Always prefer rg"));
    }

    #[test]
    fn parses_soul_md_without_frontmatter() {
        let content = "# Soul Profile\nPrioritize deliberate planning before execution.\n";
        let parsed = parse_instruction_markdown(Path::new("SOUL.MD"), content).unwrap();

        assert_eq!(parsed.name, "soul");
        assert_eq!(parsed.description, "Soul Profile");
        assert!(parsed.tags.contains(&"convention".to_string()));
        assert!(parsed.tags.contains(&"soul-md".to_string()));
        assert!(parsed.body.contains("deliberate planning"));
    }

    #[test]
    fn keeps_strict_frontmatter_for_skills_directory() {
        let content = "# Missing frontmatter";
        let err = parse_instruction_markdown(Path::new(".skills/missing.md"), content).unwrap_err();
        assert!(matches!(err, SkillError::InvalidFrontmatter { .. }));
    }
}
