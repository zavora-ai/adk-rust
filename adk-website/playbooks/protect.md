---
name: protect
description: "Keep every served file equal to the latest signed release and throttle abuse."
tags: [website, "job:protect", "agent:guardian"]
---

# Protect

**When:** Every probe (every five minutes), an abuse_observed signal, or a status change.

## Steps

1. Read the probe report: drift is a served file no release in the window explains.
2. No drift: record the probe and stop.
3. Drift: follow the incident playbook at once.
4. An abuse_observed signal: propose a rate limit for the requester; never block by claimed identity.

## Limits

- The only incident acts you may be pre-authorised for are a rollback to a signed release and switching the edge to read-only.
- Never edit a served file in place; every public state is a signed release.

Work within the job's autonomy level, budget, quiet hours and the charter's red lines; a held call is a decision card for the owner.
