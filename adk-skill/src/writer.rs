//! Writing skills to disk.
//!
//! Everything else in this crate reads: [`discover_skill_files`](crate::discover_skill_files)
//! walks a root, [`parse_skill_markdown`](crate::parse_skill_markdown) turns a file into a
//! [`ParsedSkill`](crate::ParsedSkill), and [`SkillIndex`](crate::SkillIndex) holds the result.
//! There was no write path, so an agent could not persist a skill it derived at runtime and an
//! operator could not generate one programmatically.
//!
//! [`SkillWriter`] closes that, writing into the `.skills` directory
//! [`load_skill_index`](crate::load_skill_index) already discovers.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::error::{SkillError, SkillResult};

/// Longest permitted skill name, from the `agentskills.io` field definition.
const MAX_NAME_LEN: usize = 64;

/// Directory, relative to a writer's root, that skills are written into.
const SKILLS_DIR: &str = ".skills";

/// Checks that `name` is a valid skill identifier.
///
/// The specification allows 1–64 characters of lowercase letters, digits, and hyphens. A name may
/// not begin or end with a hyphen.
///
/// This is also the path-safety boundary: because a name becomes a filename, rejecting everything
/// outside `[a-z0-9-]` is what prevents a caller from escaping the skills directory with `..` or a
/// path separator.
///
/// # Errors
///
/// Returns [`SkillError::Validation`] naming the specific rule that failed.
///
/// # Example
///
/// ```rust
/// use adk_skill::validate_skill_name;
///
/// assert!(validate_skill_name("disk-triage").is_ok());
/// assert!(validate_skill_name("../escape").is_err());
/// ```
pub fn validate_skill_name(name: &str) -> SkillResult<()> {
    if name.is_empty() {
        return Err(SkillError::Validation("skill name must not be empty".to_string()));
    }

    if name.chars().count() > MAX_NAME_LEN {
        return Err(SkillError::Validation(format!(
            "skill name must be at most {MAX_NAME_LEN} characters, got {}",
            name.chars().count()
        )));
    }

    if let Some(bad) = name.chars().find(|c| !matches!(c, 'a'..='z' | '0'..='9' | '-')) {
        return Err(SkillError::Validation(format!(
            "skill name must contain only lowercase letters, digits, and hyphens; found {bad:?} \
             in {name:?}"
        )));
    }

    if name.starts_with('-') || name.ends_with('-') {
        return Err(SkillError::Validation(format!(
            "skill name must not begin or end with a hyphen: {name:?}"
        )));
    }

    Ok(())
}

/// Frontmatter as written, omitting anything unset so a generated file stays readable.
///
/// Separate from [`SkillFrontmatter`](crate::SkillFrontmatter), which is a parse target and
/// serializes every field including empty ones.
#[derive(Debug, Serialize)]
struct FrontmatterOut<'a> {
    name: &'a str,
    description: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    license: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    compatibility: &'a Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tags: &'a Vec<String>,
    #[serde(rename = "allowed-tools", skip_serializing_if = "Vec::is_empty")]
    allowed_tools: &'a Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    references: &'a Vec<String>,
    #[serde(skip_serializing_if = "is_false")]
    trigger: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    hint: &'a Option<String>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    metadata: &'a HashMap<String, Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    triggers: &'a Vec<String>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// A skill to be written to disk.
///
/// `name` and `description` are required by the specification; everything else is optional and is
/// omitted from the generated file when unset.
///
/// # Example
///
/// ```rust
/// use adk_skill::SkillDraft;
///
/// let draft = SkillDraft::new("disk-triage", "Diagnose low disk space. Use when a disk alert fires.")
///     .with_body("1. Check the largest directories.\n2. Report growth rate.")
///     .with_tags(["ops"])
///     .with_allowed_tools(["read_file"]);
///
/// assert_eq!(draft.name(), "disk-triage");
/// ```
#[derive(Debug, Clone, Default)]
pub struct SkillDraft {
    name: String,
    description: String,
    body: String,
    version: Option<String>,
    license: Option<String>,
    compatibility: Option<String>,
    tags: Vec<String>,
    allowed_tools: Vec<String>,
    references: Vec<String>,
    trigger: bool,
    hint: Option<String>,
    metadata: HashMap<String, Value>,
    triggers: Vec<String>,
}

impl SkillDraft {
    /// Creates a draft with the two required fields.
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self { name: name.into(), description: description.into(), ..Default::default() }
    }

    /// Sets the instructional Markdown body.
    pub fn with_body(mut self, body: impl Into<String>) -> Self {
        self.body = body.into();
        self
    }

    /// Sets the version identifier.
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    /// Sets the license identifier.
    pub fn with_license(mut self, license: impl Into<String>) -> Self {
        self.license = Some(license.into());
        self
    }

    /// Sets the environment requirements.
    pub fn with_compatibility(mut self, compatibility: impl Into<String>) -> Self {
        self.compatibility = Some(compatibility.into());
        self
    }

    /// Sets the discovery tags.
    pub fn with_tags<I, S>(mut self, tags: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.tags = tags.into_iter().map(Into::into).collect();
        self
    }

    /// Sets the pre-approved tool names.
    pub fn with_allowed_tools<I, S>(mut self, tools: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.allowed_tools = tools.into_iter().map(Into::into).collect();
        self
    }

    /// Sets the supporting resource paths.
    pub fn with_references<I, S>(mut self, references: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.references = references.into_iter().map(Into::into).collect();
        self
    }

    /// Requires explicit invocation by name rather than automatic selection.
    pub fn with_trigger(mut self, trigger: bool) -> Self {
        self.trigger = trigger;
        self
    }

    /// Sets the guided-input hint.
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Sets the extension metadata.
    ///
    /// Useful for recording provenance — which incident a learned skill came from, when it was
    /// promoted, and what evidence supported it.
    pub fn with_metadata(mut self, metadata: HashMap<String, Value>) -> Self {
        self.metadata = metadata;
        self
    }

    /// Adds one extension metadata entry.
    pub fn with_metadata_entry(mut self, key: impl Into<String>, value: Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }

    /// Sets the file glob patterns that activate this skill.
    pub fn with_triggers<I, S>(mut self, triggers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.triggers = triggers.into_iter().map(Into::into).collect();
        self
    }

    /// The skill's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Checks the draft against the specification.
    ///
    /// # Errors
    ///
    /// Returns [`SkillError::Validation`] when the name is invalid or the description is empty.
    pub fn validate(&self) -> SkillResult<()> {
        validate_skill_name(&self.name)?;

        if self.description.trim().is_empty() {
            return Err(SkillError::Validation(format!(
                "skill {:?} must have a non-empty description; it is what an agent matches on",
                self.name
            )));
        }

        Ok(())
    }

    /// Renders the draft as skill Markdown: YAML frontmatter, then the body.
    ///
    /// The output is what [`parse_skill_markdown`](crate::parse_skill_markdown) accepts, so a
    /// draft written and reparsed yields equivalent content.
    ///
    /// # Errors
    ///
    /// Returns an error if the draft fails [`validate`](Self::validate) or the frontmatter cannot
    /// be serialized.
    pub fn to_markdown(&self) -> SkillResult<String> {
        self.validate()?;

        let frontmatter = serde_yaml::to_string(&FrontmatterOut {
            name: &self.name,
            description: &self.description,
            version: &self.version,
            license: &self.license,
            compatibility: &self.compatibility,
            tags: &self.tags,
            allowed_tools: &self.allowed_tools,
            references: &self.references,
            trigger: self.trigger,
            hint: &self.hint,
            metadata: &self.metadata,
            triggers: &self.triggers,
        })?;

        Ok(format!("---\n{}---\n\n{}\n", frontmatter, self.body.trim()))
    }
}

/// Writes skills into a root's `.skills` directory.
///
/// The destination is the directory [`load_skill_index`](crate::load_skill_index) already
/// discovers, so a skill written here is picked up by the next index load.
///
/// # Example
///
/// ```rust
/// use adk_skill::{SkillDraft, SkillWriter, load_skill_index};
///
/// # fn main() -> adk_skill::SkillResult<()> {
/// let root = tempfile::tempdir().unwrap();
/// let writer = SkillWriter::new(root.path());
///
/// writer.write(&SkillDraft::new("disk-triage", "Diagnose low disk space.")
///     .with_body("Check the largest directories first."))?;
///
/// let index = load_skill_index(root.path())?;
/// assert!(index.find_by_name("disk-triage").is_some());
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct SkillWriter {
    root: PathBuf,
}

impl SkillWriter {
    /// Creates a writer targeting `<root>/.skills`.
    ///
    /// Pass the same root given to [`load_skill_index`](crate::load_skill_index).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The directory skills are written into.
    pub fn skills_dir(&self) -> PathBuf {
        self.root.join(SKILLS_DIR)
    }

    /// The path a skill of this name occupies.
    ///
    /// # Errors
    ///
    /// Returns [`SkillError::Validation`] if the name is not a valid identifier.
    pub fn path_for(&self, name: &str) -> SkillResult<PathBuf> {
        validate_skill_name(name)?;
        Ok(self.skills_dir().join(format!("{name}.md")))
    }

    /// Writes `draft`, replacing any existing skill of the same name, and returns its path.
    ///
    /// The write goes to a temporary file in the same directory and is then renamed, so a crash
    /// mid-write cannot leave a half-written skill that fails to parse and breaks the whole index
    /// load. Missing directories are created.
    ///
    /// # Errors
    ///
    /// Returns [`SkillError::Validation`] if the draft is invalid, or an IO error if the write
    /// fails.
    pub fn write(&self, draft: &SkillDraft) -> SkillResult<PathBuf> {
        let rendered = draft.to_markdown()?;
        let path = self.path_for(&draft.name)?;

        let dir = self.skills_dir();
        std::fs::create_dir_all(&dir)?;

        // `NamedTempFile::persist` uses replace-existing semantics on Windows as well as Unix.
        // A unique name also lets independent writers update different skills concurrently.
        let mut temporary = tempfile::NamedTempFile::new_in(&dir)?;
        temporary.write_all(rendered.as_bytes())?;
        temporary.as_file().sync_all()?;
        temporary.persist(&path).map_err(|error| SkillError::Io(error.error))?;

        // On Unix, syncing the directory makes the rename durable across a power loss. Windows'
        // directory handles do not support this operation, while `persist` still supplies the
        // required atomic replace semantics there.
        #[cfg(unix)]
        std::fs::File::open(&dir)?.sync_all()?;

        tracing::debug!(skill = %draft.name, path = %path.display(), "wrote skill");
        Ok(path)
    }

    /// Removes the skill of this name, returning whether a file was present.
    ///
    /// # Errors
    ///
    /// Returns [`SkillError::Validation`] if the name is invalid, or an IO error if removal fails
    /// for a reason other than the file being absent.
    pub fn remove(&self, name: &str) -> SkillResult<bool> {
        let path = self.path_for(name)?;

        match std::fs::remove_file(&path) {
            Ok(()) => {
                tracing::debug!(skill = %name, "removed skill");
                Ok(true)
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(SkillError::Io(err)),
        }
    }

    /// Whether a skill of this name exists.
    ///
    /// # Errors
    ///
    /// Returns [`SkillError::Validation`] if the name is invalid.
    pub fn exists(&self, name: &str) -> SkillResult<bool> {
        Ok(self.path_for(name)?.is_file())
    }

    /// The root this writer targets.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::load_skill_index;
    use crate::parser::parse_skill_markdown;

    #[test]
    fn valid_names_are_accepted() {
        for name in ["a", "disk-triage", "sweep-2", "0", &"x".repeat(MAX_NAME_LEN)] {
            assert!(validate_skill_name(name).is_ok(), "should accept {name:?}");
        }
    }

    #[test]
    fn invalid_names_are_rejected() {
        for name in [
            "",
            "Disk-Triage",
            "disk triage",
            "disk_triage",
            "-leading",
            "trailing-",
            "../escape",
            "nested/name",
            "dot.name",
            &"x".repeat(MAX_NAME_LEN + 1),
        ] {
            assert!(validate_skill_name(name).is_err(), "should reject {name:?}");
        }
    }

    #[test]
    fn a_draft_round_trips_through_the_parser() {
        let draft = SkillDraft::new("disk-triage", "Diagnose low disk space")
            .with_body("1. Check largest directories.\n2. Report growth rate.")
            .with_version("1.2.3")
            .with_license("Apache-2.0")
            .with_compatibility("Requires read access to the filesystem")
            .with_tags(["ops", "storage"])
            .with_allowed_tools(["read_file", "run_command"])
            .with_references(["references/thresholds.md"])
            .with_trigger(true)
            .with_hint("name a mount point")
            .with_triggers(["*.log"])
            .with_metadata_entry("incident", Value::from("INC-42"));

        let rendered = draft.to_markdown().expect("renders");
        let parsed = parse_skill_markdown(Path::new("disk-triage.md"), &rendered).expect("parses");

        assert_eq!(parsed.name, "disk-triage");
        assert_eq!(parsed.description, "Diagnose low disk space");
        assert_eq!(parsed.version.as_deref(), Some("1.2.3"));
        assert_eq!(parsed.license.as_deref(), Some("Apache-2.0"));
        assert_eq!(parsed.compatibility.as_deref(), Some("Requires read access to the filesystem"));
        assert_eq!(parsed.tags, vec!["ops", "storage"]);
        assert_eq!(parsed.allowed_tools, vec!["read_file", "run_command"]);
        assert_eq!(parsed.references, vec!["references/thresholds.md"]);
        assert!(parsed.trigger);
        assert_eq!(parsed.hint.as_deref(), Some("name a mount point"));
        assert_eq!(parsed.triggers, vec!["*.log"]);
        assert_eq!(parsed.metadata.get("incident"), Some(&Value::from("INC-42")));
        assert_eq!(parsed.body, "1. Check largest directories.\n2. Report growth rate.");
    }

    #[test]
    fn a_minimal_draft_omits_unset_fields() {
        let rendered = SkillDraft::new("minimal", "Only the required fields")
            .with_body("Body.")
            .to_markdown()
            .expect("renders");

        for absent in ["version:", "license:", "tags:", "allowed-tools:", "trigger:", "metadata:"] {
            assert!(!rendered.contains(absent), "{absent} should be omitted from:\n{rendered}");
        }
        assert!(parse_skill_markdown(Path::new("minimal.md"), &rendered).is_ok());
    }

    #[test]
    fn an_empty_description_is_rejected() {
        let error = SkillDraft::new("named", "   ")
            .with_body("Body.")
            .to_markdown()
            .expect_err("an empty description must not be written");

        assert!(error.to_string().contains("description"), "got {error}");
    }

    #[test]
    fn an_invalid_name_is_rejected_before_any_file_is_touched() {
        let root = tempfile::tempdir().expect("tempdir");
        let writer = SkillWriter::new(root.path());

        assert!(writer.write(&SkillDraft::new("../escape", "Traversal attempt")).is_err());
        assert!(
            !root.path().join(SKILLS_DIR).exists(),
            "a rejected name must not create the skills directory"
        );
    }

    #[test]
    fn a_written_skill_is_discovered_by_the_index() {
        let root = tempfile::tempdir().expect("tempdir");
        let writer = SkillWriter::new(root.path());

        let path = writer
            .write(
                &SkillDraft::new("disk-triage", "Diagnose low disk space")
                    .with_body("Check the largest directories."),
            )
            .expect("writes");

        assert!(path.is_file());
        let index = load_skill_index(root.path()).expect("index loads");
        let found = index.find_by_name("disk-triage").expect("skill is indexed");
        assert_eq!(found.description, "Diagnose low disk space");
        assert_eq!(found.body, "Check the largest directories.");
    }

    #[test]
    fn writing_the_same_name_replaces_the_previous_skill() {
        let root = tempfile::tempdir().expect("tempdir");
        let writer = SkillWriter::new(root.path());

        writer.write(&SkillDraft::new("sweep", "First").with_body("One.")).expect("first write");
        writer.write(&SkillDraft::new("sweep", "Second").with_body("Two.")).expect("second write");

        let index = load_skill_index(root.path()).expect("index loads");
        assert_eq!(index.len(), 1, "replacing must not leave a duplicate");
        assert_eq!(index.find_by_name("sweep").expect("present").description, "Second");
    }

    #[test]
    fn concurrent_writers_do_not_share_a_temporary_path() {
        let root = tempfile::tempdir().expect("tempdir");
        let writer = SkillWriter::new(root.path());

        std::thread::scope(|scope| {
            let first = writer.clone();
            scope.spawn(move || {
                first
                    .write(&SkillDraft::new("first", "First skill").with_body("One."))
                    .expect("first write");
            });
            let second = writer.clone();
            scope.spawn(move || {
                second
                    .write(&SkillDraft::new("second", "Second skill").with_body("Two."))
                    .expect("second write");
            });
        });

        let index = load_skill_index(root.path()).expect("index loads");
        assert!(index.find_by_name("first").is_some());
        assert!(index.find_by_name("second").is_some());
    }

    #[test]
    fn writing_leaves_no_temporary_file_behind() {
        let root = tempfile::tempdir().expect("tempdir");
        let writer = SkillWriter::new(root.path());
        writer.write(&SkillDraft::new("sweep", "Description").with_body("Body.")).expect("writes");

        let leftovers: Vec<_> = std::fs::read_dir(writer.skills_dir())
            .expect("read dir")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.ends_with(".tmp"))
            .collect();

        assert!(leftovers.is_empty(), "found temporary files: {leftovers:?}");
    }

    #[test]
    fn remove_reports_whether_a_skill_was_present() {
        let root = tempfile::tempdir().expect("tempdir");
        let writer = SkillWriter::new(root.path());
        writer.write(&SkillDraft::new("sweep", "Description").with_body("Body.")).expect("writes");

        assert!(writer.exists("sweep").expect("exists"));
        assert!(writer.remove("sweep").expect("removes"), "the skill was present");
        assert!(!writer.remove("sweep").expect("second remove"), "already gone");
        assert!(!writer.exists("sweep").expect("exists"));
    }

    #[test]
    fn remove_rejects_an_invalid_name() {
        let root = tempfile::tempdir().expect("tempdir");
        let writer = SkillWriter::new(root.path());

        assert!(writer.remove("../escape").is_err());
    }
}
