// Suggested replies (spec 3.3 §5.1): an agent's offered replies stand under its latest message
// until the person writes; a tap fills the message box and sends nothing.
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { Composer } from "../src/components/Composer";
import { Thread } from "../src/components/Thread";
import { FixtureClient } from "../src/fixture/fixture";
import type { FeedEntry } from "../src/generated/api";

const said = (seq: number, from: "agent" | "you", text: string): FeedEntry => ({
  seq,
  at: "2026-11-20T10:00:00Z",
  atText: "10:00",
  items: [{ kind: "message", from, author: from === "you" ? "You" : "Operations", text }],
});
const offered = (seq: number, replies: string[]): FeedEntry => ({
  seq,
  at: "2026-11-20T10:00:00Z",
  atText: "10:00",
  items: [{ kind: "suggestions", replies, callId: `s${seq}` }],
});

describe("suggested replies", () => {
  it("stand under the agent's latest message, and a tap only offers the words", async () => {
    const onSuggest = vi.fn();
    render(
      <Thread
        entries={[said(1, "agent", "p95 doubled after 09:12."), offered(2, ["What else changed at 09:12?", "Show the error logs"])]}
        open={[]}
        client={new FixtureClient()}
        send={vi.fn()}
        onSettled={vi.fn()}
        onSuggest={onSuggest}
      />,
    );
    const group = screen.getByRole("group", { name: "Suggested replies" });
    expect(within(group).getAllByRole("button").map((button) => button.textContent)).toEqual(["What else changed at 09:12?", "Show the error logs"]);
    await userEvent.click(within(group).getByRole("button", { name: "Show the error logs" }));
    expect(onSuggest).toHaveBeenCalledWith("Show the error logs");
  });

  it("go once the person writes, and only the latest offer shows", () => {
    const { rerender } = render(
      <Thread entries={[offered(1, ["Old"]), said(2, "agent", "Done."), offered(3, ["New"])]} open={[]} client={new FixtureClient()} send={vi.fn()} onSettled={vi.fn()} onSuggest={vi.fn()} />,
    );
    expect(within(screen.getByRole("group", { name: "Suggested replies" })).getAllByRole("button").map((b) => b.textContent)).toEqual(["New"]);
    rerender(
      <Thread entries={[offered(1, ["Old"]), said(2, "agent", "Done."), offered(3, ["New"]), said(4, "you", "Thanks")]} open={[]} client={new FixtureClient()} send={vi.fn()} onSettled={vi.fn()} onSuggest={vi.fn()} />,
    );
    expect(screen.queryByRole("group", { name: "Suggested replies" })).toBeNull();
  });

  it("fill the message box, which sends only on Send", async () => {
    const onSend = vi.fn().mockResolvedValue(undefined);
    const { rerender } = render(<Composer placeholder="Message Operations" queued={[]} onSend={onSend} />);
    rerender(<Composer placeholder="Message Operations" queued={[]} onSend={onSend} draft={{ text: "Show the error logs", n: 1 }} />);
    const box = screen.getByLabelText("Message Operations") as HTMLInputElement;
    expect(box.value).toBe("Show the error logs");
    expect(onSend).not.toHaveBeenCalled();
    await userEvent.click(screen.getByRole("button", { name: "Send" }));
    expect(onSend).toHaveBeenCalledWith("Show the error logs", { kind: "pointer" });
  });
});
