# Documentation Test Examples

This folder contains working code examples that validate the official documentation.

## Structure

The folder structure mirrors the official docs:

```
doc-test/
├── README.md                    ← You are here
├── quickstart_test/             # Getting started examples
├── agents/                      # Agent documentation tests
│   ├── llm_agent_test/          # LLM Agent examples
│   ├── multi_agent_test/        # Multi-agent systems
│   ├── workflow_test/           # Workflow agents (sequential, parallel, loop)
│   ├── graph_agent_test/        # Graph-based workflows
│   └── realtime_agent_test/     # Realtime voice agents
└── models/                      # Model provider tests
    └── providers_test/          # All LLM providers
```

## Running Examples

Each subfolder is a standalone Cargo project. Navigate to the folder and run:

```bash
cd doc-test/agents/llm_agent_test
cargo run --bin basic_agent
```

## API Keys

Most examples require API keys. Set them as environment variables:

```bash
export GOOGLE_API_KEY="your-key"      # Gemini (default)
export OPENAI_API_KEY="your-key"      # OpenAI
export ANTHROPIC_API_KEY="your-key"   # Anthropic
export DEEPSEEK_API_KEY="your-key"    # DeepSeek
export GROQ_API_KEY="your-key"        # Groq
```

Ollama examples don't need API keys but require the local server:
```bash
ollama serve
ollama pull llama3.2
```

## Example Index

### Agents

| Folder | Examples | Documentation |
|--------|----------|---------------|
| `llm_agent_test` | basic_agent, shaped_behavior, instruction_templating, multi_tools, structured_output, complete_example | [llm-agent.md](../docs/official_docs/agents/llm-agent.md) |
| `multi_agent_test` | customer_service, hierarchical | [multi-agent.md](../docs/official_docs/agents/multi-agent.md) |
| `workflow_test` | sequential_pipeline, parallel_analysis, loop_refiner, conditional_router | [workflow-agents.md](../docs/official_docs/agents/workflow-agents.md) |
| `graph_agent_test` | parallel_processing, conditional_routing, react_pattern, supervisor_routing, human_in_loop, checkpointing | [graph-agents.md](../docs/official_docs/agents/graph-agents.md) |
| `realtime_agent_test` | basic_realtime, realtime_with_tools, realtime_vad, realtime_handoff | [realtime-agents.md](../docs/official_docs/agents/realtime-agents.md) |

### Models

| Folder | Examples | Documentation |
|--------|----------|---------------|
| `providers_test` | gemini_example, openai_example, anthropic_example, deepseek_example, groq_example, ollama_example | [providers.md](../docs/official_docs/models/providers.md) |

## Contributing

When updating documentation:
1. Update the doc-test example to match
2. Run `cargo build` to verify compilation
3. Test the example manually if possible
