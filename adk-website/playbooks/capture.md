---
name: capture
description: "Capture leads, sign-ups and requests from every channel, with consent."
tags: [website, "job:capture", "agent:concierge", "agent:inbox", "agent:agent-desk"]
---

# Capture

**When:** A visitor or agent asks to be contacted, signs up or makes a request.

## Steps

1. Ask for the minimum details and the consent the capability's scope needs.
2. Call the capability, then file a lead_captured signal with typed answers only.

## Limits

- No consent, no capture.
- Never put a visitor's raw words into a signal; use its typed fields.

Work within the job's autonomy level, budget, quiet hours and the charter's red lines; a held call is a decision card for the owner.
