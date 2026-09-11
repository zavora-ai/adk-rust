# BI Analyst — an agent that works a dashboard the way a person does

Opens the dashboards a business already has, reads the numbers behind each tile,
notices what is odd, drills into it, and explains what it found — reporting every
step into a console you can watch and interrupt.

Runs with **no BI credentials**: [`mcp-bi`](https://github.com/zavora-ai/mcp-bi)
defaults to a seeded fixture with three saved dashboards over one trading dataset.

## Three servers, three jobs

| Server | Transport | Why it is here |
|---|---|---|
| `mcp-bi` | stdio | The dashboards: list, open, chart data, drill down, render |
| `mcp-market-data` | stdio | Quotes and history, to check a dashboard against the market. Optional |
| `computer-use-mcp` | HTTP | The run console, and a browser for dashboards with no data API |

Two transports on one agent: `McpHttpClientBuilder` for the console,
`McpServerManager` for the child processes, and `LlmAgentBuilder::toolset`
accumulates both.

## The rule that makes it trustworthy

An agent handed a dashboard image will describe a trend it never measured. So every
visual is read twice:

- `bi_insights` computes direction, change, min and max **with the labels they
  occurred at**, mean, deviation, outliers and gaps
- `bi_render_chart` draws the same numbers and returns the picture with those
  statistics attached

The brief requires the agent to cite a statistic. When it opens a dashboard in a
browser — the only way to read a platform with no data API — what it sees on screen
corroborates a queried number rather than being the source of one.

## Drill-down without guessing

`bi_get_dashboard` reports the dimensions each chart can be broken down by, and
`bi_drill_down` reports rows before and after. That pair is what lets the agent pick
a drill path and know whether it narrowed anything — instead of inventing column
names and presenting a step that did nothing as a finding.

## Run it

```bash
# 1. build the BI server
cargo build --release --manifest-path ../mcp-servers/mcp-bi/Cargo.toml

# 2. the console, so there is something to watch
npx -y -p @zavora-ai/computer-use-mcp computer-use-mcp-console --no-demo

# 3. this agent
export DEEPSEEK_API_KEY=sk-...
export MCP_BI_BIN=../mcp-servers/mcp-bi/target/release/mcp-bi
cargo run --manifest-path examples/bi_analyst/Cargo.toml
```

Open <http://127.0.0.1:4517/> and ask something like *"Walk the dashboards and tell
me what needs attention."* The agent waits for that first message.

| Variable | Meaning |
|---|---|
| `DEEPSEEK_API_KEY` | Required |
| `MCP_BI_BIN` | The `mcp-bi` binary |
| `CONSOLE_URL` | Console host. Default `http://127.0.0.1:4517` |
| `MCP_MARKET_DATA_BIN` | Optional. Adds a market cross-check |
| `BI_BACKEND` | Passed to `mcp-bi`: `superset`, `metabase`, `powerbi`, `tableau`, `looker`, `qlik`, `quicksight` |

Against a real platform:

```bash
BI_BACKEND=superset SUPERSET_URL=http://localhost:8088 SUPERSET_TOKEN=... \
  cargo run --manifest-path examples/bi_analyst/Cargo.toml
```

## Tool surface

18 tools: 5 run-console tools and 13 desktop ones from `computer-use-mcp`, plus
whatever `mcp-bi` and `mcp-market-data` expose. No `run_script`, no `filesystem`, no
`process_kill` — this agent reads dashboards.

Nothing writes to a dashboard. `mcp-bi` declares `writes_allowed = "none"`, because
a dashboard is changed through the platform's own review, not by an agent.

## Degrades rather than fails

No market-data binary, no BI credentials, no Blender-style prerequisites: the agent
reports what is missing and works with what is there. Only the BI server is
essential, and its absence says how to build it.

## Not yet verified

The agent has been verified to start, connect all three servers, filter to 18 tools
and join a run — but **no live model run has exercised the analysis loop**, because
the DeepSeek balance funding this work was exhausted.

The BI server behind it is better covered: 27 unit and fixture tests, plus 27 checks
against a **live Apache Superset** loaded with its own example dashboards — 9
dashboards, 21 datasets, 1,000 rows of real chart data, SQL through SQL Lab, and a
31 KB PNG rendered from those rows. So the tools this agent calls are proven; the
agent's judgement in calling them is not.
