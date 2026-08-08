---
marp: true
theme: uncover
class: invert
paginate: true
backgroundColor: #0A1628
color: #F1F5F9
style: |
  /* Palette lifted from the adk-rust.com design tokens so the episode and the
     site read as one brand:
       --rust-orange #F74C00   --electric-blue #3B82F6   --purple #A855F7
       --emerald     #10B981   --amber          #F59E0B   --navy   #0A1628
       code surface  #0D1520   card border      #1E3050   muted    #94A3B8 */
  section {
    font-family: 'Inter', 'SF Pro Display', -apple-system, sans-serif;
    background: #0A1628;
  }
  h1 {
    color: #F74C00;
    font-size: 2.6em;
    font-weight: 800;
    letter-spacing: -0.02em;
  }
  h2 {
    color: #79B2FF;
    font-size: 1.6em;
    font-weight: 600;
  }
  h3 {
    color: #F1F5F9;
    font-size: 1.05em;
  }
  strong { color: #F74C00; }
  em { color: #94A3B8; }
  code {
    background: #0D1520;
    color: #61E7A5;
    padding: 2px 8px;
    border-radius: 4px;
    font-family: 'JetBrains Mono', ui-monospace, monospace;
  }
  pre {
    background: #0D1520;
    border: 1px solid #1E3050;
    border-radius: 12px;
  }
  table { font-size: 0.66em; border-collapse: collapse; }
  th {
    background: #111D32;
    color: #79B2FF;
    border: none;
    font-weight: 600;
  }
  td { border-color: #1E3050; }
  blockquote {
    border-left: 4px solid #F74C00;
    padding-left: 1em;
    font-style: normal;
    color: #94A3B8;
  }
  /* Marp strips raw HTML, so layout comes from markdown driven by CSS.
     `dense` top-aligns a content-heavy slide and scales it down — the uncover
     theme centres a large h1 and will otherwise push rows off a 16:9 frame. */
  section.dense {
    justify-content: flex-start;
    padding-top: 0.5em;
  }
  section.dense h1 { font-size: 1.45em; margin-bottom: 0.15em; }
  section.dense h2 { font-size: 0.95em; margin-top: 0; }
  section.dense h3 { font-size: 0.8em; }
  section.dense ul, section.dense p { font-size: 0.78em; }
  section.dense pre { font-size: 0.6em; }
  section.dense table { font-size: 0.5em; }
  /* Big-number strip: the header row carries the metric, the body row its label. */
  section.stats th {
    background: transparent;
    color: #F74C00;
    font-size: 2.5em;
    font-weight: 800;
    border: none;
  }
  section.stats td { font-size: 1.05em; color: #94A3B8; border: none; }
  section.stats table { font-size: 1em; border: none; }
  /* Two-column comparison. */
  section.split table { font-size: 0.6em; }
  section.split th { font-size: 1.3em; }
  /* Screens: heading plus capture, no caption — a full-bleed screenshot reads
     better on video than a smaller one with a subtitle the narrator is already
     saying out loud. */
  section.shot { justify-content: flex-start; padding-top: 0.4em; }
  section.shot h1 { font-size: 1.25em; margin-bottom: 0.25em; }
  /* The closing sequence runs hotter. */
  section.close h1 { font-size: 2.9em; }
  section.close h2 { color: #F1F5F9; font-size: 1.25em; }
  section.cta h1 { font-size: 2.2em; }
  section.cta h2 { color: #61E7A5; font-size: 1.9em; font-weight: 700; }
---

# ADK-Rust v2.0.0

## Agents That Act

🎧 *Rust & Beyond Podcast — Episode 3*

*Hosts: James & Ada*

---

<!-- _class: invert dense -->

# Three Ways an Agent Acts

| Autonomously | Continuously | Proactively |
|---|---|---|
| Give it a goal **and a way to check it** | Close the laptop. **Come back. Same step.** | **Nobody has to ask** |
| It plans, acts, and verifies until the check passes | Sessions, memory, and atomic checkpoints carry the work | A schedule, a file change, or a webhook starts it |

*Everything else in v2 serves one of those three.*

---

<!-- _class: invert dense -->

# Give It a Goal and a Way to Check It

```bash
adk-rust goal "make the test suite pass" --until "cargo test"
```

- **`--until` is the important half** — you hand over a command that proves the outcome
- It stops when *your* check passes, not when the model feels finished
- `code` for one task · `ultracode` for parallel reviewers

## `adk-devtools` — six tools, one sandbox

`read_file` · `write_file` · `edit_file` · `glob` · `grep` · `bash`

- An edit only works on a file the agent has **read**
- An edit only works on a match confirmed **unique**

---

<!-- _class: invert shot -->

# Leave With the Crate

![w:1080](ep3-assets/multiagent-seven-specialists.png)

---

<!-- _class: invert dense -->

# Close the Laptop. Come Back.

```
save before  ──▶  resolve the call  ──▶  save after  ──▶  resume
```

- `goal` checkpoints atomically — interrupt it, `--resume`, continue from the same step
- **CodeAct pauses the interpreter** when a program calls a tool; the host resolves it and the script resumes
- A paused program becomes **durable agent state**
- Managed state reports its own durability, so operators know what survives a restart
- Bi-temporal knowledge-graph memory · project-scoped isolation · session fork and replay

---

<!-- _class: invert dense -->

# Nobody Has to Ask

## `AmbientAgent` — three trigger sources

| `CronTrigger` | `FileWatchTrigger` | `WebhookTrigger` |
|---|---|---|
| On a schedule | When a directory changes | When an external event arrives |
| | | Loopback by default · verifier required to listen wider |

- Every trigger event carries its **verified principal**
- Bounded concurrent dispatch

*Amos runs **11 scheduled routines** against a live ERP. Nightly. Unprompted.*

---

<!-- _class: invert shot -->

# One Sentence In. A Posted Journal Out.

<!-- ASSEMBLY: replace this still with ep3-assets/amos-one-cycle.mp4 — an 18s loop
     that cycles all four Amos scenes (bill · receipt · M-Pesa reconcile · plain-English
     answer). The still is only a fallback for the PNG export. -->

![w:620](ep3-assets/amos-bill-journal.png)

---

<!-- _class: invert shot -->

# Watch It Work

![w:1080](ep3-assets/simple-agent-trace.png)

---

<!-- _class: invert shot -->

# Let the Model Write the Workflow

![w:1080](ep3-assets/codeact-boundary-diagram.png)

---

<!-- _class: invert dense -->

# Everything in v2

## Realtime
Multimodal **video input** · affective dialogue · typed raw audio · GA providers with tool dispatch

## Protocols
MCP on official **`rmcp 3.1`** (`2025-11-25`) · dynamic local servers · reconnect-safe notifications ·
complete **ACP v1** with exact capability publication · tool approval resolved **mid-run**

## Governed computer use
Digest-bound approvals · single executor for mutation · verification reports what was observed

## Graph
**Fan-in in the core builder** — deferred nodes run once, after every upstream path completes

---

<!-- _class: invert dense -->

# Built to Be Trusted

| Boundary | Now |
|---|---|
| `adk-sandbox` | Output capture is memory-bounded **as it reads** |
| `adk-devtools` | Workspace paths resolve inside the workspace, symlinks included |
| `adk-devtools` | `bash` runs with a **scoped environment** |
| `adk-server` | A2A and UI routes sit behind configured authentication |
| `adk-auth` | Secret access is authorizable and audited, behind a bounded, revocable cache |

**25 security improvements · 57 fixes · 27 documented API changes, each with a migration path**

---

<!-- _class: invert dense -->

# Four Products, One Foundation

| Product | What it does |
|---|---|
| **ZSpreadsheet** | Describe the workbook. Get a real file you can open, edit, version, and download. |
| **Amos AI** | Talk to the books. Plans the work, uses the live ERP, asks before posting, returns evidence. |
| **JobHunter** | Two audiences, two agent teams, their own state and workspaces. |
| **ADK Gateway** | One Rust binary connecting agents to Telegram, Slack, WhatsApp, Discord, Matrix. |

*Same runtime underneath all four.*

---

<!-- _class: invert dense -->

# Every Change, Written Down

- **27 public API changes**, each with its downstream impact in a table
- A migration guide with before-and-after code
- Mostly three things:

| New config fields | New enum variants | Safer defaults |
|---|---|---|
| `..Default::default()` | Add match arms | Opt back in deliberately |

*Read the four or five entries that touch what you actually use.*

---

# Start Free

```bash
cargo add adk-rust

cargo install cargo-adk
cargo adk new my-agent    # 12 templates, 9 add-ons
cargo run
```

**Free forever** — 3 agents, 1 environment.

*Or run agents in the browser: playground.adk-rust.com*

---

<!-- _class: invert close -->

# It works.

## So you build a second one.

## Then a fifth.

## Then somebody asks a question you cannot answer from a log file.

---

<!-- _class: invert close -->

# *Who approved that refund?*

## Which agent read that customer record?

## Where is the evidence?

---

<!-- _class: invert shot -->

# Now Run a Hundred of Them

![w:1080](ep3-assets/enterprise-agents_dashboard.png)

---

<!-- _class: invert shot -->

# Agents Never Hold a Raw Secret

![w:1080](ep3-assets/enterprise-credentials_vault.png)

---

<!-- _class: invert shot -->

# Blocked. Not Warned About.

![w:1080](ep3-assets/enterprise-governance.png)

---

<!-- _class: invert close -->

# 318 Templates.

# 24 Industries.

## Banking · healthcare · insurance · legal · manufacturing · retail

*Day one is a running KYC agent, not a blank project.*

---

<!-- _class: invert shot -->

# Regulated Depth, Out of the Box

![w:1080](ep3-assets/enterprise-banking_agent.png)

---

<!-- _class: invert shot -->

# The Agent Prepares. A Person Decides.

![w:1080](ep3-assets/enterprise-healthcare_agent.png)

---

<!-- _class: invert close -->

# You Own It

## Perpetual licence · full source in your repository

## Your VPC, your cluster, your data centre

## Unlimited agents, environments, credits

---

<!-- _class: invert close -->

# $8,999

## Perpetual. Unlimited. Full source.

*12 months of updates and support · SSO · SCIM · private cloud · on-prem · 24/7 SLA*

---

<!-- _class: invert cta -->

# Bring One Workload

One agent you already run.
We map its runtime, tools, credentials, and approvals with you — then you decide.

## enterprise.adk-rust.com

⭐ github.com/zavora-ai/adk-rust · 🎮 playground.adk-rust.com

*Go build something that acts.*
