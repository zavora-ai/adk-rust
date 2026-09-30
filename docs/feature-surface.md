# The `adk-rust` feature surface

`adk-rust` is both a re-export facade and a dependency menu. Every new capability
arrives as a new top-level feature, so the menu has grown to **143 features**
across 44 crates. This records what the surface currently is, which parts of it
are load-bearing, and the rule that keeps it from growing further.

`scripts/check-feature-growth.py` enforces the rule in CI and as a lefthook gate.
`--report` prints the surface by axis; the default mode is the gate.

## The principle

> A feature earns its place when turning it off changes **what links**. When the
> choice is made at run time, it belongs in configuration.

That single test separates the surface into the parts that are build-time
capability and the parts that are runtime preference wearing a compile-time
flag. It also explains why the count is hard to reduce further: Cargo unifies
features additively, so a fine-grained flag buys less control than its name
suggests — any crate in the graph that enables `postgres-session` links it for
everyone.

## The three-part split

| Bucket | Count | Disposition |
|---|---:|---|
| Advertised | 87 | What the docs tell users to write. Unchanged. |
| Unadvertised members | 41 | Keep working and keep their feature key. Folded into a bundle; documented as reachable through it. |
| Deprecated no-ops | 19 | Kept for semver, scheduled for removal at the next major. |
| **Total** | **143** | |

Nothing is removed in a minor release. `STABILITY.md` (Deprecation Policy)
requires an `#[deprecated]` announcement and an N+2 minor grace period before a
public item goes away; features are held to the same standard even though
`cargo-semver-checks` does not cover them.

### Why not 25

An earlier estimate of 143 → 25 was wrong, and the resolver is what shows why.
**24 features are choose-one selectors** — twelve LLM providers, thirteen
database backends minus the default, five realtime transports. They are
semantically exclusive: `features = ["gemini", "postgres-session"]` is a
different build from either alone. Collapsing them into one bundle per axis
would change what a manifest means, not just how long the list is. The
achievable reduction is the advertised surface, not the feature count.

## The axes

| Axis | Members | Bundle | Note |
|---|---:|---|---|
| tier | 7 | — | `minimal` (default), `standard`, `enterprise`, `full`, `gemini-agent-platform{,-full}` |
| capability | 45 | — | `agents`, `models`, `tools`, `server`, `runner`, `eval`, … |
| choose-one | 24 | — | providers, database backends, realtime transports |
| cli | 4 | `cli` | 2 more collapse, 2 stay |
| action | 3 | `action` / `action-full` | 5 collapse into `action-full` |
| media | 2 | `audio` / `audio-onnx` | 9 demoted |
| graph | 1 | `graph` | 6 demoted to members |
| mcp | 1 | `mcp` / `mcp-http` | 2 demoted |
| gcp | 0 | `gemini-agent-platform` | all 11 demoted; already bundled |
| code | — | `code-tools` | 3 demoted |

## The 19 deprecated no-ops

Each activates the same dependencies as a sibling. None is removed before the
next major; each keeps its feature key so no existing manifest breaks.

| Feature | Replacement | Why |
|---|---|---|
| `cerebras`, `fireworks`, `mistral`, `perplexity`, `sambanova`, `together` | `OpenAICompatibleConfig::<provider>()` | One OpenAI client; the provider is a base URL |
| `whisper-onnx`, `distil-whisper`, `moonshine`, `chatterbox` | `audio-onnx` | One `ort` runtime; the model is a checkpoint path |
| `action-code`, `action-db`, `action-email` | `action` | No dependency beyond `action` itself |
| `action-rss` | `action-http` | An HTTP node reading a feed |
| `a2a`, `agent-registry` | `server` | No dependency beyond `adk-server` |
| `cli-deepseek`, `cli-groq` | `cli` | The CLI reads the provider from the environment |
| `personas` | `eval` | Persona fixtures link `adk-eval` only |

The six OpenAI-compatible aliases are the clearest case. `AGENTS.md` already
documents them as backward-compat aliases over one 1,261-line client, so the
feature keys carry nothing but a spelling of a base URL.

## The 19 that stay despite sharing dependencies

Shared dependency effect is not automatically a defect. These are recorded as
`kind="keep"` with the reason, so a future reader does not "fix" them:

- `azure-ai`, `deepseek`, `groq`, `openrouter` — four distinct clients over one
  reqwest stack.
- `gemini-interactions` — a distinct Beta transport on the Gemini client.
- `example-store` / `vertex-agent-registry` — distinct registries.
- `vertex-rag` / `agent-retrieval` — RAG Engine versus managed VectorStore.
- `codeact`, `record-payloads`, `mcp-sampling`, `graph-time-travel`,
  `audio-fx`, `optimize`, `video-avatar` — capability that adds no new crate.

## The growth rule

A new `adk-rust` feature must do one of:

1. Pull a dependency that no existing feature pulls, **or**
2. Join a declared axis as a member of a bundle, **or**
3. Carry a recorded decision in `ALIASES` saying why it is a runtime choice.

Otherwise the gate fails with the conflicting features named. The rule is the
one `gemini-agent-platform` has used since #556; it now covers the whole
surface instead of one family.

Verified discriminating — adding a plausible next feature
(`llama-cpp = ["models", "adk-model/openai"]`) fails the gate and names the
seven features it duplicates.

## Adding an axis

When a genuinely new family appears, add a prefix to `AXES` in the script. The
gate does not require it — `capability` is the fallback — but an ungrouped
feature is a feature nobody will find, and the axis table is what the docs are
generated from.
