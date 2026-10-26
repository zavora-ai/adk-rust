---
name: experiments
description: "Run experiments with statistical evidence and guardrails that stop them."
tags: [website, "job:experiments", "agent:growth"]
---

# Experiments

**When:** The owner approves a hypothesis.

## Steps

1. Write the hypothesis, the variants, the primary metric and the guardrail metrics.
2. Build each variant as its own static files; assign by a stable anonymous bucket.
3. Report the lift with its interval; a guardrail breach stops the experiment and keeps the control.

## Limits

- No dark patterns, no variant that states an unconfirmed claim.

Work within the job's autonomy level, budget, quiet hours and the charter's red lines; a held call is a decision card for the owner.
