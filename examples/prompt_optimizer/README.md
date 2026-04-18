# Prompt Optimizer Example

Demonstrates the **prompt optimization** workflow from `adk-eval` — iteratively
improving an agent's system instructions using an optimizer LLM and an
evaluation set.

## What This Shows

- Defining an initial agent with intentionally suboptimal instructions
- Loading an evaluation set of test cases from JSON files
- Configuring a `PromptOptimizer` with a separate optimizer LLM, evaluator,
  and optimization parameters
- Running the optimization loop with iteration-by-iteration progress
- Early stopping when the target threshold is met
- Writing the best-performing instructions to a file

## How It Works

The prompt optimizer runs an iterative loop:

1. **Evaluate** the agent against the eval set to get a baseline score
2. **Propose** improved instructions via the optimizer LLM
3. **Apply** the best improvement and re-evaluate
4. **Repeat** until `max_iterations` or `target_threshold` is reached
5. **Output** the best instructions to `optimized_instructions.txt`

```
┌─────────────┐     ┌──────────────┐     ┌─────────────┐
│  Eval Set    │────▶│  Evaluator   │────▶│   Score     │
│  (3 cases)   │     │              │     │             │
└─────────────┘     └──────────────┘     └──────┬──────┘
                                                │
                    ┌──────────────┐             │
                    │  Optimizer   │◀────────────┘
                    │  LLM         │
                    └──────┬───────┘
                           │
                    ┌──────▼───────┐
                    │  Improved    │
                    │  Instructions│
                    └──────────────┘
```

## Eval Set Format

Each test case is a JSON file in the `eval_set/` directory:

```json
{
  "input": "What is the difference between a Vec and a slice in Rust?",
  "expected": "The response should clearly explain that Vec is an owned, heap-allocated, growable collection while a slice is a borrowed view...",
  "tags": ["rust", "data-structures", "beginner"]
}
```

### Fields

| Field | Description |
|-------|-------------|
| `input` | The user prompt sent to the agent |
| `expected` | Description of what a good response should contain |
| `tags` | Optional category tags for filtering and organization |

## Included Test Cases

| File | Topic | Tags |
|------|-------|------|
| `test_case_1.json` | Vec vs slice | rust, data-structures, beginner |
| `test_case_2.json` | Error handling without unwrap | rust, error-handling, intermediate |
| `test_case_3.json` | Borrow checker explanation | rust, ownership, core-concepts |

## Optimizer Configuration

| Parameter | Value | Description |
|-----------|-------|-------------|
| `max_iterations` | 3 | Maximum optimization rounds |
| `target_threshold` | 0.8 | Stop early if this score is reached |
| `output_path` | `optimized_instructions.txt` | Where to write the best instructions |

## Prerequisites

- `GOOGLE_API_KEY` environment variable set (for the Gemini LLM provider)

## Setup

```bash
cp .env.example .env
# Edit .env and add your Google API key
```

## Run

```bash
cargo run --manifest-path examples/prompt_optimizer/Cargo.toml
```

## Output

The example prints iteration-by-iteration progress and writes the best
instructions to `optimized_instructions.txt`. Sample output:

```
╔══════════════════════════════════════════╗
║  Prompt Optimizer — ADK-Rust v0.7.0      ║
╚══════════════════════════════════════════╝

📝 Initial agent instructions:
   "Answer questions"

📋 Loaded 3 test case(s) from eval_set/

🚀 Starting optimization loop...
═══════════════════════════════════════════

📊 Optimization Results:
   Initial score:     0.35
   Final score:       0.82
   Iterations run:    2

   🎯 Target threshold reached! Early stopping triggered.

💾 Optimized instructions written to: optimized_instructions.txt
```

## Adding Test Cases

Create a new JSON file in the `eval_set/` directory following the format above.
All `.json` files in the directory are automatically loaded and sorted by
filename for deterministic ordering.
