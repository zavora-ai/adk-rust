#!/usr/bin/env bash
set -euo pipefail

# Advisory check only: verifies that the per-token rates encoded in the pricing
# modules still appear on the vendors' pricing pages. Vendor pages change layout
# independently of their rates, so a miss means "re-verify by hand", not
# necessarily "the rate is wrong".
#
# Each check is provider|url|model|rate... where every rate must appear verbatim
# in the fetched page text. Rates are taken from:
#   adk-model/src/openai/pricing.rs
#   adk-gemini/src/pricing.rs
#   adk-anthropic/src/pricing.rs
checks=(
  "OpenAI gpt-5.6-terra|https://developers.openai.com/api/docs/pricing.md|\$2.00|\$0.20|\$12.00"
  "OpenAI gpt-5|https://developers.openai.com/api/docs/pricing.md|\$1.25|\$0.125|\$10.00"
  "OpenAI gpt-5-nano|https://developers.openai.com/api/docs/pricing.md|\$0.05|\$0.005|\$0.40"
  "OpenAI gpt-5.3-codex|https://developers.openai.com/api/docs/pricing.md|\$1.75|\$0.175|\$14.00"
  "Gemini 3.7 Flash|https://ai.google.dev/gemini-api/docs/pricing|\$0.75|\$3.75|\$0.075"
  "Gemini 3.5 Flash|https://ai.google.dev/gemini-api/docs/pricing|\$1.50|\$9.00|\$0.15"
  "Gemini 2.5 Flash-Lite cache|https://ai.google.dev/gemini-api/docs/pricing|\$0.10|\$0.40|\$0.01"
  "Anthropic Sonnet 5|https://docs.claude.com/en/docs/about-claude/pricing|\$2 / MTok|\$10 / MTok"
  "Anthropic Opus 5|https://docs.claude.com/en/docs/about-claude/pricing|\$5 / MTok|\$25 / MTok"
  "DeepSeek v4-flash|https://api-docs.deepseek.com/quick_start/pricing|\$0.44|\$1.32"
)

failed=0
scratch_dir="$(mktemp -d)"
trap 'rm -rf -- "${scratch_dir}"' EXIT
response_file="${scratch_dir}/pricing-doc.txt"

for check in "${checks[@]}"; do
  IFS='|' read -r -a fields <<<"${check}"
  label="${fields[0]}"
  url="${fields[1]}"
  rates=("${fields[@]:2}")

  if ! curl --fail --location --silent --show-error --max-time 30 \
    --output "${response_file}" "${url}"; then
    echo "unreachable  ${label}: could not fetch ${url}"
    failed=1
    continue
  fi

  missing=()
  for rate in "${rates[@]}"; do
    grep -Fq "${rate}" "${response_file}" || missing+=("${rate}")
  done

  if [ "${#missing[@]}" -eq 0 ]; then
    echo "ok           ${label}"
  else
    echo "drift?       ${label}: ${missing[*]} not found at ${url}"
    failed=1
  fi
done

if [ "${failed}" -ne 0 ]; then
  cat <<'EOF'

One or more encoded rates were not found on the vendor page. Re-verify against
the pricing page, update the constants and the "verified" date in the module
docs, then update the anchors in this script.
EOF
fi

exit "${failed}"
