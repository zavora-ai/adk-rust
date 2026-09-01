# Vertex AI Skill Registry

The Skill Registry is the Gemini Enterprise Agent Platform **Build**-pillar
service that stores versioned `SKILL.md` packages as zip archives on
`aiplatform.googleapis.com`. ADK-Rust integrates with it **consume-only**:
agents discover, download, and load centrally governed skills, while
provisioning (create, update, delete, publish) stays with platform tooling.

> **Note:** the Skill Registry API is **v1beta1 (Preview)**, served from
> regional `https://{location}-aiplatform.googleapis.com` endpoints in
> `us-central1`, `europe-west4`, and `us-east5` only. It is distinct from the
> Agent Registry, a separate Govern-pillar service for cataloging agents.

## Setup

Enable the feature on `adk-skill` (and on `adk-cli` for the commands):

```toml
[dependencies]
adk-skill = { version = "2.2.0", features = ["vertex-skill-registry"] }
```

Authentication uses Application Default Credentials
(`gcloud auth application-default login`). Configuration comes from
`GOOGLE_CLOUD_PROJECT` and `GOOGLE_CLOUD_LOCATION`, or explicit values.

## What the client does — and does not do

| Operation | Surface |
|-----------|---------|
| Get a skill (with payload) | `SkillRegistryClient::get_skill` |
| List skills | `SkillRegistryClient::list_skills` |
| Semantic search | `SkillRegistryClient::search_skills` |
| List / get revisions | `list_skill_revisions`, `get_skill_revision` |
| Download, verify, extract | `fetch_skill_content`, `fetch_skill_revision_content` |
| Create / update / delete | **Not implemented** — platform tooling owns lifecycle |

Every downloaded payload is SHA-256-verified against the registry's digest
and extracted with defense-in-depth zip validation (entry count, path
traversal, symlinks, duplicate names, size, compression ratio, and depth
limits) before any byte is used.

## Load registry skills into an agent

`load_skill_index_from_registry` produces a standard `SkillIndex`, so
injection, selection, and coordination work exactly as with filesystem
skills:

```rust
use adk_skill::registry::{
    RegistrySkillFilter, SkillRegistryClient, SkillRegistryConfig,
    load_skill_index_from_registry, merge_skill_indexes,
};
use adk_skill::load_skill_index;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = SkillRegistryClient::new_with_adc(SkillRegistryConfig::from_env()?)?;

    // Select by semantic search…
    let remote = load_skill_index_from_registry(
        &client,
        RegistrySkillFilter::by_query("quarterly reporting").with_top_k(5),
    )
    .await?;

    // …or by explicit names, optionally pinned to a revision.
    let pinned = load_skill_index_from_registry(
        &client,
        RegistrySkillFilter::by_names(["report-writer"]).with_revision("3"),
    )
    .await?;

    // Merge with project-local skills — local wins on name collision,
    // matching the project-local-over-global precedence of
    // load_skill_index_with_extras.
    let local = load_skill_index(".")?;
    let merged = merge_skill_indexes(local, remote);
    println!("{} skill(s) available ({} pinned)", merged.len(), pinned.len());
    Ok(())
}
```

Registry-backed `SkillDocument`s are built through the same parser and
hashing as filesystem skills; they carry a virtual `path` of
`{resource name}/SKILL.md` and no `last_modified` timestamp.

## The `search_skills` agent tool

`SkillSearchTool` exposes registry search to LLM agents. It is read-only and
concurrency-safe, so it participates in parallel tool dispatch:

```rust
use adk_skill::registry::{SkillRegistryClient, SkillRegistryConfig, SkillSearchTool};
use std::sync::Arc;

fn build_tool() -> adk_core::Result<SkillSearchTool> {
    let client = SkillRegistryClient::new_with_adc(SkillRegistryConfig::from_env()?)?;
    Ok(SkillSearchTool::new(Arc::new(client)))
}
```

Input is `{"query": string, "top_k"?: integer}`; output is a JSON array of
`{name, skillName, description}` objects, best match first (the API returns
no scores).

## CLI

The `adk-rust` binary gains two read-only subcommands behind the
`vertex-skill-registry` feature:

```toml
[dependencies]
adk-cli = { version = "2.2.0", features = ["vertex-skill-registry"] }
```

```bash
# Semantic search (best match first)
adk-rust skills search "quarterly reporting" --top-k 5

# Materialize a skill package under .skills/<skill-id>/
adk-rust skills pull report-writer

# Pin a revision and choose the target directory
adk-rust skills pull report-writer --revision 3 --dir vendored-skills
```

Both commands read `--project`/`--location` flags, falling back to
`GOOGLE_CLOUD_PROJECT`/`GOOGLE_CLOUD_LOCATION`. Pulled skills land in the
standard skills layout (`SKILL.md` plus its `references/`), so `skills list`,
`skills match`, and agent skill discovery pick them up immediately. There is
no `push` command — publishing goes through platform tooling.
