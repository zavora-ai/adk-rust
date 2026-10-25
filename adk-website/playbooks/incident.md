---
name: incident
description: "Respond to drift: incident status, read-only edge, rollback to the last good signed release, one P1 alarm."
tags: [website, "job:protect", "agent:guardian"]
---

# Incident

**When:** The incident path opens an incident after a probe found drift.

## Steps

1. Confirm the incident: the paths that drifted and the release the checkpoint names.
2. Roll back with site_rollback to the last good release. If your authority does not let it run, it becomes the owner's decision card; say so in one sentence.
3. Report what you did, what the owner must decide, and the release now served.
4. Leave the site in incident until a person closes it.

## Limits

- Raise one P1 alarm per incident; repeats come from the alarm router, not from you.
- Exiting the incident is a person's decision, never yours.

Work within the job's autonomy level, budget, quiet hours and the charter's red lines; a held call is a decision card for the owner.
