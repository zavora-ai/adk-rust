# User Personas Example

Demonstrates the **user personas** feature from `adk-eval` — loading persona
definitions from JSON files and using `UserSimulator` to drive multi-turn
conversations that reflect different user styles.

## What This Shows

- Loading persona profiles from a directory via `PersonaRegistry`
- Creating a `UserSimulator` for each persona with a separate LLM instance
- Running a 5-turn multi-turn conversation between each persona and an agent
  under test
- Comparing how the same agent responds differently to different persona styles

## Persona JSON Format

Each persona is defined as a JSON file in the `personas/` directory:

```json
{
  "name": "impatient-developer",
  "description": "A senior developer who wants quick, code-focused answers",
  "traits": {
    "communicationStyle": "direct and terse",
    "verbosity": "terse",
    "expertiseLevel": "expert"
  },
  "goals": [
    "Get a working code example quickly",
    "Understand performance implications"
  ],
  "constraints": [
    "Never ask for basic explanations",
    "Prefer code over prose"
  ]
}
```

### Fields

| Field | Description |
|-------|-------------|
| `name` | Unique identifier for the persona |
| `description` | Human-readable description |
| `traits.communicationStyle` | Free-form style description |
| `traits.verbosity` | `terse`, `normal`, or `verbose` |
| `traits.expertiseLevel` | `novice`, `intermediate`, or `expert` |
| `goals` | Objectives the persona pursues during conversation |
| `constraints` | Topics or patterns the persona avoids |

## Included Personas

| Persona | Expertise | Verbosity | Style |
|---------|-----------|-----------|-------|
| `impatient-developer` | Expert | Terse | Direct, code-focused |
| `curious-beginner` | Novice | Verbose | Friendly, inquisitive |

## Prerequisites

- `GOOGLE_API_KEY` environment variable set (for the Gemini LLM provider)

## Setup

```bash
cp .env.example .env
# Edit .env and add your Google API key
```

## Run

```bash
cargo run --manifest-path examples/user_personas/Cargo.toml
```

## Adding Custom Personas

Create a new JSON file in the `personas/` directory following the format above.
The `PersonaRegistry::load_directory()` call automatically picks up all `.json`
files in the directory.
