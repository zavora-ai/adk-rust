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
  "OpenAI gpt-5.6-terra|https://developers.openai.com/api/docs/pricing.md|gpt-5.6-terra|\$2.00|\$0.20|\$12.00"
  "OpenAI gpt-5|https://developers.openai.com/api/docs/pricing.md|gpt-5|\$1.25|\$0.125|\$10.00"
  "OpenAI gpt-5-nano|https://developers.openai.com/api/docs/pricing.md|gpt-5-nano|\$0.05|\$0.005|\$0.40"
  "OpenAI gpt-5.3-codex|https://developers.openai.com/api/docs/pricing.md|gpt-5.3-codex|\$1.75|\$0.175|\$14.00"
  "Gemini 3.7 Flash|https://ai.google.dev/gemini-api/docs/pricing|Gemini 3.7 Flash|\$0.75|\$3.75|\$0.075"
  "Gemini 3.5 Flash|https://ai.google.dev/gemini-api/docs/pricing|Gemini 3.5 Flash|\$1.50|\$9.00|\$0.15"
  "Gemini 2.5 Flash-Lite cache|https://ai.google.dev/gemini-api/docs/pricing|Gemini 2.5 Flash-Lite|\$0.10|\$0.40|\$0.01"
  "Anthropic Sonnet 5|https://docs.claude.com/en/docs/about-claude/pricing|Claude Sonnet 5|\$2 / MTok|\$10 / MTok"
  "Anthropic Opus 5|https://docs.claude.com/en/docs/about-claude/pricing|Claude Opus 5|\$5 / MTok|\$25 / MTok"
  "DeepSeek v4-flash|https://api-docs.deepseek.com/quick_start/pricing|deepseek-v4-flash|\$0.44|\$1.32"
)

failed=0
scratch_dir="$(mktemp -d)"
trap 'rm -rf -- "${scratch_dir}"' EXIT
response_file="${scratch_dir}/pricing-doc.txt"

for check in "${checks[@]}"; do
  IFS='|' read -r -a fields <<<"${check}"
  label="${fields[0]}"
  url="${fields[1]}"
  model="${fields[2]}"
  rates=("${fields[@]:3}")

  if ! curl --fail --location --silent --show-error --max-time 30 \
    --output "${response_file}" "${url}"; then
    echo "unreachable  ${label}: could not fetch ${url}"
    failed=1
    continue
  fi

  if python3 - "${response_file}" "${model}" "${rates[@]}" <<'PY'
import html
import pathlib
import re
import sys

page = pathlib.Path(sys.argv[1]).read_text(errors="ignore")
model, *rates = sys.argv[2:]
text = html.unescape(re.sub(r"\s+", " ", re.sub(r"<[^>]+>", " ", page)))
positions = [match.start() for match in re.finditer(re.escape(model), text, re.IGNORECASE)]

# Require every rate to occur in the same bounded model section. This avoids a
# false pass when an old price remains elsewhere on a provider's catalog page.
matched = any(
    all(rate in text[max(0, position - 1000):position + 1000] for rate in rates)
    for position in positions
)
if not matched:
    print(f"model/rate association not found for {model}: {', '.join(rates)}")
    raise SystemExit(1)
PY
  then
    echo "ok           ${label}"
  else
    echo "drift?       ${label}: model and rates were not associated at ${url}"
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
