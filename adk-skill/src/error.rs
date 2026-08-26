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

    // ===== Skill Registry payloads (feature `vertex-skill-registry`) =====
    /// The `zippedFilesystem` field was not valid standard base64.
    #[cfg(feature = "vertex-skill-registry")]
    #[error(
        "skill registry payload is not valid base64: {message}. The zippedFilesystem field must be standard base64; re-fetch the skill or report the registry entry"
    )]
    RegistryPayloadDecode { message: String },

    /// The decoded zip did not match the registry's `sha256` field.
    #[cfg(feature = "vertex-skill-registry")]
    #[error(
        "skill registry payload sha256 mismatch: expected {expected}, computed {actual}. The payload may be corrupt or tampered with; re-fetch the skill before using its contents"
    )]
    RegistryChecksumMismatch { expected: String, actual: String },

    /// The payload is not a well-formed zip archive.
    #[cfg(feature = "vertex-skill-registry")]
    #[error(
        "skill archive is not a valid zip: {message}. The registry serves SKILL.md packages as zip archives; re-fetch the skill or report the registry entry"
    )]
    ArchiveFormat { message: String },

    /// The archive exceeds the maximum accepted size.
    #[cfg(feature = "vertex-skill-registry")]
    #[error(
        "skill archive is {size} bytes, exceeding the {limit}-byte limit. The registry rejects archives over this size; do not extract oversized payloads"
    )]
    ArchiveTooLarge { size: u64, limit: u64 },

    /// The archive contains more entries than allowed.
    #[cfg(feature = "vertex-skill-registry")]
    #[error(
        "skill archive contains {count} entries, exceeding the limit of {limit}. Split the skill into smaller packages or remove unneeded files"
    )]
    ArchiveTooManyEntries { count: usize, limit: usize },

    /// An entry path contains a `..` component.
    #[cfg(feature = "vertex-skill-registry")]
    #[error(
        "skill archive entry `{name}` contains a `..` path component. Path traversal is rejected; all entries must resolve inside the extraction root"
    )]
    ArchivePathTraversal { name: String },

    /// An entry path is absolute.
    #[cfg(feature = "vertex-skill-registry")]
    #[error(
        "skill archive entry `{name}` is an absolute path. Entries must be relative so extraction stays inside the target directory"
    )]
    ArchiveAbsolutePath { name: String },

    /// An entry is a symbolic link.
    #[cfg(feature = "vertex-skill-registry")]
    #[error(
        "skill archive entry `{name}` is a symlink. Symlinks are rejected because they can escape the extraction root"
    )]
    ArchiveSymlink { name: String },

    /// Two entries share the same normalized name.
    #[cfg(feature = "vertex-skill-registry")]
    #[error(
        "skill archive contains duplicate entry `{name}`. Duplicate names are rejected because later entries would silently overwrite earlier ones"
    )]
    ArchiveDuplicateEntry { name: String },

    /// The declared uncompressed total exceeds the limit.
    #[cfg(feature = "vertex-skill-registry")]
    #[error(
        "skill archive declares {total} uncompressed bytes, exceeding the {limit}-byte limit. Refusing to extract to avoid resource exhaustion"
    )]
    ArchiveUncompressedTooLarge { total: u64, limit: u64 },

    /// An entry's compression ratio exceeds the limit (zip-bomb defense).
    #[cfg(feature = "vertex-skill-registry")]
    #[error(
        "skill archive entry `{name}` has compression ratio {ratio}, exceeding the limit of {limit}. Highly compressed entries are rejected as potential zip bombs"
    )]
    ArchiveCompressionRatio { name: String, ratio: u64, limit: u64 },

    /// An entry nests deeper than the allowed directory depth.
    #[cfg(feature = "vertex-skill-registry")]
    #[error(
        "skill archive entry `{name}` nests {depth} directory levels, exceeding the limit of {limit}. Flatten the skill's directory layout"
    )]
    ArchiveDepthExceeded { name: String, depth: usize, limit: usize },
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
            // Decode and checksum failures are corrupt upstream payloads;
            // archive-rule violations are rejected input data.
            #[cfg(feature = "vertex-skill-registry")]
            SkillError::RegistryPayloadDecode { .. } => {
                (ErrorCategory::Internal, "skill.registry.payload_decode")
            }
            #[cfg(feature = "vertex-skill-registry")]
            SkillError::RegistryChecksumMismatch { .. } => {
                (ErrorCategory::Internal, "skill.registry.checksum_mismatch")
            }
            #[cfg(feature = "vertex-skill-registry")]
            SkillError::ArchiveFormat { .. } => {
                (ErrorCategory::InvalidInput, "skill.archive.invalid_format")
            }
            #[cfg(feature = "vertex-skill-registry")]
            SkillError::ArchiveTooLarge { .. } => {
                (ErrorCategory::InvalidInput, "skill.archive.too_large")
            }
            #[cfg(feature = "vertex-skill-registry")]
            SkillError::ArchiveTooManyEntries { .. } => {
                (ErrorCategory::InvalidInput, "skill.archive.too_many_entries")
            }
            #[cfg(feature = "vertex-skill-registry")]
            SkillError::ArchivePathTraversal { .. } => {
                (ErrorCategory::InvalidInput, "skill.archive.path_traversal")
            }
            #[cfg(feature = "vertex-skill-registry")]
            SkillError::ArchiveAbsolutePath { .. } => {
                (ErrorCategory::InvalidInput, "skill.archive.absolute_path")
            }
            #[cfg(feature = "vertex-skill-registry")]
            SkillError::ArchiveSymlink { .. } => {
                (ErrorCategory::InvalidInput, "skill.archive.symlink")
            }
            #[cfg(feature = "vertex-skill-registry")]
            SkillError::ArchiveDuplicateEntry { .. } => {
                (ErrorCategory::InvalidInput, "skill.archive.duplicate_entry")
            }
            #[cfg(feature = "vertex-skill-registry")]
            SkillError::ArchiveUncompressedTooLarge { .. } => {
                (ErrorCategory::InvalidInput, "skill.archive.uncompressed_too_large")
            }
            #[cfg(feature = "vertex-skill-registry")]
            SkillError::ArchiveCompressionRatio { .. } => {
                (ErrorCategory::InvalidInput, "skill.archive.compression_ratio")
            }
            #[cfg(feature = "vertex-skill-registry")]
            SkillError::ArchiveDepthExceeded { .. } => {
                (ErrorCategory::InvalidInput, "skill.archive.depth_exceeded")
            }
        };
        adk_core::AdkError::new(ErrorComponent::Tool, category, code, err.to_string())
            .with_source(err)
    }
}
