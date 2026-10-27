// The workflow observer's Runs tab (spec 3.3 §12 developer track): shown only where the server
// observes graph runs; the run's words and graph; a step's "What changed"; the fork dialog's
// checks; and a waiting node's card drawn from the inbox card only (S-I1).
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App } from "../src/App";
import { ApiError } from "../src/client";
import { FixtureClient } from "../src/fixture/fixture";
import {
  DECISION,
  SESSION,
  WorkflowFixture,
  chargeCard,
  doneRun,
  invoiceRun,
  stepDetail,
  waitingRun,
} from "../src/fixture/workflow";
import type { DecisionCard } from "../src/generated/api";
import { HttpWorkflowClient } from "../src/workflow/client";
import { check, FORK_VALUE_LIMIT } from "../src/workflow/fork";
import { isTaken, layout } from "../src/workflow/layout";
import { runsOf } from "../src/workflow/route";
import { Runs } from "../src/workflow/Runs";
import type { RunsHome } from "../src/workflow/Runs";
import { COLLAPSED_LINES, StepDetailView } from "../src/workflow/StepDetail";
import { ForkDialog } from "../src/workflow/ForkDialog";

afterEach(() => {
  vi.unstubAllGlobals();
  window.history.replaceState(null, "", "/");
});

/** The home's inbox: `cards` by id; a decision it does not hold answers 404. */
function inbox(cards: DecisionCard[] = []): RunsHome & { commands: unknown[] } {
  const commands: unknown[] = [];
  return {
    commands,
    async decision(id: string) {
      const card = cards.find((known) => known.id === id);
      if (!card) throw new ApiError(404, "not found", "surface.not_found");
      return card;
    },
    async command(command) {
      commands.push(command);
      return { type: "answered", card: { ...chargeCard(), status: "decided", statusText: "Decided", allowed: { options: [], openInHome: false } } };
    },
  };
}

describe("the Runs tab", () => {
  it("shows only where the server observes graph runs", async () => {
    window.history.replaceState(null, "", "/ui/developer?runs=s-7f21");
    const { unmount } = render(
      <App client={new FixtureClient()} initialSection="developer" workflow={new WorkflowFixture(waitingRun(), true)} />,
    );
    const absent = await screen.findByRole("navigation", { name: "Developer panel" });
    expect(within(absent).getAllByRole("button").map((tab) => tab.textContent)).toEqual([
      "Topology",
      "Protocols",
      "Telemetry",
      "State",
    ]);
    unmount();
    render(<App client={new FixtureClient()} initialSection="developer" workflow={new WorkflowFixture()} />);
    const tabs = await screen.findByRole("navigation", { name: "Developer panel" });
    await waitFor(() => expect(within(tabs).getAllByRole("button").map((tab) => tab.textContent?.replace(/\d/g, ""))).toContain("Runs"));
  });

  it("probes the routes and reads 404 as a server without them", async () => {
    for (const [status, expected] of [
      [404, undefined],
      [200, [invoiceRun()]],
    ] as const) {
      vi.stubGlobal(
        "fetch",
        vi.fn(async () => new Response(JSON.stringify(status === 200 ? { graphs: expected } : { error: { message: "no route" } }), { status })),
      );
      expect(await new HttpWorkflowClient().graphs()).toEqual(expected);
    }
  });

  it("opens at the address a card links to", () => {
    expect([
      runsOf({ pathname: "/ui/developer/runs/s%2D1", search: "" }),
      runsOf({ pathname: "/ui/fixture.html", search: "?runs=s-7f21" }),
      runsOf({ pathname: "/ui/developer", search: "" }),
    ]).toEqual([{ session: "s-1" }, { session: "s-7f21" }, undefined]);
  });

  it("says what there is to say without a thread or a graph", async () => {
    const workflow = new WorkflowFixture();
    const { rerender } = render(<Runs client={workflow} home={inbox()} graphs={[invoiceRun()]} agent="invoice-run" />);
    expect(screen.getByText("Open a thread to see its run.")).toBeTruthy();
    rerender(<Runs client={workflow} home={inbox()} graphs={[invoiceRun()]} agent="Research" session={SESSION} />);
    expect(screen.getByText("Research is not a graph agent. Graph runs show here.")).toBeTruthy();
  });

  it("names the run, each node and each step in words", async () => {
    render(<Runs client={new WorkflowFixture(doneRun())} home={inbox()} graphs={[invoiceRun()]} agent="invoice-run" session={SESSION} />);
    await screen.findByText("Started 09:14 · 5.0 s · 4 steps");
    const graph = screen.getByRole("img", { name: /invoice-run/ });
    expect(graph.getAttribute("aria-label")).toBe("invoice-run: draft Done, pay Done, receipt Done");
    const steps = screen.getByRole("region", { name: "Steps" });
    expect([...steps.querySelectorAll(".step-row strong")].map((row) => row.textContent)).toEqual([
      "Step 0 · 09:14:00",
      "Step 1 · 09:14:01",
      "Step 2 · 09:14:03",
      "Step 3 · 09:15:03",
    ]);
    expect(within(steps).getByText("from step 1 · edited note")).toBeTruthy();
  });

  it("draws a waiting node's card from the inbox card only", async () => {
    const { unmount } = render(
      <Runs client={new WorkflowFixture()} home={inbox([chargeCard()])} graphs={[invoiceRun()]} agent="invoice-run" session={SESSION} />,
    );
    expect(await screen.findByRole("button", { name: /Confirm it's you and approve/ })).toBeTruthy();
    expect(screen.getByText("Charge Acme 42.00 USD")).toBeTruthy();
    unmount();
    // The run says pay waits on a decision; the inbox holds none: nothing answers here.
    render(<Runs client={new WorkflowFixture()} home={inbox()} graphs={[invoiceRun()]} agent="invoice-run" session={SESSION} />);
    await screen.findByText("Started 09:14 · 4.2 s · 3 steps");
    await new Promise((resolve) => setTimeout(resolve, 20));
    for (const name of [/approve/i, /^Edit$/, /^Deny$/, /Allow once/]) {
      expect(screen.queryByRole("button", { name })).toBeNull();
    }
  });

  it("answers through the engine's decide command", async () => {
    const home = inbox([chargeCard()]);
    render(<Runs client={new WorkflowFixture()} home={home} graphs={[invoiceRun()]} agent="invoice-run" session={SESSION} />);
    await userEvent.click(await screen.findByRole("button", { name: /^Deny$/ }));
    await waitFor(() => expect(home.commands).toEqual([expect.objectContaining({ type: "decide", decisionId: DECISION, answer: "deny" })]));
  });
});

describe("the run graph's layout", () => {
  it("lays nodes out by distance from the entry, END last, and marks the edges taken", () => {
    const placed = layout(invoiceRun());
    expect(placed.nodes.map((node) => [node.name, node.x, node.y])).toEqual([
      ["draft", 16, 44],
      ["pay", 232, 44],
      ["receipt", 448, 44],
    ]);
    expect(placed.end?.x).toBe(664);
    expect(placed.edges.map((routed) => [routed.edge.from, routed.edge.to, routed.path.includes("C")])).toEqual([
      ["draft", "__end__", true],
      ["draft", "pay", false],
      ["pay", "receipt", false],
      ["receipt", "__end__", false],
    ]);
    expect(invoiceRun().edges.map((edge) => isTaken(edge, waitingRun().taken))).toEqual([false, true, false, false]);
  });
});

describe("a step's detail", () => {
  it("names each change in words and hides Before when every key was added", () => {
    render(<StepDetailView detail={stepDetail()} onBack={() => undefined} />);
    const table = screen.getByRole("table");
    expect(within(table).getAllByRole("columnheader").map((cell) => cell.textContent)).toEqual(["Key", "After"]);
    expect(within(table).getByText("Added")).toBeTruthy();
  });

  it("shows before and after for a change and a removal, collapsing long values", async () => {
    const lines = Array.from({ length: 30 }, (_, day) => ({ day }));
    const detail = {
      ...stepDetail(),
      changes: [
        { key: "invoice", change: "changed" as const, changeText: "Changed", before: { status: "ready" }, after: { lines } },
        { key: "note", change: "removed" as const, changeText: "Removed", before: "Bill Acme for October" },
        { key: "token", change: "added" as const, changeText: "Added", after: "[redacted]", note: "Hidden: looks like a secret" },
      ],
    };
    render(<StepDetailView detail={detail} onBack={() => undefined} />);
    const table = screen.getByRole("table");
    expect(within(table).getAllByRole("columnheader").map((cell) => cell.textContent)).toEqual(["Key", "Before", "After"]);
    expect(within(table).getAllByRole("rowheader").map((cell) => cell.textContent)).toEqual(["invoiceChanged", "noteRemoved", "tokenAdded"]);
    expect(within(table).getByText("Hidden: looks like a secret")).toBeTruthy();
    const total = JSON.stringify({ lines }, null, 2).split("\n").length;
    const showAll = within(table).getByRole("button", { name: `Show all ${total} lines` });
    expect(showAll.previousElementSibling?.textContent?.split("\n").length).toBe(COLLAPSED_LINES);
    await userEvent.click(showAll);
    expect(within(table).getByRole("button", { name: "Show less" })).toBeTruthy();
  });

  it("says a step that changed nothing", () => {
    render(<StepDetailView detail={{ ...stepDetail(), changes: [] }} onBack={() => undefined} />);
    expect(screen.getByText("This step changed nothing.")).toBeTruthy();
  });
});

describe("the fork dialog", () => {
  it("checks each edit as JSON, its size and the graph's channels", () => {
    const channels = ["invoice", "note"];
    expect(check({ note: '"re-run for audit"', invoice: '{"amountCents":5000}' }, channels)).toEqual({
      set: { note: "re-run for audit", invoice: { amountCents: 5000 } },
      errors: {},
    });
    expect(check({ note: "re-run" }, channels)).toEqual({ set: {}, errors: { note: "Not valid JSON" } });
    expect(check({ note: JSON.stringify("x".repeat(FORK_VALUE_LIMIT)) }, channels)).toEqual({ set: {}, errors: { note: "Too large" } });
    expect(check({ amount: "5000" }, channels)).toEqual({ set: {}, errors: {}, notAChannel: "amount" });
  });

  it("forks with the edited channel and starts it, then shows the fork", async () => {
    const workflow = new WorkflowFixture(doneRun());
    render(<Runs client={workflow} home={inbox()} graphs={[invoiceRun()]} agent="invoice-run" session={SESSION} />);
    const steps = await screen.findByRole("region", { name: "Steps" });
    await userEvent.click(within(steps).getAllByRole("button", { name: "Fork from here" })[1]);
    const dialog = await screen.findByRole("dialog", { name: "Fork from step 1" });
    expect(within(dialog).getByText("Runs again: pay, receipt")).toBeTruthy();
    expect(within(dialog).getByText(/Already happened and won't be sent again unless you change it: pay — POST/)).toBeTruthy();
    expect(within(dialog).getByText("A changed call waits for you.")).toBeTruthy();
    const note = within(dialog).getAllByRole("button", { name: "Edit" })[2];
    await userEvent.click(note);
    const editor = within(dialog).getByRole("textbox", { name: "note" });
    await userEvent.clear(editor);
    await userEvent.type(editor, "re-run");
    expect(within(dialog).getByText("Not valid JSON")).toBeTruthy();
    expect((within(dialog).getByRole("button", { name: "Fork" }) as HTMLButtonElement).disabled).toBe(true);
    await userEvent.clear(editor);
    await userEvent.type(editor, '"re-run for audit"');
    await userEvent.click(within(dialog).getByRole("button", { name: "Fork" }));
    await screen.findByText("Forked. The original run is unchanged.");
    expect(workflow.forks).toEqual([{ from: "cp-1", set: { note: "re-run for audit" }, start: true }]);
    expect(await screen.findByText("Fork of the 09:14 run at step 1 · edited note")).toBeTruthy();
  });

  it("names what becomes of a changed call only when the server says, and edits no capped value", async () => {
    const detail = stepDetail();
    const capped = {
      ...detail,
      fork: { ...detail.fork, changedCalls: undefined },
      stateValues: [{ key: "document", value: "xxxx…", note: "Too large to show in full", editable: false }],
    };
    render(<ForkDialog detail={capped} channels={["document"]} onFork={() => Promise.reject(new Error("not sent"))} onClose={() => undefined} />);
    const dialog = screen.getByRole("dialog", { name: "Fork from step 1" });
    expect(within(dialog).queryByText("A changed call waits for you.")).toBeNull();
    expect(within(dialog).getByText("Too large to show in full")).toBeTruthy();
    expect((within(dialog).getByRole("button", { name: "Edit" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("is refused while a decision waits", async () => {
    render(<Runs client={new WorkflowFixture()} home={inbox([chargeCard()])} graphs={[invoiceRun()]} agent="invoice-run" session={SESSION} />);
    const steps = await screen.findByRole("region", { name: "Steps" });
    await userEvent.click(within(steps).getAllByRole("button", { name: "Fork from here" })[1]);
    const dialog = await screen.findByRole("dialog", { name: "Fork from step 1" });
    expect(within(dialog).getByText("Answer the waiting decision first.")).toBeTruthy();
    expect((within(dialog).getByRole("button", { name: "Fork" }) as HTMLButtonElement).disabled).toBe(true);
    await userEvent.keyboard("{Escape}");
    expect(screen.queryByRole("dialog")).toBeNull();
  });
});
