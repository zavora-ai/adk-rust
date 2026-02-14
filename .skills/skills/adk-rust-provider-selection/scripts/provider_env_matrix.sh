#!/usr/bin/env bash
set -euo pipefail

provider="${1:-all}"

show() {
  local name="$1"
  local vars="$2"
  echo "[$name] required: $vars"
}

case "$provider" in
  gemini) show "gemini" "GOOGLE_API_KEY or Vertex credentials" ;;
  openai) show "openai" "OPENAI_API_KEY" ;;
  anthropic) show "anthropic" "ANTHROPIC_API_KEY" ;;
  deepseek) show "deepseek" "DEEPSEEK_API_KEY" ;;
  groq) show "groq" "GROQ_API_KEY" ;;
  ollama) show "ollama" "running local ollama server" ;;
  all)
    show "gemini" "GOOGLE_API_KEY or Vertex credentials"
    show "openai" "OPENAI_API_KEY"
    show "anthropic" "ANTHROPIC_API_KEY"
    show "deepseek" "DEEPSEEK_API_KEY"
    show "groq" "GROQ_API_KEY"
    show "ollama" "running local ollama server"
    ;;
  *)
    echo "Unknown provider: $provider" >&2
    exit 1
    ;;
esac
