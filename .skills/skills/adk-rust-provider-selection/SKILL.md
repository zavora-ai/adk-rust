---
name: adk-rust-provider-selection
description: Select and configure ADK-Rust model providers and feature flags with environment validation. Use when choosing Gemini/OpenAI/Anthropic/DeepSeek/Groq/Ollama/mistralrs for a project.
---

# ADK Rust Provider Selection

## Overview
Map product constraints to provider choice, then verify env and feature wiring before implementation.

## Workflow
1. Identify constraints: latency, cost, local/offline, tool-calling, streaming.
2. Pick provider and model family from the matrix.
3. Validate required env vars using `scripts/provider_env_matrix.sh`.
4. Confirm feature flags in Cargo manifests.

## Guardrails
1. Do not assume API key names; verify exact env var keys.
2. Keep provider selection explicit in code and docs.
3. Include at least one provider-specific smoke command.

## References
- Use `references/provider-matrix.md`.
