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
