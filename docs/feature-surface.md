# The `adk-rust` feature surface

`adk-rust` is both a re-export facade and a dependency menu. Every new capability
arrives as a new top-level feature, so the menu has grown to **154 features**
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
| Features | 154 | What `--report` lists by axis. Unadvertised members of a bundle keep working and keep their feature key. |
| Deprecated no-ops | 0 | The 19 aliases were folded into their siblings in 3.0.0; see [below](#the-19-aliases-removed-in-300). |
| **Total** | **154** | |

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
| capability | 54 | — | `agents`, `models`, `tools`, `server`, `runner`, `eval`, … |
| choose-one | 26 | — | providers, database backends, realtime transports; `mistralrs` links `adk-mistralrs`, in-process local inference (spec 3.1 §8.4) |
| cli | 7 | `cli` | `cli` compiles DeepSeek and Groq, which share its reqwest stack; `cli-mistralrs` links `adk-mistralrs` through the CLI (spec 3.1 §8.1) |
| action | 4 | `action` / `action-full` | `action` includes the database and email placeholders, `action-http` the RSS node |
| media | 10 | `audio` / `audio-onnx` | `audio-onnx` compiles every ONNX speech model on one `ort` runtime |
| graph | 13 | `graph` | 6 demoted to members |
| mcp | 3 | `mcp` / `mcp-http` | 2 demoted |
| harness | 12 | `harness` | the `harness-*` packs and add-ons are members (spec 3.1 §8.4); `harness-encrypted` links `adk-session`'s envelope and the OS keychain, `harness-memory-local` links the `ort` runtime (spec 3.1 §7.12, §7.15.2); `harness-kernel` links `adk-kernel`, the agent kernel (spec 3.2 §8.2) |
| surface | 1 | `surface` | links `adk-surface`, the engine and web home every agent surface runs on (spec 3.3 §6.1), and with `cli` the launcher's web home; opt-in, in no tier. One feature per surface (`web`, `tui`, `channels`, `mobile`, …) stays on `adk-surface`, so the umbrella grows by this one key |
| website | 1 | `website` | links `adk-website`, the AWP 2.0 website product, and `adk-site`, its pipeline (spec 3.5 §7.1, D-14); turns on the edge (`adk-server/awp`), AWP signing and the kernel, and with `cli` the `adk-rust site` and `awp` commands. In `full`. Hosts, generators and backends stay features of `adk-site` and `adk-kernel`, so the umbrella grows by this one key |
| autonomy | 1 | `autonomy` | everything an agent needs to run unattended ([guide](official_docs/autonomy/index.md)): turns on `harness-kernel`, `server`, `sessions` and `auth`, and the member features `adk-server` `background`, `webhook-ingress` and `approvals`, `adk-core` `cron`, `adk-session` and `adk-harness` `sqlite`, and `adk-auth` `signed-audit`. Over `harness-kernel` it links the cron scheduler (`cron`, `chrono-tz`) and compiles in background runs, the approvals service and the harness's SQLite stores. In `enterprise` and `full`, so the umbrella grows by this one key |
| vision | 2 | `vision` | `vision` links `adk-vision`, local-first perception (spec 3.7 §7.1), with the fetcher, the cloud describers, Meta `sam-3.1` and the image privacy guardrail, and turns on the harness's vision pack and with `cli` `adk-rust vision`; pure Rust, in `full`. It also forwards the streams of spec 3.7 §7.13 — `stream`, `track` (`trackforge`), `gemini-video`, and every frame source: `rtsp` (`retina`), `ffmpeg` (`ffmpeg-sidecar`, which spawns a user-installed binary and links nothing native), `screen`, `browser` and `surface-camera` (no dependency beyond `stream`) — because each is pure Rust with no native runtime, and a stream without a source is not usable; only the test fixtures (`stream-fixtures`) stay on `adk-vision`. With `harness-kernel` and `computer-use` it turns on their `vision` features, the kernel's `VisionWakeSource` as a watch sink and computer-use observations as a frame source, adding no key. `vision-local` links the native runtimes the on-device backends need — `ort` for RF-DETR, EfficientSAM, SAM 2.1 and SigLIP 2, `oar-ocr` for PP-OCRv6 and `oar-ocr-vl` on Candle for PaddleOCR-VL-1.6 — and is in no tier (§13 Q11). Backends and providers stay features of `adk-vision` |
| gcp | 13 | `gemini-agent-platform` | all demoted; already bundled |
| code | — | `code-tools` | 3 demoted |

## The 19 aliases removed in 3.0.0

Each activated the same dependencies as a sibling. 3.0.0 removed them and folded what they
compiled into that sibling, so no capability is lost; the
[migration guide](official_docs/migration/2.x-to-3.0.md#adk-rust-features) maps each one.

| Feature | Replacement | Why |
|---|---|---|
| `cerebras`, `fireworks`, `mistral`, `perplexity`, `sambanova`, `together` | `openai` with `OpenAICompatibleConfig::<provider>()` | One OpenAI client; the provider is a base URL |
| `whisper-onnx`, `distil-whisper`, `moonshine`, `chatterbox` | `audio-onnx`, which now compiles all four | One `ort` runtime; the model is a checkpoint path |
| `action-code`, `action-db`, `action-email` | `action`, which now compiles the database and email placeholders | No dependency beyond `action` itself |
| `action-rss` | `action-http`, which now compiles the RSS node | An HTTP node reading a feed |
| `a2a`, `agent-registry` | `server`, which now compiles `adk_rust::registry` | No dependency beyond `adk-server` |
| `cli-deepseek`, `cli-groq` | `cli`, which now compiles both providers | No dependency beyond the CLI's reqwest stack |
| `personas` | `eval`, which now compiles `adk_rust::personas` | Persona fixtures link `adk-eval` only |

The member crates keep a feature only where it gates code: `adk-audio`'s checkpoint
features, `adk-graph`'s `action-db`, `action-email` and `action-rss`, `adk-server`'s
`agent-registry`, `adk-eval`'s `personas` and `adk-cli`'s `deepseek` and `groq` remain.
`adk-model`'s seven preset aliases (`cerebras`, `fireworks`, `mistral`, `perplexity`,
`sambanova`, `together`, `xai`), `adk-audio`'s `streaming`, `adk-auth`'s `adk-server`,
`adk-gemini`'s `studio` and `adk-telemetry`'s `metrics` compiled nothing and are removed.

## The 23 that stay despite sharing dependencies

Shared dependency effect is not automatically a defect. These are recorded as
`kind="keep"` with the reason, so a future reader does not "fix" them:

- `azure-ai`, `deepseek`, `groq`, `muse`, `openrouter` — five distinct clients over
  one reqwest stack.
- `gemini-interactions` — a distinct Beta transport on the Gemini client.
- `example-store` / `vertex-agent-registry` — distinct registries.
- `vertex-rag` / `agent-retrieval` — RAG Engine versus managed VectorStore.
- `codeact`, `record-payloads`, `mcp-sampling`, `graph-time-travel`,
  `audio-fx`, `optimize`, `video-avatar`, `harness-hooks` — capability that adds no new crate.
- `graph-sqlite` / `graph-postgres` / `postgres-session` — distinct stores over one sqlx crate.
- `memory`, `sessions` — the trait surfaces, which link their default backends.

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
