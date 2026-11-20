# Agent Skills

A skill is a Markdown file with YAML frontmatter, in the [agentskills.io](https://agentskills.io)
format, that holds the instructions for one kind of task. ADK-Rust indexes the skills of a
project, picks the one that best matches the user's message, and adds its body to what the model
receives. `adk-skill` discovers, parses, indexes and selects skills; `LlmAgent`, `CodeActAgent`
and `Runner` use it behind their `skills` feature.

| Approach | Use it when | Set with |
|----------|-------------|----------|
| Agent injection | One agent should apply the best-matching skill to each user message | `LlmAgentBuilder::with_skills_from_root` and the other [builder methods](#give-an-agent-skills) |
| Runner injection | Every agent of a runner should see the skill, and the session should record it | [`Runner::with_skill_injector`](#inject-at-the-runner) |
| Progressive disclosure | The model should list skills and load one when it decides to | [`SkillToolset`](#progressive-disclosure) |
| Skill Registry | Skills come from the Vertex AI Skill Registry | [Vertex AI Skill Registry](skill-registry.md) |

## Install

`adk-rust`'s `skills` feature, in `standard` and above, re-exports `adk-skill` as
`adk_rust::skill` and turns on the `skills` features of `adk-agent` and `adk-runner`:

```toml
[dependencies]
adk-rust = { version = "3.0.0", features = ["standard"] }
```

> **Note:** `skills` is not a default feature of `adk-agent` or `adk-runner`, and `minimal`
> compiles no skills. A project that depends on those crates directly asks for it; without it
> the skill builder methods and `Runner::with_skill_injector` do not exist.

```toml
[dependencies]
adk-agent = { version = "3.0.0", features = ["skills"] }
adk-runner = { version = "3.0.0", features = ["skills"] }
adk-skill = "3.0.0"
```

| Feature | Crate | Adds |
|---------|-------|------|
| `skills` | `adk-agent`, `adk-runner`, `adk-rust` | The skill builder methods and `Runner::with_skill_injector` |
| `progressive-disclosure` | `adk-skill` (`skills-progressive-disclosure` on `adk-agent` and `adk-rust`) | `SkillToolset` |
| `vertex-skill-registry` | `adk-skill`, `adk-rust` | The [Skill Registry](skill-registry.md) client |

## Write a skill

Put each skill under `.skills/` (or `.claude/skills/`) at the project root. A skill can be one
Markdown file, or a directory whose `SKILL.md` holds the skill and whose `references/`,
`scripts/` and `agents/` subdirectories hold its resources:

```text
my-project/
├── .skills/
│   ├── incident-triage.md
│   └── release-notes/
│       ├── SKILL.md
│       └── references/
│           └── example.md
├── AGENTS.md
└── src/
    └── main.rs
```

`.skills/release-notes/SKILL.md`:

```markdown
---
name: release-notes
description: Write release notes from a list of merged changes. Use when asked for release notes or a changelog entry.
tags: [writing, release]
---
Group the changes under the headings Added, Changed and Fixed.
Write one line per change, in the present tense, starting with the crate name in backticks.
Leave out changes that only touch tests or CI.
```

| Frontmatter field | Required | Holds |
|-------------------|----------|-------|
| `name` | Yes | The skill's identifier: lowercase letters, digits and hyphens |
| `description` | Yes | What the skill is for; selection weighs it heavily |
| `tags` | No | Labels that selection scores and `SelectionPolicy` filters on |
| `version`, `license`, `compatibility` | No | Metadata carried on the skill |
| `allowed-tools` | No | The tools the skill uses: a list, or one string of names separated by spaces or commas |
| `references` | No | Supporting files the skill reads |
| `trigger`, `hint`, `triggers`, `metadata` | No | Metadata for hosts; selection and injection do not act on them |

### What is indexed

`load_skill_index(root)` builds a `SkillIndex` from every file it discovers under `root`:

| Files | Parsing |
|-------|---------|
| `.skills/**/*.md` and `.claude/skills/**/*.md` | Frontmatter with `name` and `description` is required |
| `AGENTS.md`, `AGENT.md`, `CLAUDE.md`, `GEMINI.md`, `COPILOT.md` and `SKILLS.md` anywhere in the tree, and `SOUL.md` at the root | Frontmatter is optional; without it the file becomes a skill named after the file |

Files under a skill's `references/`, `scripts/` and `agents/` directories are resources, not
skills. Discovery skips `.git`, `target`, `node_modules`, `dist`, `build` and similar
directories. A file that fails to parse is left out of the index with a warning, so one broken
skill does not stop the rest from loading. Each skill gets a content hash and an id made of its
name and the hash's first 12 characters.

`load_skill_index_with_extras(root, &extra_dirs)` adds skill directories outside the project,
such as a global `~/.config/adk/skills/`. When two skills share a name, the one under the
project's `.skills/` or `.claude/skills/` wins.

## Give an agent skills

The agent below loads the project's skills when it is built. For each user message it selects
the best-matching skill and prepends the skill's body to that message in the model request. A
`before_model_callback` prints the first line the model receives:

```rust
use adk_rust::futures::StreamExt;
use adk_rust::prelude::*;
use adk_rust::session::{CreateRequest, SessionService};
use adk_rust::{SessionId, UserId};
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let model = AnthropicClient::new(AnthropicConfig::new(
        std::env::var("ANTHROPIC_API_KEY")?,
        "claude-sonnet-5-5",
    ))?;

    let agent = LlmAgentBuilder::new("assistant")
        .instruction("You are a concise engineering assistant.")
        .model(Arc::new(model))
        // Indexes ./.skills, ./.claude/skills and the convention files under ".".
        .with_skills_from_root(".")?
        .before_model_callback(Box::new(|_ctx, request| {
            Box::pin(async move {
                let first_line = request
                    .contents
                    .last()
                    .and_then(|content| content.parts.first())
                    .and_then(Part::text)
                    .and_then(|text| text.lines().next())
                    .unwrap_or_default()
                    .to_string();
                println!("model receives: {first_line}");
                Ok(BeforeModelResult::Continue(request))
            })
        }))
        .build()?;

    let sessions = Arc::new(InMemorySessionService::new());
    let runner = Runner::builder()
        .app_name("skills_demo")
        .agent(Arc::new(agent))
        .session_service(sessions.clone())
        .build()?;
    let session = sessions
        .create(CreateRequest {
            app_name: "skills_demo".to_string(),
            user_id: "dev".to_string(),
            session_id: None,
            state: HashMap::new(),
        })
        .await?;

    let message = "Write release notes for these merged changes: adk-model makes \
                   gemini-3.8-flash the default model; adk-session fixes a Redis key \
                   collision; CI caches the docs build.";
    let mut events = runner
        .run(UserId::new("dev")?, SessionId::new(session.id())?, Content::new("user").with_text(message))
        .await?;
    while let Some(event) = events.next().await {
        let event = event?;
        if event.is_final_response()
            && let Some(content) = event.content()
        {
            for text in content.parts.iter().filter_map(Part::text) {
                println!("{text}");
            }
        }
    }
    Ok(())
}
```

Run from the project above, it prints `model receives: [skill:release-notes]`, then notes grouped
under Added and Fixed with the CI change left out. A message no skill matches is sent unchanged.

| `LlmAgentBuilder` method | Does |
|--------------------------|------|
| `with_skills(index)` | Uses a `SkillIndex` you loaded |
| `with_skills_from_root(root)` | Loads the index from `root`; fails the build with the load error |
| `with_auto_skills()` | `with_skills_from_root(".")` |
| `with_skill_policy(policy)` | Sets the [`SelectionPolicy`](#selection) (default: the top skill scoring at least 1.0) |
| `with_skill_budget(chars)` | Cuts the injected body at `chars` characters (default 2,000) |

`CodeActAgent`'s builder has the same five methods. It selects a skill from the user's message
in the same way and puts the skill's block at the start of its system prompt; see
[CodeAgent](../agents/code-agent.md).

The injected block wraps the skill's body:

```text
[skill:release-notes]
Group the changes under the headings Added, Changed and Fixed.
...
[/skill]

Write release notes for these merged changes: ...
```

## Selection

`select_skills(&index, query, &policy)` scores each skill against the words of the query. Every
query word found in a field adds that field's weight:

| Field | Weight |
|-------|--------|
| `name` | 4.0 |
| `description` | 2.5 |
| `tags` | 2.0 |
| Body | 1.0 |

The sum is divided by the square root of the number of distinct words in the body, so a long
body does not outweigh a focused skill. Tokenization is Unicode-aware. The same query against the
same index always gives the same scores; no model is involved.

| `SelectionPolicy` field | Default | Effect |
|-------------------------|---------|--------|
| `top_k` | `1` | How many matches to return; injection uses the first |
| `min_score` | `1.0` | Matches scoring below it are dropped |
| `include_tags` | empty | When set, only skills with one of these tags are scored |
| `exclude_tags` | empty | Skills with any of these tags are dropped |

Ties are broken by name, then by path. To see how a query scores, run the selection yourself:

```rust
use adk_rust::skill::{SelectionPolicy, load_skill_index, select_skills};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let index = load_skill_index(".")?;
    for skill in index.summaries() {
        println!("indexed {} from {}", skill.name, skill.path.display());
    }

    let policy = SelectionPolicy { top_k: 3, min_score: 0.0, ..SelectionPolicy::default() };
    for found in select_skills(&index, "write release notes for the merged changes", &policy) {
        println!("{:.2} {}", found.score, found.skill.name);
    }
    Ok(())
}
```

## Inject at the runner

`Runner::with_skill_injector` selects a skill for each run's user message before any agent runs,
so every agent of the runner sees the skill. The runner prepends the block to the message before
it saves it, so the session records the message with the block, and the run's span carries
`adk.skills.selected_name` and `adk.skills.selected_id`. Injection runs before the plugins'
`on_user_message` callbacks.

```rust
use adk_rust::prelude::*;
use adk_rust::skill::{SkillInjector, SkillInjectorConfig};
use std::sync::Arc;

fn runner_with_skills(
    agent: Arc<dyn Agent>,
) -> std::result::Result<Runner, Box<dyn std::error::Error>> {
    let injector = SkillInjector::from_root(".", SkillInjectorConfig::default())?;
    Ok(Runner::builder()
        .app_name("skills_demo")
        .agent(agent)
        .session_service(Arc::new(InMemorySessionService::new()))
        .build()?
        .with_skill_injector(injector))
}
```

| Where the skill is injected | What the model receives | What the session keeps |
|-----------------------------|-------------------------|------------------------|
| `LlmAgentBuilder::with_skills` and related methods | The block before the user's message | The message as the user sent it |
| `Runner::with_skill_injector` | The block before the user's message | The message with the block |

| `SkillInjectorConfig` field | Default | Effect |
|-----------------------------|---------|--------|
| `policy` | `SelectionPolicy::default()` | Selection |
| `max_injected_chars` | `2000` | Cuts the injected body |
| `global_skills_dir` | `None` | A further skill directory; project skills win on a name clash |
| `extra_paths` | empty | Further skill directories |

`Runner::with_auto_skills_mut(root, config)` builds the injector in place. The index is a snapshot
taken when the injector is built: `SkillInjector::reloaded()` rescans the root and returns a new
injector.

## Progressive disclosure

With `adk-skill`'s `progressive-disclosure` feature, `SkillToolset` gives the model the skills as
tools instead of injecting a body into every matching message:

| Tool | Returns |
|------|---------|
| `list_skills` | Each skill's name and description |
| `load_skill` | One skill's instructions; also activates its `allowed-tools` for the next model turn |
| `load_skill_resource` | A file from the skill's `references/`, `assets/` or `scripts/` directory |

The toolset adds its own protocol to each model request, so the agent's instruction need not
describe it. A resource is readable only once its skill is loaded, and paths that leave the skill
directory are refused.

```rust
use adk_rust::prelude::*;
use adk_rust::skill::{SkillToolset, SkillToolsetConfig, load_skill_index};
use adk_rust::ToolRegistry;
use std::sync::Arc;

fn agent_with_skill_tools(
    model: Arc<dyn Llm>,
    tools: Arc<dyn ToolRegistry>,
) -> std::result::Result<LlmAgent, Box<dyn std::error::Error>> {
    let index = Arc::new(load_skill_index(".")?);
    // `tools` resolves the names a skill lists in `allowed-tools`.
    let skills = SkillToolset::new(index, tools, SkillToolsetConfig::default());
    Ok(LlmAgentBuilder::new("assistant").model(model).toolset(Arc::new(skills)).build()?)
}
```

## Write skills from code

`SkillWriter` writes a `SkillDraft` to `<root>/.skills/<name>.md`, where `load_skill_index` finds
it. The write goes through a temporary file that replaces the destination, so a crash never
leaves a half-written skill. `validate_skill_name` rejects a name outside `[a-z0-9-]`, which keeps
a name such as `../escape` from leaving the directory.

```rust
use adk_rust::skill::{SkillDraft, SkillWriter, load_skill_index};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = SkillWriter::new(".").write(
        &SkillDraft::new("disk-triage", "Diagnose low disk space. Use when a disk alert fires.")
            .with_body("1. List the largest directories.\n2. Report the growth rate.")
            .with_tags(["ops"]),
    )?;
    println!("wrote {}", path.display());

    let index = load_skill_index(".")?;
    assert!(index.find_by_name("disk-triage").is_some());
    Ok(())
}
```

## Related

- [Vertex AI Skill Registry](skill-registry.md) — skills from the managed registry
- [Harness configuration](../harness/configuration.md) — the harness loads skills progressively
- [A2A Getting Started](../a2a/getting-started.md) — `A2aServer::builder().skill_index(..)`
  lists a skill index on the agent card
- [`adk-skill` on docs.rs](https://docs.rs/adk-skill) — `ContextCoordinator`, `SkillInjector::build_plugin`
  and the rest of the API

---

**Next**: [Vertex AI Skill Registry →](skill-registry.md)
