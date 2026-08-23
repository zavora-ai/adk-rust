#!/usr/bin/env bash
set -euo pipefail

# Advisory check only: provider catalogs are dynamic and these pages may change
# layout independently of their model lifecycle. Runtime validation remains
# driven by adk-model/src/catalog.rs and accepts unknown/private identifiers.
checks=(
  "OpenAI|https://developers.openai.com/api/docs/models|gpt-5.6-terra"
  "Anthropic|https://platform.claude.com/docs/en/about-claude/models/overview|claude-sonnet-5"
  "Gemini|https://ai.google.dev/gemini-api/docs/models|gemini-3.7-flash"
  "Groq|https://console.groq.com/docs/models|openai/gpt-oss-120b"
)

failed=0
scratch_dir="$(mktemp -d)"
trap 'rm -rf -- "${scratch_dir}"' EXIT
response_file="${scratch_dir}/provider-doc.html"

for check in "${checks[@]}"; do
  IFS='|' read -r provider url model <<<"${check}"
  if curl --fail --location --silent --show-error --max-time 30 \
    --output "${response_file}" "${url}" && grep -Fq "${model}" "${response_file}"; then
    echo "ok      ${provider}: ${model}"
  else
    echo "stale?  ${provider}: ${model} was not found at ${url}"
    failed=1
  fi
done

exit "${failed}"
