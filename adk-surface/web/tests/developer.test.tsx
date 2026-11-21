// The developer view of a run (spec 3.3 §12 developer track): the graph, the tree and the trace
// of one run, read from the session's trace spans or, without telemetry, its events.
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../src/App";
import { duration, memberStates, openWork, runFromEvents, runFromSpans, shownSteps, tickText, ticks } from "../src/developer/run";
import type { TraceSpan } from "../src/developer/types";
import { DeveloperFixture, operationsAgent, operationsEvents, operationsTrace } from "../src/fixture/developer";

const NOW = Date.parse("2026-11-19T10:00:00Z");

describe("a run from the trace", () => {
  const run = runFromSpans(operationsTrace(NOW), NOW, openWork(operationsEvents(NOW)));

  it("nests every step under the agent that ran it, with handoffs for the agents it handed to", () => {
    expect(run.steps.map((step) => `${"  ".repeat(step.depth)}${step.kind} ${step.label}`)).toEqual([
      "agent operations",
      "  model claude-opus-5-5",
      "  handoff → metrics",
      "    model claude-sonnet-5-5",
      "    tool query_p95",
      "    tool read_deploys",
      "    model claude-sonnet-5-5",
      "  model claude-opus-5-5",
      "  tool render_screen",
      "  handoff → deployer",
      "    model claude-sonnet-5-5",
      "    held restart_service",
    ]);
  });

  it("folds an agent's call_llm into its generation, keeping the response", () => {
    const first = run.steps[1];
    expect(first.tokens).toEqual({ input: 1240, output: 186 });
    expect(first.detail).toEqual({ label: "Response", text: "Delegate the measurement to Metrics, then decide on a rollback." });
    expect(run.steps.some((step) => step.label === "call_llm" || step.label === "model call")).toBe(false);
  });

  it("is live while a call waits, and that call is held for the person", () => {
    expect(run.live).toBe(true);
    const held = run.steps.find((step) => step.kind === "held");
    expect(held?.end).toBeUndefined();
    expect(held?.agent).toBe("deployer");
    expect(held?.detail?.text).toContain('"checkout-api"');
    expect(run.total).toBeCloseTo(38_400, -1);
    expect(run.tokens).toEqual({ input: 6290, output: 820 });
  });

  it("gives each member its state: the coordinator works, Metrics is done, Deployer needs you", () => {
    const states = memberStates(run, operationsAgent().topology!.members);
    expect(states.get("operations")?.state).toBe("active");
    expect(states.get("metrics")).toEqual({ state: "done", time: expect.closeTo(9800, -1) });
    expect(states.get("deployer")?.state).toBe("held");
  });

  it("ends when the trace has every span and nothing waits", () => {
    const done = runFromSpans(operationsTrace(NOW), NOW);
    expect(done.live).toBe(false);
    expect(done.steps.find((step) => step.label === "metrics")?.kind).toBe("agent");
  });

  it("makes text a model or tool wrote inert", () => {
    const sly: TraceSpan = { ...operationsTrace(NOW)[0], name: "", span_id: "x", attributes: { "gen_ai.operation.name": "execute_tool", "gen_ai.tool.name": "read‮file", "gen_ai.tool.call.result": "ok​" } };
    const steps = runFromSpans([sly], NOW).steps;
    expect(steps[0].label).toBe("read\\u{202e}file");
    expect(steps[0].detail?.text).toBe("ok\\u{200b}");
  });
});

describe("a run from the session's events", () => {
  const run = runFromEvents(operationsEvents(NOW), NOW);

  it("shows the person, the agents in turn and the call that waits", () => {
    expect(run.source).toBe("events");
    expect(run.steps.map((step) => `${"  ".repeat(step.depth)}${step.kind} ${step.label}`)).toEqual([
      "person You: Checkout feels slow since this morning. Find out why, and …",
      "agent operations",
      "  model Asking Metrics to measure first.",
      "  handoff hands off to metrics",
      "  handoff → metrics",
      "    model checkout p95 went from 410 to 840 ms after the 09:12 deploy.",
      "  model Handing the rollback to Deployer.",
      "  handoff hands off to deployer",
      "  handoff → deployer",
      "    held restart_service",
    ]);
    expect(run.live).toBe(true);
    expect(run.steps.find((step) => step.label === "operations")?.end).toBeUndefined();
  });

  it("records a state change on the step that made it", () => {
    const step = run.steps.find((candidate) => candidate.label === "Handing the rollback to Deployer.");
    expect(step?.attrs).toContainEqual(["state_delta", expect.stringContaining('"suspect"')]);
  });
});

describe("the run's helpers", () => {
  it("hides the steps under a folded step", () => {
    const run = runFromSpans(operationsTrace(NOW), NOW, openWork(operationsEvents(NOW)));
    const metrics = run.steps.find((step) => step.label === "→ metrics")!;
    const shown = shownSteps(run.steps, new Set([metrics.id]));
    expect(shown.length).toBe(run.steps.length - 4);
    expect(shown).toContain(metrics);
  });

  it("writes durations and round ticks for people", () => {
    expect([duration(0.4), duration(212), duration(2600), duration(38_400), duration(125_000)]).toEqual(["<1 ms", "212 ms", "2.60 s", "38.4 s", "2 min 5 s"]);
    expect(ticks(38_400)).toEqual([0, 10_000, 20_000, 30_000]);
    expect(ticks(800)).toEqual([0, 200, 400, 600, 800]);
    expect([0, 200, 2500, 10_000].map(tickText)).toEqual(["0", "200 ms", "2.5 s", "10 s"]);
  });
});

describe("the developer panel's run view", () => {
  // The user ID the panel reads the session's events with, as a developer typed it before.
  beforeEach(() => {
    const items = new Map<string, string>([["adk.home.developer.user", "you"]]);
    vi.stubGlobal("localStorage", {
      get length() {
        return items.size;
      },
      clear: () => items.clear(),
      getItem: (key: string) => items.get(key) ?? null,
      key: (index: number) => [...items.keys()][index] ?? null,
      removeItem: (key: string) => void items.delete(key),
      setItem: (key: string, value: string) => void items.set(key, String(value)),
    } satisfies Storage);
  });
  afterEach(() => vi.unstubAllGlobals());

  it("names its tabs, opening on the run", async () => {
    render(<App client={new DeveloperFixture()} initialSection="developer" />);
    const tabs = await screen.findByRole("navigation", { name: "Developer panel" });
    expect(within(tabs).getAllByRole("button").map((tab) => tab.textContent)).toEqual(["Run", "Protocols", "State"]);
    expect(within(tabs).getByRole("button", { name: "Run" }).getAttribute("aria-pressed")).toBe("true");
  });

  it("opens on what waits for the person, and links the graph, the tree and the trace", async () => {
    render(<App client={new DeveloperFixture()} initialSection="developer" />);
    const tree = await screen.findByRole("tree", { name: "Steps of this run" });
    await waitFor(() => expect(within(tree).getAllByRole("treeitem").length).toBe(12));
    const inspector = screen.getByRole("complementary", { name: "restart_service" });
    expect(within(inspector).getByText("Held for you", { selector: "dd" })).toBeTruthy();
    expect(screen.getByRole("group", { name: "This run" }).textContent).toContain("Held for you");

    // The graph's Metrics picks its handoff in the tree and the trace.
    const graph = screen.getByRole("group", { name: "Agents and how they hand work to each other" });
    await userEvent.click(within(graph).getByRole("button", { name: /metrics/ }));
    expect(within(tree).getByRole("treeitem", { selected: true }).textContent).toContain("→ metrics");
    const trace = screen.getByRole("list", { name: "Steps on a timeline" });
    expect(within(trace).getByRole("button", { pressed: true }).getAttribute("aria-label")).toMatch(/^→ metrics, Handoff/);
    expect(within(graph).getByRole("button", { name: /deployer/ }).textContent).toContain("Needs you");
  });

  it("moves through the tree with the keyboard and folds a branch", async () => {
    render(<App client={new DeveloperFixture()} initialSection="developer" />);
    const tree = await screen.findByRole("tree", { name: "Steps of this run" });
    await waitFor(() => expect(within(tree).getAllByRole("treeitem").length).toBe(12));
    const items = within(tree).getAllByRole("treeitem");
    items[0].focus();
    fireEvent.keyDown(items[0], { key: "Home" });
    fireEvent.keyDown(items[0], { key: "ArrowDown" });
    fireEvent.keyDown(items[1], { key: "ArrowDown" });
    expect(document.activeElement?.textContent).toContain("→ metrics");
    fireEvent.keyDown(document.activeElement!, { key: "ArrowLeft" });
    await waitFor(() => expect(within(tree).getAllByRole("treeitem").length).toBe(8));
    expect(within(tree).getByRole("treeitem", { name: /→ metrics/ }).getAttribute("aria-expanded")).toBe("false");
  });
});
