use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("yaml parse error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    #[error("invalid skill frontmatter in {path}: {message}")]
    InvalidFrontmatter { path: PathBuf, message: String },

    #[error("missing required field `{field}` in {path}")]
    MissingField { path: PathBuf, field: &'static str },

    #[error("invalid skills root, expected directory: {0}")]
    InvalidSkillsRoot(PathBuf),

    #[error("skill validation error: {0}")]
    Validation(String),

    #[error("index error: {0}")]
    IndexError(String),
}

pub type SkillResult<T> = Result<T, SkillError>;

impl From<SkillError> for adk_core::AdkError {
    fn from(err: SkillError) -> Self {
        use adk_core::{ErrorCategory, ErrorComponent};
        let (category, code) = match &err {
            SkillError::Io(_) => (ErrorCategory::Internal, "skill.io"),
            SkillError::Yaml(_) => (ErrorCategory::InvalidInput, "skill.yaml_parse"),
            SkillError::InvalidFrontmatter { .. } => {
                (ErrorCategory::InvalidInput, "skill.invalid_frontmatter")
            }
            SkillError::MissingField { .. } => (ErrorCategory::InvalidInput, "skill.missing_field"),
            SkillError::InvalidSkillsRoot(_) => {
                (ErrorCategory::NotFound, "skill.invalid_skills_root")
            }
            SkillError::Validation(_) => (ErrorCategory::InvalidInput, "skill.validation"),
            SkillError::IndexError(_) => (ErrorCategory::Internal, "skill.index"),
        };
        adk_core::AdkError::new(ErrorComponent::Tool, category, code, err.to_string())
            .with_source(err)
    }
}
