# ADK Studio (retired)

ADK Studio, the visual editor for ADK-Rust agents, is retired (spec 3.3 §13 decision 12). Its
last release, adk-studio 1.0.1 on ADK-Rust 1.0, stays in
[zavora-ai/adk-studio](https://github.com/zavora-ai/adk-studio); ADK-Rust 3.0 has no Studio.

| Studio did | In ADK-Rust 3.0 |
|---|---|
| Watched a workflow's runs and steps | The [workflow observer](../surfaces/workflow.md): a graph agent's runs, steps and forks on the developer panel's **Runs** tab, from `adk-surface`'s `workflow` feature in a server that mounts it |
| Built workflows on a canvas | A [YAML or JSON workflow file](../agents/graph-workflows-yaml.md), checked with `adk-rust graph validate` and run with `adk-rust graph run`; `adk-rust graph schema` prints its JSON Schema for editor completion |
| Action nodes | [`adk-action`](../tools/action-nodes.md)'s 14 node types, run by `adk-graph`'s `ActionNodeExecutor` in a graph built in Rust or in a workflow file |
| Webhook, schedule and event triggers | The agent kernel's wake sources: [cron wakes](../kernel/scheduling.md#cron-wakes-and-missed-occurrences), file changes and webhooks with [`adk-rust wake`](../kernel/cli.md#adk-rust-wake) |

> **Note:** `adk-rust serve` does not mount the workflow observer: no adk-rust umbrella feature
> and no adk-cli feature turns it on, so `/api/surface/workflow/*` answers `404` there and the
> developer panel shows no **Runs** tab. The `scripted_workflow` example of `adk-surface` serves
> it; [The Workflow Observer](../surfaces/workflow.md#try-it) shows how to run it.
