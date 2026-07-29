---
marp: true
theme: uncover
class: invert
paginate: true
backgroundColor: #0a1628
color: #f0f4f8
style: |
  section {
    font-family: 'Inter', 'SF Pro Display', -apple-system, sans-serif;
  }
  h1 {
    color: #ff6b35;
    font-size: 2.8em;
    font-weight: 700;
  }
  h2 {
    color: #4ecdc4;
    font-size: 1.8em;
  }
  strong {
    color: #ff6b35;
  }
  code {
    background: #1a2744;
    color: #4ecdc4;
    padding: 2px 8px;
    border-radius: 4px;
  }
  .metric {
    font-size: 3em;
    font-weight: 800;
    color: #ff6b35;
  }
  table {
    font-size: 0.7em;
  }
  th {
    background: #1a2744;
    color: #4ecdc4;
  }
  blockquote {
    border-left: 4px solid #ff6b35;
    padding-left: 1em;
    font-style: italic;
    color: #a0b4c8;
  }
---

# ADK-Rust v1.0.0

## The Stable Foundation

🎧 *Rust & Beyond Podcast — Episode 2*

*Hosts: James & Ada*

---

# The Numbers

<div style="display: grid; grid-template-columns: 1fr 1fr; gap: 2em; text-align: center; margin-top: 1em;">

<div>
<span style="font-size: 3.5em; font-weight: 800; color: #ff6b35;">130K+</span>

Downloads in 6 months
</div>

<div>
<span style="font-size: 3.5em; font-weight: 800; color: #4ecdc4;">39</span>

Published crates
</div>

<div>
<span style="font-size: 3.5em; font-weight: 800; color: #ff6b35;">60+</span>

Example agents
</div>

<div>
<span style="font-size: 3.5em; font-weight: 800; color: #4ecdc4;">4.6×</span>

Faster than Python ADK
</div>

</div>

---

# What Stable Means

## The Semver Contract

- Pin to `1.x` → **your build won't break**
- API changes go through **deprecation** first
- Migration guides ship with every major bump
- Every public type and trait method **audited and locked**

<br>

| Tier | Commitment |
|------|-----------|
| **Stable** | Semver guaranteed, no breaking changes in 1.x |
| **Beta** | API may evolve with deprecation notices |
| **Experimental** | Subject to change between minor versions |

---

# Architecture

## Composable Technology Layers

```
┌─────────────────────────────────────────────┐
│  Your Agent Application                      │
├─────────────────────────────────────────────┤
│  adk-agent  │  adk-graph  │  adk-managed    │
├─────────────────────────────────────────────┤
│  adk-model (7 providers)  │  adk-tool (MCP) │
├─────────────────────────────────────────────┤
│  adk-runner  │  adk-session (6 backends)    │
├─────────────────────────────────────────────┤
│  adk-core (traits: Agent, Tool, Llm, Event) │
└─────────────────────────────────────────────┘
```

**Tiered features:** `minimal` → `standard` → `enterprise` → `full`

*Never pay for what you don't use.*

---

# What's New in v1.0.0

## Features

- **Gemini Interactions API** — stateful server-side conversation history
- **Managed Agent Runtime** — durable execution with checkpointing
- **adk-bench** — cross-framework benchmarking (4.6× faster cold start)

## Hardening

- **Lock poison recovery** — one panicked task won't crash your service
- **Security audit** — every advisory documented, `cargo audit` CI-integrated
- **MSRV enforcement** — `rust-toolchain.toml` pins Rust 1.95

---

# Contributors

## The People Behind v1.0.0

| Contributor | Contribution |
|------------|-------------|
| **@mikefaille** | AdkIdentity, realtime audio, LiveKit, skill system |
| **@rohan-panickar** | OpenAI-compatible providers, xAI, multimodal |
| **@dhruv-pant** | Gemini service account authentication |
| **@tomtom215** | A2A Protocol v1.0.0 types crate |
| **@danielsan** | Google deps fixes, RAG crash report |
| **@CodingFlow** | Gemini 3 thinking, global endpoint, citations |
| **@ctylx** | Skill discovery fix |
| **@poborin** | Project config proposal |
| **@chillin-capybara** | ACP integration, adk-acp crate |
| **@baotao2006** | UTF-8 boundary audit, CJK fixes |

---

# Contributors (continued)

> *"This release would not exist without the people who showed up, wrote code, filed issues, and pushed us to be better."*

<br>

### And dozens more in:
- GitHub Discussions
- Issue reports & pre-release testing  
- Documentation improvements
- Playground examples

<br>

**We see you. Thank you.** 🙏

---

# The Vision

## Building the Future of Autonomous Software

> The next generation of software will be built by **composing autonomous agents**, not by writing every line of logic by hand.

<br>

### Why Rust?

- **Safe** — no data races, no null pointers
- **Fast** — 50ms cold start, zero-cost abstractions
- **Concurrent** — async-native, designed for multi-agent systems
- **Expressive** — traits model agent behaviors naturally

---

# The Vision (continued)

## From Framework to Platform

<div style="display: grid; grid-template-columns: 1fr 1fr; gap: 2em; margin-top: 1em;">

<div>

### Today (v1.0.0)
- 39 composable crates
- 7 model providers
- 6 session backends
- A2A protocol
- Graph workflows
- Real-time voice

</div>

<div>

### Tomorrow
- Playground → **Marketplace**
- Managed Runtime → **Hosting Platform**
- Protocol Layer → **Cross-org Standard**
- Spatial agents (3D/AR)
- Multi-modal native
- Trivial cloud deploy

</div>

</div>

---

# The Vision (continued)

## The Contract

<br>

> Build on a stable foundation, and the **platform grows underneath you**.

<br>

- ✅ Open source — always
- ✅ On crates.io — always
- ✅ Composable — always
- ✅ Semver stable — your code works in two years
- ✅ New capabilities — without rewriting

---

# What's Next — 2026 Roadmap

## Highlights

- 🌐 **Spatial Agents** — reason about physical 3D/AR space
- 🔌 **Deeper MCP** — wire up any tool server
- 💳 **AP2 Payments** — multi-party agentic commerce
- ⚡ **Performance** — always faster, always leaner
- 🛠️ **Developer Experience** — always easier

<br>

📖 Roadmap is public on GitHub — **read it, comment, shape it.**

---

# Get Started

## 30 Seconds to Your First Agent

```bash
# Add to existing project
cargo add adk-rust

# Or scaffold a new one
cargo install cargo-adk
cargo adk new my-agent
cargo run
```

<br>

### Or try the Playground first:
**https://playground.adk-rust.com**

*No install. No API keys. No sign-up. Just agents running.*

---

# ADK-Rust v1.0.0

## 39 crates · 130K downloads · Production ready · Semver stable

<br>

⭐ **GitHub:** github.com/zavora-ai/adk-rust
🎮 **Playground:** playground.adk-rust.com
📚 **Docs:** docs.rs/adk-rust
📦 **crates.io:** crates.io/crates/adk-rust
💬 **Discussions:** github.com/zavora-ai/adk-rust/discussions

<br>

*Thank you for watching. Go build something extraordinary.*
