// The home's layout (spec 3.3 §5.1): the sections in an icon rail on a wide screen, each still
// named by its words; a waiting decision that leads with its question; and the canvas beside a
// thread, which keeps one drawing in view at its latest version.
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { App } from "../src/App";
import { CanvasPane, Thread } from "../src/components/Thread";
import { DecisionCard } from "../src/components/DecisionCard";
import { FixtureClient, invoiceCard } from "../src/fixture/fixture";
import { GenuiFixtureClient, timesheetCard } from "../src/fixture/genui";
import type { FeedEntry } from "../src/generated/api";

describe("the icon rail", () => {
  it("gives each section a mark and keeps its name, Developer included", async () => {
    render(<App client={new FixtureClient()} initialSection="today" />);
    const nav = await screen.findByRole("navigation", { name: "Sections" });
    const links = within(nav).getAllByRole("link");
    expect(links.map((link) => link.textContent?.replace(/\d+$/, ""))).toEqual(["Threads", "Decisions", "Today", "Memory & rules", "Developer"]);
    // The Decisions mark carries the count of what waits.
    await waitFor(() => expect(within(nav).getByRole("link", { name: /^Decisions/ }).querySelector(".badge-tab")?.textContent).toBe("2"));
    for (const link of links) {
      expect(link.querySelector("svg.tab-icon")?.getAttribute("aria-hidden")).toBe("true");
      expect(link.getAttribute("title")).toBeTruthy();
    }
    await userEvent.click(within(nav).getByRole("link", { name: "Developer" }));
    expect(within(nav).getByRole("link", { name: "Developer" }).getAttribute("aria-current")).toBe("page");
    expect(await screen.findByRole("navigation", { name: "Developer panel" })).toBeTruthy();
  });
});

describe("a waiting decision", () => {
  it("leads with its question and what it costs, then who asks and the exact call", () => {
    render(<DecisionCard card={invoiceCard()} send={vi.fn()} onSettled={vi.fn()} />);
    const card = screen.getByRole("article", { name: "Send the October invoice to [CLIENT]" });
    const order = [...card.children].map((child) => child.className.split(" ")[0]);
    expect(order.slice(0, 3)).toEqual(["decision-heading", "decision-details", "decision-who"]);
    const mark = card.querySelector(".decision-qmark")!;
    expect(mark.textContent).toBe("?");
    expect(mark.getAttribute("aria-hidden")).toBe("true");
    expect(within(card).getByLabelText("The exact call")).toBeTruthy();
  });
});

describe("the canvas beside a thread", () => {
  const entries = (version: number): FeedEntry[] => [
    { seq: 1, at: "2026-11-20T10:01:00Z", atText: "10:01", items: [{ kind: "message", from: "agent", author: "Finance", text: "Here is October so far." }] },
    { seq: 2, at: "2026-11-20T10:02:00Z", atText: "10:02", items: [{ kind: "drawn", surface: timesheetCard(1) }] },
    ...(version > 1 ? [{ seq: 3, at: "2026-11-20T10:05:00Z", atText: "10:05", items: [{ kind: "drawn" as const, surface: timesheetCard(2) }] }] : []),
  ];

  it("shows a drawing beside the thread, leaving a line that brings it back", async () => {
    const onPin = vi.fn();
    const { rerender } = render(
      <Thread entries={entries(1)} open={[]} client={new GenuiFixtureClient("display")} send={vi.fn()} onSettled={vi.fn()} session="s-finance" onPin={onPin} />,
    );
    await userEvent.click(screen.getByRole("button", { name: "Show beside" }));
    const surfaceId = timesheetCard().surfaceId;
    expect(onPin).toHaveBeenCalledWith(surfaceId);
    rerender(
      <Thread entries={entries(1)} open={[]} client={new GenuiFixtureClient("display")} send={vi.fn()} onSettled={vi.fn()} session="s-finance" pinned={surfaceId} onPin={onPin} />,
    );
    expect(screen.queryByRole("button", { name: "Show beside" })).toBeNull();
    expect(screen.getByText("shown beside the thread", { exact: false })).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Bring back" }));
    expect(onPin).toHaveBeenLastCalledWith(undefined);
  });

  it("keeps the latest version, its earlier ones and its A2UI", async () => {
    const onClose = vi.fn();
    const surfaceId = timesheetCard().surfaceId;
    render(
      <CanvasPane entries={entries(2)} pinned={surfaceId} open={[]} client={new GenuiFixtureClient("display")} send={vi.fn()} onSettled={vi.fn()} session="s-finance" onClose={onClose} />,
    );
    const canvas = screen.getByRole("complementary", { name: "Canvas" });
    expect(within(canvas).getByText("v2")).toBeTruthy();
    const tabs = within(canvas).getByRole("tablist");
    expect(within(tabs).getAllByRole("tab").map((tab) => tab.textContent)).toEqual(["Drawing", "Earlier versions · 1", "A2UI"]);
    await userEvent.click(within(tabs).getByRole("tab", { name: "A2UI" }));
    expect(within(canvas).getByRole("tabpanel").textContent).toContain(`"surfaceId": "${surfaceId}"`);
    await userEvent.click(within(canvas).getByRole("button", { name: "Close the canvas" }));
    expect(onClose).toHaveBeenCalled();
  });

  it("says so when its drawing has left the thread", () => {
    render(<CanvasPane entries={[]} pinned="gone" open={[]} client={new GenuiFixtureClient("display")} send={vi.fn()} onSettled={vi.fn()} onClose={vi.fn()} />);
    expect(screen.getByText("This drawing is no longer in the thread.")).toBeTruthy();
  });

  it("opens in place of the side panel on the threads screen", async () => {
    render(<App client={new GenuiFixtureClient("display")} initialSection="threads" />);
    const pins = await screen.findAllByRole("button", { name: "Show beside" });
    await userEvent.click(pins[0]!);
    const canvas = await screen.findByRole("complementary", { name: "Canvas" });
    expect(within(canvas).getByRole("tab", { name: "Drawing" }).getAttribute("aria-selected")).toBe("true");
    await waitFor(() => expect(document.querySelector(".side")?.hasAttribute("hidden")).toBe(true));
    await userEvent.click(within(canvas).getByRole("button", { name: "Close the canvas" }));
    await waitFor(() => expect(screen.queryByRole("complementary", { name: "Canvas" })).toBeNull());
  });
});

describe("the phone's decision", () => {
  it("leads with its question too, its heading still named by the card", async () => {
    const { PhoneApp } = await import("../src/phone/PhoneApp");
    const { phoneFixture } = await import("../src/fixture/phone");
    const phone = phoneFixture("decide");
    render(<PhoneApp client={phone.client} initial={phone.initial} />);
    const heading = await screen.findByRole("heading", { name: "Publish a release", level: 1 });
    const mark = heading.querySelector('[aria-hidden="true"]');
    expect(mark?.textContent).toBe("?");
  });
});

describe("the canvas in its own window", () => {
  it("opens the drawing at its own address, and that window follows the session", async () => {
    const open = vi.spyOn(window, "open").mockReturnValue(null);
    try {
      const surfaceId = timesheetCard().surfaceId;
      render(
        <CanvasPane entries={[{ seq: 2, at: "2026-11-20T10:02:00Z", atText: "10:02", items: [{ kind: "drawn", surface: timesheetCard(1) }] }]} pinned={surfaceId} open={[]} client={new GenuiFixtureClient("display")} send={vi.fn()} onSettled={vi.fn()} session="s-finance" onClose={vi.fn()} />,
      );
      await userEvent.click(screen.getByRole("button", { name: "Open in its own window" }));
      expect(open).toHaveBeenCalledWith(`/ui/canvas?session=s-finance&surface=${encodeURIComponent(surfaceId)}`, "_blank", "popup,width=760,height=900");
    } finally {
      open.mockRestore();
    }
    const { CanvasWindow } = await import("../src/screens/CanvasWindow");
    const { cleanup } = await import("@testing-library/react");
    cleanup();
    render(<CanvasWindow client={new GenuiFixtureClient("display")} send={vi.fn()} location={{ search: `?session=s-finance&surface=${encodeURIComponent(timesheetCard().surfaceId)}` }} />);
    const canvas = await screen.findByRole("complementary", { name: "Canvas" });
    expect(await within(canvas).findByText("October timesheet", { selector: "h2" })).toBeTruthy();
    expect(within(canvas).queryByRole("button", { name: "Open in its own window" })).toBeNull();
  });
});

describe("ask for a drawing", () => {
  it("puts the request in the message box, sent only with Send", async () => {
    const { Composer } = await import("../src/components/Composer");
    const onSend = vi.fn().mockResolvedValue(undefined);
    render(<Composer askDrawing placeholder="Message Finance" queued={[]} onSend={onSend} />);
    const box = screen.getByLabelText("Message Finance") as HTMLInputElement;
    await userEvent.type(box, "October's hours by client");
    await userEvent.click(screen.getByRole("button", { name: "Ask for a drawing" }));
    expect(box.value).toBe("Show this as a drawing I can look at: October's hours by client");
    expect(document.activeElement).toBe(box);
    expect(onSend).not.toHaveBeenCalled();
    await userEvent.click(screen.getByRole("button", { name: "Ask for a drawing" }));
    expect(box.value).toBe("Show this as a drawing I can look at: October's hours by client");
  });
});
