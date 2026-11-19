// A busy agent's steps fold into one work-log line (spec 3.3 §5.1): three or more plain tool
// steps in a row read as "5 steps · 1 failed · the last one", open on demand; a command's run
// keeps its card, and a step still working stays in view.
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { lines, Thread, worklog } from "../src/components/Thread";
import { codeThread, FixtureClient } from "../src/fixture/fixture";
import type { FeedEntry, ThreadItem } from "../src/generated/api";

function steps(count: number, { failed = [] as number[], running = false } = {}): FeedEntry[] {
  const items: ThreadItem[] = [];
  for (let n = 1; n <= count; n += 1) {
    items.push({ kind: "activity", tool: "read_file", text: `Read file part-${n}.csv`, callId: `c${n}` });
    if (!(running && n === count)) {
      items.push({ kind: "result", tool: "read_file", ok: !failed.includes(n), text: failed.includes(n) ? "Failed" : "Done", callId: `c${n}` });
    }
  }
  return [{ seq: 1, at: "2026-11-20T09:00:00Z", atText: "09:00", items }];
}

describe("the work log", () => {
  it("folds three or more steps in a row, and leaves two alone", () => {
    expect(worklog(lines(steps(2))).map((unit) => unit.kind)).toEqual(["line", "line"]);
    const units = worklog(lines(steps(5)));
    expect(units.map((unit) => unit.kind)).toEqual(["log"]);
    expect(units[0]!.kind === "log" && units[0]!.steps.length).toBe(5);
  });

  it("keeps a step still working in view, outside the fold", () => {
    const units = worklog(lines(steps(4, { running: true })));
    expect(units.map((unit) => unit.kind)).toEqual(["log", "line"]);
  });

  it("never folds a command's run", () => {
    expect(worklog(lines(codeThread())).map((unit) => unit.kind)).toEqual(["line", "line", "line", "line"]);
  });

  it("says how many steps, how many failed and the last, and opens to show them all", async () => {
    render(<Thread entries={steps(5, { failed: [2] })} open={[]} client={new FixtureClient()} send={vi.fn()} onSettled={vi.fn()} />);
    const log = screen.getByText("5 steps").closest("details")!;
    const summary = within(log.querySelector("summary")!);
    expect(summary.getByText("1 failed")).toBeTruthy();
    expect(summary.getByText("Read file part-5.csv")).toBeTruthy();
    expect(log.open).toBe(false);
    await userEvent.click(screen.getByText("5 steps"));
    expect(log.open).toBe(true);
    expect(within(log).getByText("Read file part-2.csv").closest(".activity")?.className).toContain("activity-failed");
  });
});
