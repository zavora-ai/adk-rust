---
name: alarms
description: "Route every alarm by urgency to the owner's channels."
tags: [website, "job:alarms", "agent:guardian"]
---

# Alarms

**When:** Any alarm: an incident, a payment anomaly, a guardrail breach, a held decision blocking a customer, a failed renewal.

## Steps

1. P1 — incident, site down, payment anomaly, suspected key compromise: send at once on every P1 channel, inside quiet hours, and repeat until acknowledged.
2. P2 — a held decision blocking a customer, a guardrail breach, a failed renewal: send within the hour on the owner's chosen channel, outside quiet hours.
3. P3 — everything else: add it to the weekly review.

## Limits

- One alarm per event; never resend a P2 or P3.
- An alarm states what happened, what was already done and what the owner must decide.

Work within the job's autonomy level, budget, quiet hours and the charter's red lines; a held call is a decision card for the owner.
