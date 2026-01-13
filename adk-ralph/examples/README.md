# Ralph Example Prompts and PRDs

This directory contains example prompts and their expected output structures to help you understand how Ralph transforms project ideas into structured requirements, designs, and tasks.

## Directory Structure

```
examples/
├── README.md                    # This file
├── prompts/                     # Example prompts for different project types
│   ├── cli-rust.md              # CLI application in Rust
│   ├── web-api-python.md        # REST API in Python
│   ├── fullstack-typescript.md  # Full-stack TypeScript app
│   ├── library-go.md            # Go library
│   └── microservice-java.md     # Java microservice
└── outputs/                     # Expected output examples
    ├── cli-rust/
    │   ├── prd.md               # Generated PRD
    │   ├── design.md            # Generated design
    │   └── tasks.json           # Generated task list
    └── web-api-python/
        ├── prd.md
        ├── design.md
        └── tasks.json
```

## Using These Examples

### 1. Run Ralph with an Example Prompt

```bash
# Read the prompt file and pass to Ralph
cargo run -p adk-ralph -- "$(cat examples/prompts/cli-rust.md)"
```

### 2. Compare Output

After Ralph completes, compare the generated files with the examples in `outputs/` to understand the expected structure.

### 3. Customize for Your Needs

Use these examples as templates for your own project prompts. The more detail you provide, the better Ralph can understand your requirements.

## Prompt Writing Tips

1. **Start with the goal**: What is the main purpose of the project?
2. **List key features**: What functionality should it have?
3. **Specify technology**: What language, frameworks, and libraries?
4. **Define constraints**: Any performance, security, or compatibility requirements?
5. **Mention testing**: What testing approach and coverage?

## Example Prompt Structure

```markdown
Create a [type of project] in [language] called '[name]'.

Purpose:
[Brief description of what the project does]

Features:
- Feature 1 with details
- Feature 2 with details
- Feature 3 with details

Technical Requirements:
- Framework/library choices
- Database/storage requirements
- External integrations

Testing:
- Testing framework
- Coverage expectations
```
