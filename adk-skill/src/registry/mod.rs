//! Read-only client for the Vertex AI **Skill Registry** (v1beta1, Preview).
//!
//! The Skill Registry is the Gemini Enterprise Agent Platform **Build**
//! pillar service that stores versioned `SKILL.md` packages as zip archives
//! on `aiplatform.googleapis.com`. It is served from **regional** endpoints
//! (`https://{location}-aiplatform.googleapis.com`) under **v1beta1 only**,
//! and is currently available in three regions: `us-central1`,
//! `europe-west4`, and `us-east5`.
//!
//! > **Note:** do not confuse the Skill Registry with the **Agent Registry**.
//! > The Agent Registry is a separate Govern-pillar service for cataloging
//! > agents; the Skill Registry (this module) stores `SKILL.md` skill
//! > packages under `projects/*/locations/*/skills/*`.
//!
//! # Scope
//!
//! This client is **read-only**: get, list, semantic search, revision
//! listing/get, and content download. Lifecycle operations (create, update,
//! delete) are intentionally unimplemented — platform tooling owns them.
//!
//! Content download is [`SkillRegistryClient::fetch_skill_content`], which
//! decodes the base64 `zippedFilesystem` payload returned by `skills.get`
//! (there is no separate `:download` endpoint), verifies its SHA-256 digest
//! against the registry's `sha256` field, and safely extracts the archive
//! in memory — mirroring the server-side zip validation rules as
//! defense-in-depth (see [`extract`]).
//!
//! # Frontmatter parity
//!
//! Only `name` (as `displayName`), `description`, `license`, and
//! `compatibility` are first-class registry fields. Everything else in the
//! `SKILL.md` frontmatter — e.g. `metadata.category` and `allowed-tools` —
//! survives only inside the zip payload and is recovered by parsing the
//! extracted `SKILL.md`.

mod client;
pub mod extract;

pub use client::{
    ListSkillRevisionsResponse, ListSkillsResponse, RetrievedSkill, Skill, SkillContent,
    SkillRegistryClient, SkillRegistryConfig, SkillRevision, SkillSource, SkillState,
};
pub use extract::{
    MAX_ARCHIVE_BYTES, MAX_COMPRESSION_RATIO, MAX_DIRECTORY_DEPTH, MAX_ENTRIES,
    MAX_UNCOMPRESSED_BYTES, extract_skill_archive, extract_skill_archive_to_dir,
};
