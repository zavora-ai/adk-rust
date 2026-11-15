# Spec 3.6 — Rust & Beyond Studio: video production by an ADK-Rust team

**Status:** Draft for owner review. Phase 0 started 2026-11-15: findings and run plan in §12.
**Grounded at:** `main` @ `459ec9558` (2026-11-15).
**Depends on:** specs 3.1 (harness), 3.2 (kernel), 3.3 (surfaces). Spec 3.4's `self-improve` feature is not in
`adk-harness` yet and is not required.

## 1. Summary

ADK-Rust 3.0.0 launches with a season of the *Rust & Beyond* podcast and a series of educational videos on YouTube.
Episodes 1–3 were two voices over slides and a still image of the set. Season 3 puts the same two hosts, **James** and
**Ada**, in a 3D set rendered in Unreal Engine 5.9, performing their lines with lip sync, gestures and camera coverage.

The promise is that **ADK-Rust makes the videos**. A multi-agent team built on the harness, the kernel and
`adk-graph` researches, writes, voices, builds the set, directs, renders, edits, checks and uploads every episode
through governed tool calls. Humans approve at four named gates and do nothing else.

When the team cannot do something, that is a gap in ADK-Rust or in a tool. Production parks at a checkpoint. The gap
becomes an issue and a PR, verified live. Once the fix merges, production resumes from the checkpoint. The gaps found and
fixed this way become content for the campaign.

## 2. Goals

| # | Goal | Pass condition |
|---|------|----------------|
| G1 | A season of production-quality videos for the 3.0.0 launch | Every episode meets the quality bar in §9 |
| G2 | Every frame made by ADK-Rust agents | The production receipt (§6) shows zero open interventions |
| G3 | The v2 cast, carried into 3D | The owner signs off James, Ada and the set against the v2 key art |
| G4 | Gaps found by real production become merged fixes | Each gap has an issue, a live-verified PR and a resumed run |
| G5 | Show the whole platform, not a script runner | The team uses teams, graph checkpoints, the kernel, surfaces, MCP, computer use, eval, audit and budgets (§7) |
| G6 | The team is reusable | An asset-free scenario of the team runs in `examples/` against scripted tools |

## 3. Non-goals

- A general video editor or a non-linear editing UI.
- Training or fine-tuning models.
- Realistic deepfakes of real people. The hosts are stylised characters.
- Live streaming. Episodes are rendered offline.
- Shipping Unreal, Blender or licensed Fab assets inside any ADK-Rust crate or public repository.

## 4. The campaign

### 4.1 Season plan

| Slot | Format | Length | Content |
|------|--------|--------|---------|
| Teaser | Trailer, 16:9 and 9:16 | 45–60 s | The hosts in 3D for the first time. "This trailer was made by ADK-Rust." |
| Episode 4 (flagship) | Podcast, 3D set | 25–35 min | ADK-Rust 3.0.0: the harness, the kernel, surfaces, the agentic web |
| Series, one a week | Explainer: hosts + live demo cutaways | 8–15 min each | One per v3 pillar (below) |
| Shorts | 9:16, cut from each episode | under 60 s | One idea and one command per short |
| Behind the scenes | Documentary | 15–20 min | The studio itself, its receipts and the PRs it filed |

Series topics, one pillar each:

1. The harness — `adk-rust` in your project, modes, approvals, the sandbox (spec 3.1)
2. The kernel — an always-on agent that sleeps, wakes and recovers (spec 3.2)
3. Surfaces — one approval on every screen (spec 3.3)
4. Steering — redirecting a running turn (#1382)
5. Governance — one governed path for every tool call
6. Durable runs — graph checkpoints, resume and time travel
7. The agentic web — AWP 2.0 and `adk-website` (spec 3.5)
8. Realtime voice — agents that listen and talk

### 4.2 Calendar

Dates are relative to **T**, the day 3.0.0 is on crates.io. Demos are recorded against the published crates, so no
episode is final before T.

| When | Release |
|------|---------|
| T − 14 days | Teaser |
| T | Episode 4, README and docs embeds |
| T + 7 days, weekly | One series episode, with two or three shorts |
| After the series | Behind the scenes |

### 4.3 What every video carries

- **The receipt line** in the description: agents, tool calls, human decisions, spend and PRs filed, with a link to the
  signed receipt (§6).
- **YouTube chapters** from the script's chapter structure, as episodes 2–3 already do.
- **Captions** from the script, re-aligned to the final audio.
- **The synthetic-content disclosure** set on upload, because "made by agents" is the message.

## 5. The cast and the set

### 5.1 From the v2 key art

The source is `docs/podcast/ep3-video/public/set.png` (also `docs/podcast/Podcast.webp`).

| | James | Ada |
|---|-------|-----|
| Look | Cyborg: chrome skull-like head, cyan visor, over-ear headset with a cyan ring, exposed mechanical neck with orange-lit circuitry, robotic hands, black hoodie over a grey tee | Chin-length dark bob with a magenta streak, a faint pink circuit line on one cheek, burnt-orange bomber jacket, black turtleneck |
| Side of the set | Blue and teal, hex-pattern wall | Red and orange, code on the wall |
| Props | Laptop, wrist band, name plate | Enamel mug, stacked books, name plate |
| Voice | `Fenrir` | `Kore` |

The set: a round dark table, two boom-arm broadcast microphones, the chrome *RUST & BEYOND* title, an `ADK-RUST` neon
sign, a red `LIVE` sign, and floating holographic panels (waveform, Ferris, checklist).

### 5.2 Character route

| Route | How | Lip sync | Cost of the route |
|-------|-----|----------|-------------------|
| **A. MetaHuman (default)** | Ada authored in MetaHuman Creator in the editor (`MetaHumanCharacter` plugin). James is a MetaHuman whose face keeps its rig under a chrome material, with the visor, headset, neck and hands as attached meshes | MetaHuman Animator's audio-driven animation from each host's voice stem (`MetaHumanAnimator`, `MetaHumanCoreML` ship with UE 5.9 here) | Highest fidelity; one rig standard for both hosts; body animation retargets from the UE mannequin |
| B. Stylised meshes | Image-to-3D of the key art (Tripo or Meshy), cleaned, retopologised and rigged in Umbo Studio | Needs ARKit-style face blendshapes, which Umbo's human rig does not have ("no fingers or face bones") | Closer to the 2D art; lip sync becomes a new rigging problem |

Accessories and props (visor, headset, microphones, mug, title, signs) come from Fab, Tripo or Meshy. Umbo Studio
cleans them (check → retopo → bake → export) for Unreal.

### 5.3 Voice continuity

The hosts keep their v2 voices: Gemini multi-speaker TTS, `gemini-3.1-flash-tts-preview`, voices `Fenrir` and `Kore`
(`docs/podcast/pipeline/config.json`). Multi-speaker TTS returns one mixed track per exchange, but lip sync needs a
stem per speaker. The voice stage aligns words to the script with `adk-audio` STT word timestamps and splits each
exchange into James and Ada stems.

## 6. The promise and how it is kept

| Rule | Mechanism |
|------|-----------|
| **R1.** Every production step is a governed tool call by an ADK-Rust agent | Harness modes and `ToolPolicy`; the action ledger records each call |
| **R2.** Humans act only at four gates: script, cast and set look, final cut, publish | Graph interrupts rendered as surface decision cards (spec 3.3) |
| **R3.** Any other human action is an **intervention**: logged with a reason, and open until a fix merges or the owner accepts it | An `intervention` record per episode; the receipt lists them |
| **R4.** Every claim in a script cites a source in the repository; every demo runs live against the published crates | Fact-check stage; demo stage runs `cargo` against crates.io |
| **R5.** Each episode publishes a **production receipt** | Built from the action ledger and signed with `adk-auth`'s signed audit: agents, tool calls per server, models and spend, decisions, interventions, PRs |

Opening the Unreal project and starting its MCP server once per machine is setup, not an intervention.

## 7. Architecture

The layers follow spec 3.5's split.

```
 Product   Rust & Beyond Studio — the team, the episode graph, the campaign      (private studio repo)
 ───────────────────────────────────────────────────────────────────────────────────────────────────
 Runtime   adk-harness (packs, modes, approvals) · adk-kernel (wakes, durable processes)
           adk-graph (checkpoints, interrupts, resume) · adk-surface (decision cards, notifiers)
           adk-eval (rubric + LLM judge) · adk-auth (signed audit) · adk-runner (budgets, ledgers)
 ───────────────────────────────────────────────────────────────────────────────────────────────────
 Tools     unreal-mcp (UE 5.9, Streamable HTTP) · umbo (stdio) · meshy · Tripo · computer-use-mcp
           media MCP servers (speech, avtool/ffmpeg, image, music, video) · browser · YouTube driver
```

**Where things live:**

| What | Where | Why |
|------|-------|-----|
| Capability gaps (§11) | `zavora-ai/adk-rust`, as PRs | They are framework features any user needs |
| The studio app, episode content, the Unreal project, renders | A private repo, `zavora-ai/rust-and-beyond-studio` (proposed) | Fab's licence forbids redistributing source assets; renders are tens of GB |
| An asset-free scenario of the team | `adk-rust/examples/showrunner` | CI runs the graph against scripted MCP servers |

The studio depends on `adk-rust` by git revision until T, then on the published 3.0.x crates.

### 7.1 Platform subsystems the studio uses

| Subsystem | Use |
|-----------|-----|
| Teams (`TeamSpec`, `AgentTool`) | The crew in §8, each a member with its own tools and model |
| `adk-graph` | The episode graph (§8.2): checkpoints per stage, interrupts at the gates, resume after a gap fix |
| `adk-kernel` | Long renders: the render stage sleeps on a `FileChange` wake for the render's output and wakes when it lands, holding no model turn |
| Harness packs | Computer use (Fab, apps with no API), browser (Tripo Studio web, YouTube Studio checks) |
| `adk-tool` MCP client | Every external tool; Unreal over Streamable HTTP |
| `adk-skill` | Epic's Unreal skills (`unreal-mcp`, `create-toolset`) are agentskills-format `SKILL.md` files the crew loads as is |
| `adk-surface` | Gate decisions, progress feed, notifications to the owner |
| `adk-eval` | Rubric and LLM-judge scoring of scripts and cuts against §9 |
| `adk-artifact` | Versioned takes: audio stems, shots, cuts, thumbnails |
| `adk-runner` budgets | A spend cap per episode across models and credits |
| `adk-auth` signed audit | The production receipt |
| `adk-memory` | House style, past notes from the owner, what worked in earlier episodes |

## 8. The team

### 8.1 Crew

| Agent | Does | Tools | Output |
|-------|------|-------|--------|
| **Showrunner** (lead) | Plans the episode, owns the graph, budget and gates | Team, graph, surfaces | Episode plan |
| Researcher | Reads `CHANGELOG.md`, `changelog.d/`, specs, docs and merged PRs | devtools read/grep, `gh`, web fetch | Fact sheet; each fact cites a file or PR |
| Writer | Writes the two-host script in the existing format (`docs/podcast/pipeline/script_format.md`) | devtools | Script with chapters and cues anchored to phrases |
| Fact-checker | Checks every claim against its source; compiles every code snippet | devtools, `bash` (`cargo check`) | Claim table: pass or fail |
| Demo recorder | Runs the episode's demos for real and captures the terminal and screen | `bash`, computer use | Screen captures with command logs |
| Voice | Synthesises each exchange, caches by content hash, splits speaker stems | Gemini TTS (`adk-audio`), STT word timestamps | Narration, stems, timing manifest |
| Art director | Sources and cleans assets; records every licence | Fab (computer use), Tripo, Meshy, Umbo, Blender | Assets with licence sidecars |
| Character TD | Builds and maintains the James and Ada MetaHumans and accessories | unreal-mcp | Character assets |
| Set and lighting | Builds the level from the key art; lighting for both halves | unreal-mcp | The set level |
| Performance | Audio-driven face from stems; body idles, gestures, listening reactions, gaze to the speaker | unreal-mcp (MetaHuman Animator, animation retargeting) | Performance tracks per segment |
| Director and editor | Shot list from the timing manifest: two-shot, singles, pushes, reactions, demo cutaways; Sequencer tracks | unreal-mcp (Sequencer) | Edit decision list |
| Render wrangler | Queues Movie Render Pipeline jobs per shot; one render lane | unreal-mcp, kernel wake | Rendered shots |
| Post | Assembles, loudness-normalises, burns chapter cards, muxes, captions | media MCP (ffmpeg) | Master file, captions, chapters |
| Critic | Watches every cut and scores it against §9 | Gemini video input, `adk-eval` | Scored notes per shot; pass or send back |
| Publisher | Uploads as private, sets thumbnail, chapters, captions, disclosure; posts cross-links after approval | YouTube driver, social drivers | Private video, then public on approval |
| Gap scout | Turns any failure or intervention into a defect record with a reproduction | devtools, `gh` | Defect records (§10) |

### 8.2 The episode graph

```
research → script ⟂gate1 → fact-check → voice → stems ─┐
                                                       ├→ performance → edit → render → post → critic ⟂gate3 → publish ⟂gate4
assets → characters → set ⟂gate2 ─────────────────────┘                               ↑            │
demos (live captures) ────────────────────────────────────────────────────────────────┘            └→ back to the failing stage
```

- **Checkpoint per stage.** Every stage writes its takes to `adk-artifact` and checkpoints before the next stage.
- **Content-hash caching.** As in today's `pipeline/`, a changed line re-synthesises one exchange, and a changed cue
  re-renders and re-encodes one segment.
- **Send-backs.** The critic routes a failing shot to the stage that owns the fault (performance, edit or render),
  at most twice, then raises it to the showrunner.
- **Gates.** ⟂gate1 script, ⟂gate2 cast and set look, ⟂gate3 final cut, ⟂gate4 publish. Each is a graph interrupt
  shown as a decision card.

## 9. Quality bar

| Area | Requirement |
|------|-------------|
| Video | 1920×1080 minimum, 30 fps, H.264 High 4.1, 2 s keyframes, yuv420p, faststart (the v2 settings) |
| Audio | −14 LUFS integrated, true peak ≤ −1 dBTP, AAC 192 kbps 48 kHz |
| Lip sync | Audio leads video by no more than 45 ms and lags it by no more than 125 ms (ITU-R BT.1359 detectability) |
| Picture | No mesh interpenetration, popping, frozen faces, lighting flicker or dropped frames — scored per shot by the critic |
| Coverage | No shot shorter than 2.5 s except reactions; a cut on each speaker change unless the shot is a two-shot |
| Content | Every claim cited; every snippet compiles; every demo ran on the published release |
| Captions | Words match the script; timings within 100 ms of the audio |
| Accessibility | Captions on every video; on-screen code held at least 3 s per line |

## 10. The gap loop

1. **Detect.** A stage fails, the critic fails a shot twice for the same cause, or a human has to act. The gap scout
   writes a defect record: stage, tool, error, reproduction and a fingerprint, so a recurring gap stays one record.
2. **Park.** The graph is already checkpointed at the stage boundary; the run waits on the gap.
3. **Triage.** The coordinator classifies the gap:

   | Class | Fixed in | Verified by |
   |-------|----------|-------------|
   | ADK-Rust capability | `adk-rust`, issue + PR | A live run that fails on `main` and passes on the branch |
   | Tool (MCP server, Unreal toolset, Umbo) | That tool's repository | The stage re-run with the fixed tool |
   | Content or direction | The studio repo | The stage re-run |

4. **Resume.** After the fix merges, the run resumes from the checkpoint on the new revision.
5. **Tell the story.** Each fixed gap is a candidate short ("the bug the video found") and a line in the behind-the-scenes
   episode.

Until spec 3.4 lands, the coordinator and fix agents run this loop. Afterwards, the studio is a natural first user of
self-improvement.

## 11. Known gaps at grounding

From reading `main` @ `459ec9558`. Each is a hypothesis until a phase 0 or phase 2 run confirms it.

| # | Gap | Evidence | Likely home |
|---|-----|----------|-------------|
| K1 | **No provider-neutral deferred tool loading.** Unreal's MCP exposes hundreds of tools across 30+ toolsets; loading all into a Gemini or OpenAI request is not workable | Tool search exists only as Anthropic-native (`adk-anthropic/src/tool_search.rs`) | `adk-tool`: a deferred `McpToolset` catalog plus a search tool, for any model |
| K2 | **No YouTube driver.** Resumable upload, thumbnail, captions, chapters, privacy, analytics | `adk-drivers/src/social/` holds only `mastodon.rs` | `adk-drivers` |
| K3 | **No timeline or ffmpeg tool** | No ffmpeg use in any crate | An MCP server (the `adk-avtool` of `adk-rust-mcp-toolkit`), not a crate — keeps the framework neutral |
| K4 | **Image, video and music generation** | Lyria appears only in `adk-model/src/gemini/interactions_target.rs`; no Veo or image-output path found | MCP first (`media-mcp`); a model feature only if the owner wants it in the framework |
| K5 | **Video input for the critic** at episode length | Not confirmed that `adk-model` sends large video through the Gemini Files API | `adk-gemini` / `adk-model`, if confirmed |
| K6 | **Speaker stems from multi-speaker TTS** | Multi-speaker TTS returns one mixed track per exchange | Studio code using `adk-audio` STT word timestamps; an `adk-audio` helper if it generalises |
| K7 | **Self-improvement is not on `main`** | No `self-improve` feature in `adk-harness/Cargo.toml` | Spec 3.4; the coordinator runs the loop meanwhile |
| K8 | **Computer use on Fab and the Epic launcher** | The computer-use pack exists; Fab has no download API; unproven on these apps | `computer-use-mcp` or `adk-computer-use`, by what fails |
| K9 | **One local render lane** | Umbo and the local generators share `~/Library/Caches/zavora-mcp/local-gpu.lock`; renders, local models and `cargo` builds compete for 24 GB | Kernel resource classes, if the lock file is not enough |
| K10 | **Async Unreal tool results** | `GenerateAnimationFromAudio` and the identity tools return async result objects (`UMetaHumanPerformanceGenerationAsyncResult`); how the MCP server surfaces their completion is unconfirmed | `adk-tool` MCP client (parked calls, `adk-tool/src/mcp/parked.rs`) or a kernel wake, by what phase 0 shows |
| K11 | **MetaHuman Creator Core Data** | The `MetaHumanGenerator` toolset registers only when this optional content is installed; it installs through the Epic Games Launcher, which has no API | A one-time setup step, or a computer-use test case for K8 |

### 11.1 Claude Code's tools in ADK-Rust

The crew needs what a Claude Code session has. Most of it exists.

| Claude Code tool | ADK-Rust equivalent | State |
|------------------|---------------------|-------|
| Read (text, images, PDF) | `adk-devtools` `read_file`, images and PDF inline | Exists |
| Write, Edit, patch | `write_file`, `edit_file`, `apply_patch` | Exists |
| Glob, Grep | `glob`, `grep` | Exists |
| Bash, streamed output | `bash` with progress events | Exists |
| Background jobs, Monitor | Kernel wakes: `FileChange`, `Queue`, `Webhook`, `Cron` | Exists |
| WebFetch, WebSearch | Harness `web_fetch`, `web_search`; `GoogleSearchTool` | Exists |
| ToolSearch (deferred tools) | Anthropic-native only | **Gap K1** |
| Subagents, forks | `AgentTool`, `TeamSpec`, ACP and A2A members | Exists |
| Task list | Harness todo list and plan | Exists |
| AskUserQuestion, approvals | Surface decision cards, `ApprovalService` | Exists |
| Computer use (desktop) | Computer-use pack, `adk-computer-use`, `computer-use-mcp` | Exists; **K8** unproven on Fab |
| Browser | Browser pack (`adk-browser`), or Playwright MCP through `McpToolset` | Exists |
| Skills | `adk-skill` (agentskills.io) | Exists; Epic's skills to be confirmed in phase 0 |
| Memory | Harness facts, `adk-memory` | Exists |
| Schedules | Kernel `Cron` wake, server background cron | Exists |
| Artifacts, widgets | `adk-artifact`, `adk-surface` `genui` | Exists |

### 11.2 MCP servers

| Server | Used for | Transport | State on the studio machine (2026-11-15) |
|--------|----------|-----------|------------------------------------------|
| `unreal-mcp` (UE 5.9 `ModelContextProtocol` + `AllToolsets`) | Levels, MetaHumans, Sequencer, Movie Render Pipeline, import | Streamable HTTP, `127.0.0.1:8000/mcp` | Plugins ship in the engine; not yet enabled in a studio project |
| `umbo` (Umbo Studio 0.1.0) | Prop and accessory check, retopology, bake, rig, export | stdio | Built; not yet configured |
| Meshy | Image and text to 3D, rigging, motion | MCP | Connected; spends credits |
| Tripo | Image to 3D | API or Studio web | Not configured — see Q3 |
| `computer-use-mcp` | Fab, Epic launcher, anything without an API | stdio | In `mcp-servers/` |
| Media servers (`adk-speech`, `adk-avtool`, `adk-image`, `adk-music`, `adk-video`, or `media-mcp`) | ffmpeg assembly, fallback TTS, generated b-roll and music | stdio | `adk-rust-mcp-toolkit` made episode 2 |
| `mcp-imagegen`, `mcp-sfxgen`, `mcp-meshgen` | Local generation under the shared GPU lock | stdio | In `mcp-servers/` |
| `mcp-dam` | Asset catalogue with licences | stdio | In `mcp-servers/` |
| Playwright | Tripo Studio web, YouTube Studio checks | stdio | Configured for this project |

Credit-spending tools (Meshy, Tripo, generation APIs) run behind an approval that states the cost, inside the episode
budget.

## 12. Phases and scenarios

### Phase 0 — Spike: "Hello, Ada" (the riskiest path first)

An ADK-Rust agent, with no human action after the project opens:

1. connects to `unreal-mcp` and loads Epic's skills through `adk-skill`;
2. creates a level with one MetaHuman, a light and a camera;
3. synthesises one 10-second line in `Kore`;
4. generates the face animation from the audio;
5. renders the shot with Movie Render Pipeline, sleeping on a kernel wake until it lands;
6. muxes audio and video with ffmpeg.

Pass: a 10-second clip within the lip-sync bound of §9 and an empty intervention log. This phase is expected to confirm
or retire K1, K5, K8, K9, K10 and K11, and whether MetaHuman Animator's audio path runs on this Mac.

#### Phase 0 findings (2026-11-15)

UE 5.9 on the studio Mac ships an MCP toolset for every step of the spike, under
`Engine/Plugins/Experimental/Toolsets/`:

| Step | Toolset plugin | Entry points |
|------|----------------|--------------|
| MCP server | `ModelContextProtocol`, `ToolsetRegistry`, `AllToolsets` | Streamable HTTP at `127.0.0.1:8000/mcp`; `bAutoStartServer=True` starts it with the editor |
| Create the character | `MetaHumanGenerator` (Python toolset) | `create(asset_path)`, `begin_edit` / `end_edit`, `get_`/`set_body_shape`, `set_skin_tone`, `set_eye_color` |
| Face from audio | `MetaHumanAnimatorToolset` | `GenerateAnimationFromAudio(USoundWave, FMetaHumanPerformanceAudioParams, FMetaHumanPerformanceOutputLocation)` → async result with `PerformanceAssetPath`; `ExportPerformanceAnimationSequence` |
| Audio solve options | `FMetaHumanPerformanceAudioParams` | `OutputControls` (`FullFace`, `MouthOnly`), `Mood` (default `AutoDetect`), `MoodIntensity` 0–1, `DisableOptions` (`DownmixChannels`, `GenerateBlinks`, `HeadMovement`) |
| Sequencer | `SequencerAnimMixerToolset` | Animation mixing on Sequencer tracks |
| Render | `MovieRenderPipelineToolset` | Movie Render Queue and Movie Render Graph through Python, with its own agent skills |
| Look at frames | `MediaToolset` | Reads images and samples video files: a critic inside the editor |

`GenerateAnimationFromAudio` is documented as not retry-safe after success: the created Performance asset is reused,
not regenerated. The studio records the asset path in the stage's take so a resumed run never re-solves it.

The engine's MetaHuman plugins are installed (`MetaHumanCharacter`, `MetaHuman` (Animator), `MetaHumanCoreML`,
`MetaHumanSDK`). Whether MetaHuman Creator Core Data is installed is not yet checked (K11).

#### Phase 0 run plan

1. **Core Data.** Check `IsOptionalContentInstalled`. If absent, install MetaHuman Creator Core Data for UE 5.9 from
   the Epic Games Launcher — first as a computer-use attempt (K8), else as setup by the owner.
2. **Project.** Create `RBStudio` from `Templates/TP_BlankBP` (124 KB) at
   `~/Developer/projects/rust-and-beyond-studio/unreal/RBStudio`, a new project, so no existing `.uproject` is edited.
   Plugins: `ModelContextProtocol`, `AllToolsets`, `PythonScriptPlugin`, `MetaHumanCharacter`, `MetaHuman`,
   `MetaHumanGenerator`, `MetaHumanAnimatorToolset`, `MovieRenderPipeline`, `MovieRenderPipelineToolset`,
   `SequencerAnimMixerToolset`, `MediaToolset`. `Saved/Config/MacEditor/EditorPerProjectUserSettings.ini` sets
   `bAutoStartServer=True`.
3. **Catalogue.** Launch the editor in the background, wait for shader compilation, call `tools/list` on
   `127.0.0.1:8000/mcp`, save the catalogue to `spike/catalog.json` and count tools and schema bytes — the measure
   for K1.
4. **Agent.** A standalone crate `spike/hello_ada` with path dependencies on `adk-rust` until 3.0.0 is published:
   - `McpToolset` over Streamable HTTP, filtered to the MetaHuman, Sequencer, render, media and asset-import toolsets
     (the K1 workaround, recorded as an intervention until K1 lands);
   - `adk-skill` loading Epic's `unreal-mcp` skill files as they ship;
   - a `synthesize_line` tool on `adk-audio`'s Gemini TTS, voice `Kore`, writing a WAV;
   - `adk-devtools` `bash` in the spike workspace for ffmpeg;
   - one `LlmAgent` on `claude-sonnet-5-5` with the six steps as its instruction.
5. **Run.** The agent creates Ada, imports the WAV as a `SoundWave`, solves the face, builds a level sequence with
   Ada, the face animation, a key light and a camera, renders 1080p30 through Movie Render Queue, and muxes with ffmpeg.
6. **Check and record.** Measure lip sync against §9; every failure or human action becomes a defect record in
   `spike/gaps/` with the fields in §10.

**Build constraints on the studio Mac:** about 28 GiB free; builds use a shared target with debug info off; no `cargo`
build runs while the editor compiles shaders or renders (24 GB of memory).

### Phase 1 — Cast and set

The James and Ada MetaHumans and accessories, and the set, built from the key art. The look is approved through the
concept sign-off flow: turntables and set stills on a sign-off page, one card per asset with Approve or Needs changes.

### Phase 2 — Pilot

A 2–3 minute segment remade from an episode 3 exchange, so v2 and v3 can be compared side by side. This runs the whole
graph end to end for the first time.

### Phase 3 — Season

Teaser, episode 4, the series and shorts, on the calendar in §4.2.

### Phase 4 — Behind the scenes

The documentary from the receipts, the gap records and the PRs.

### Scenarios

| # | Scenario | Pass condition |
|---|----------|----------------|
| S1 | Phase 0 spike | §12 phase 0 pass |
| S2 | Edit one line of a finished episode | One exchange re-synthesised; only the affected shots re-rendered; new master within the quality bar |
| S3 | Critic catches a planted fault (a frozen face for 1 s) | The shot is sent back to performance and passes on re-render |
| S4 | A gap mid-episode | Run parks; defect record filed; resumes from the checkpoint after the fix with no stage repeated |
| S5 | Kill the studio process during a render | The kernel recovers the run; the render is not queued twice |
| S6 | Budget cap reached | The run parks for approval rather than overspending |
| S7 | Fact-check failure | A claim with no source blocks gate 1 |
| S8 | Publish | Upload is private until gate 4; the receipt is linked in the description |
| S9 | Asset-free scenario in CI | `examples/showrunner` runs the graph against scripted MCP servers |

## 13. Prerequisites and risks

| Item | Detail | Mitigation |
|------|--------|------------|
| **Disk** | 31 GiB free of 460 GiB on the studio Mac. Two cinematic MetaHumans, a set, Fab assets, the derived data cache and per-shot renders exceed that | An external SSD of 1 TB or more for the Unreal project, its derived data cache and renders — **needed before phase 1** |
| Memory | M4 with 24 GB runs Unreal with two MetaHumans, the agents and local models together | One render lane; no `cargo` builds or local models while rendering |
| Render time | Lumen at 1080p on an M4 is slow for 30 minutes of footage | Render only changed shots (content hash); a cloud GPU later if needed |
| MetaHuman Animator on macOS | Audio-driven animation on Apple Silicon is unproven here | Phase 0 answers this first; route B is the fallback |
| Fab licences | Source assets cannot be redistributed; some listings are tagged NoAI | Assets stay in the private repo; the art director records each licence and skips NoAI-tagged listings unless the owner approves |
| YouTube policy | Synthetic-content disclosure; monetisation rules on mass-produced content | Disclose on every upload; each episode is original teaching with live demos |
| Spend | Model calls, TTS, Meshy and Tripo credits | A per-episode budget in the runner; approval with the stated cost per credit spend |

## 14. Open questions

| # | Question | Default |
|---|----------|---------|
| Q1 | Character route: MetaHuman (A) or stylised meshes from the key art (B)? | A |
| Q2 | Where does the studio live? | Private `zavora-ai/rust-and-beyond-studio`; asset-free scenario in `adk-rust/examples/showrunner` |
| Q3 | Tripo access: an API key, or only the Tripo Studio web app? | Web app through the browser pack; API through MCP if a key exists |
| Q4 | Is an external SSD available for the Unreal project? | Required before phase 1 |
| Q5 | Keep the v2 voices `Fenrir` and `Kore`? | Yes |
| Q6 | Output resolution | 1080p30; 4K after the pilot if render time allows |
| Q7 | Season scope | Teaser, episode 4, eight series episodes, shorts, behind the scenes |
| Q8 | Publish gate | Uploads are private; the owner makes them public at gate 4 |
| Q9 | Spend cap per episode | To be set by the owner after phase 0 measures real costs |
| Q10 | Does the campaign wait for 3.0.0 on crates.io? | Yes: demos use the published crates |
| Q11 | Models per role | Writer and showrunner on `claude-sonnet-5-5` or `gemini-3.1-pro-preview`; critic on Gemini for video input; TTS `gemini-3.1-flash-tts-preview` — mixing providers shows framework neutrality |
| Q12 | Product name | "Rust & Beyond Studio"; the scenario crate `showrunner` |
