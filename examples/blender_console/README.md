# Blender Console — an agent a person can watch, and interrupt

A live, multi-turn agent that reports what it is doing into a browser console while
it works on your desktop. You type a request, it declares a plan, works, ticks the
steps off, and posts real screen captures as it goes. Type again mid-task and it
adapts.

![The console: conversation on the left, plan and a live Blender frame on the right](https://raw.githubusercontent.com/zavora-ai/computer-use-mcp/main/docs/assets/blender-console-live.png)

The frame above is a real capture of Blender, taken through the MCP session. The
purple cube came from the first request in that conversation and the yellow torus
from the second.

## What it demonstrates

- **Two MCP servers, two transports, one agent.** The console host is reached over
  Streamable HTTP (`McpHttpClientBuilder`); Blender's own server is a child process
  over stdio (`McpServerManager`). `LlmAgentBuilder::toolset` accumulates, so both
  attach to one agent.
- **Multi-turn with no queue.** One run is one conversation. The agent decides there
  is work to do by a single rule: the last transcript message has `role: "user"`.
  Answering appends an `agent` turn, which clears it. Because every run tool reply
  carries the transcript, a message typed mid-task reaches the agent on its next
  call.
- **Session memory across turns.** Every turn runs against the same
  `adk_session` id, so the third request still knows what the first one built.
- **A narrow tool surface.** The HTTP toolset is filtered to 17 tools — the 4 run
  tools and 13 desktop ones. No `run_script`, no `filesystem`, no `process_kill`.
- **Turn isolation.** A model error fails that turn, reports it into the console and
  keeps the agent alive for the next message, instead of killing the process.

## Run it

You need Node (for the console host) and a DeepSeek key. Blender is optional.

```bash
# 1. the console host. Serves the UI on / and an MCP endpoint on /mcp.
npx -y -p @zavora-ai/computer-use-mcp computer-use-mcp-console --no-demo

# 2. the agent, in another shell
export DEEPSEEK_API_KEY=sk-...
cargo run --manifest-path examples/blender_console/Cargo.toml
```

Open <http://127.0.0.1:4517/> and type something, for example *"Turn the cube
bright purple and show me the viewport."* The agent is waiting for that first
message; the host opens the run from it.

`--no-demo` matters. Without it the host drives its own scripted walkthrough, which
this agent would then try to answer.

### With Blender

Point `BLENDER_MCP_BIN` at the **official** Blender MCP server from
projects.blender.org, then start Blender and enable its MCP server. Note that
`uvx blender-mcp` installs a different, community package of the same name, so the
official server has to be invoked by path.

```bash
export BLENDER_MCP_BIN=/path/to/official/blender-mcp
# optional: load the Blender routing policy from a computer-use-mcp checkout
export COMPUTER_USE_SKILLS=/path/to/computer-use-mcp
cargo run --manifest-path examples/blender_console/Cargo.toml
```

Without `BLENDER_MCP_BIN` the agent still runs with desktop tools only, which is
enough to watch the console work.

| Variable | Meaning |
|---|---|
| `DEEPSEEK_API_KEY` | Required. |
| `CONSOLE_URL` | Console host base URL. Default `http://127.0.0.1:4517`. |
| `BLENDER_MCP_BIN` | Official `blender-mcp` binary. Omit for desktop tools only. |
| `COMPUTER_USE_SKILLS` | computer-use-mcp checkout, for the Blender routing policy. |

## How it fits together

```text
  browser ──┐
            │  console host: one run store, one desktop session
  agent  ───┘        │
        /mcp         └── the run: transcript, plan, task states, last frame
```

The host owns the run. It has to, because MCP's HTTP handler builds a server per
request — so a run recorded by one request would be invisible to the next unless
one store is injected across all of them. This agent connects to `/mcp` and gets
the desktop tools *and* the run tools from that same server, which is why a plan it
declares and a frame it captures appear in the browser without anything being
mirrored between two places.

## Cost

Thinking is left uncapped (`ReasoningEffort::Max`). Reasoning is billed against
`max_tokens`, and a dense screenshot can need thousands of tokens before the first
word of the answer — one Blender window took 6,959 — so capping it truncates the
answer and still bills in full. A short two-turn conversation like the one pictured
is a handful of model calls per turn.

## Security

The console host is loopback-only and unauthenticated. Its `/mcp` endpoint grants
desktop control to anything that can reach the port: fine on your own machine,
never on a shared or exposed host. The browser page itself is held to a two-tool
allowlist and cannot report progress or speak as the agent.
