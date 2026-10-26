// Decisions on the web home (spec 3.3 §7.1, §7.2, §7.4, §11 S-I2, S-I4): one write path with
// the version seen, "already decided" from the conflict, Enter and Esc never decide, typing
// while a decision is open is queued.
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { DecisionCard } from "../src/components/DecisionCard";
import type { Command, CommandOutcome, DecisionCard as Card, Origin } from "../src/generated/api";
import { ApiError } from "../src/client";
import { FixtureClient, invoiceCard, releaseCard } from "../src/fixture/fixture";

/** A take-over entry as the engine draws it: the tool that asks, the kind, the resource. */
function takeOverCard(): Card {
  return {
    ...invoiceCard(),
    id: "apr_code",
    version: 3,
    heading: "Type the code your bank sent",
    call: {
      tool: "harness:take-over",
      args: { takeOver: true, tool: "bank_sign_in", secret: "oneTimeCode", resource: "https://bank.example.com/login" },
      text: "harness:take-over(…)",
      truncated: false,
    },
    allowed: { options: [{ answer: "deny", label: "Deny", needsPresence: false, needsExpanded: false }], openInHome: false },
  };
}

function sendThrough(client: FixtureClient) {
  return vi.fn((command: Command, origin: Origin): Promise<CommandOutcome> => client.command(command, origin));
}

describe("a decision card", () => {
  it("adds a feature's line under the details, linking only into the home (spec 3.3 §7.1)", () => {
    const card: Card = {
      ...releaseCard(),
      detail: { text: "From the invoice-run workflow", linkText: "Open in Runs", href: "/ui/runs/r-1" },
    };
    const { rerender } = render(<DecisionCard card={card} send={vi.fn()} />);
    const line = screen.getByText(/From the invoice-run workflow/);
    expect(line.textContent).toBe("From the invoice-run workflow · Open in Runs");
    expect(within(line).getByRole("link", { name: "Open in Runs" }).getAttribute("href")).toBe("/ui/runs/r-1");

    for (const href of ["https://example.com/runs", "//example.com/runs", "javascript:alert(1)"]) {
      rerender(<DecisionCard card={{ ...card, id: href, detail: { ...card.detail!, href } }} send={vi.fn()} />);
      expect(screen.getByText(/From the invoice-run workflow/).textContent).toBe("From the invoice-run workflow");
      expect(screen.queryByRole("link", { name: "Open in Runs" })).toBeNull();
    }

    rerender(<DecisionCard card={{ ...releaseCard(), id: "plain" }} send={vi.fn()} />);
    expect(screen.queryByText(/From the invoice-run workflow/)).toBeNull();
  });

  it("draws the anatomy of §7.1 on its own background, without the entry's id or version", () => {
    const card = invoiceCard();
    const { container } = render(<DecisionCard card={card} send={vi.fn()} />);
    const article = screen.getByRole("article", { name: card.heading });
    expect(article.className).toContain("decision");
    expect(within(article).getByText(/since 18:00/)).toBeTruthy();
    expect(within(article).getAllByText("Waiting for you").length).toBeGreaterThan(0);
    expect(within(article).getByText("send_email")).toBeTruthy();
    expect(within(article).getByText("Waited through quiet time")).toBeTruthy();
    for (const label of ["Allow once", "Always allow for this person", "Edit", "Deny"]) {
      expect(within(article).getByRole("button", { name: label })).toBeTruthy();
    }
    expect(within(article).getByText("Medium risk")).toBeTruthy();
    expect(within(article).getByText("Can't be unsent")).toBeTruthy();
    expect(container.textContent).not.toContain(card.id);
  });

  // The owner's 07:03 demo: a routine an agent proposes reads as its title and the engine's
  // plain summary of who suggests it and when it runs (spec 3.3 §7.1, §8.1).
  it("says in plain words what a decision the kernel asks is about", () => {
    const card: Card = {
      ...invoiceCard(),
      heading: "Check the news every 5 minutes",
      summary: "Finance suggests this. Once you say yes, it runs every 5 minutes.",
      call: {
        tool: "kernel:agenda",
        args: { title: "Check the news every 5 minutes" },
        text: 'kernel:agenda(title: "Check the news every 5 minutes")',
        truncated: false,
      },
    };
    render(<DecisionCard card={card} send={vi.fn()} />);
    const article = screen.getByRole("article", { name: card.heading });
    expect(within(article).getByText(card.summary!)).toBeTruthy();
  });

  // S-I9: a bidi override or a zero-width space from a tool's arguments would make the call read
  // differently from what runs; the card shows each as its escape, as the terminal does.
  it("shows bidi and zero-width characters from the call as escapes, and edits them unchanged", async () => {
    const args = { to: "billing@corp\u200b.com", attachment: "invoice_\u202efdp.exe\u202c" };
    const card: Card = {
      ...invoiceCard(),
      heading: "Send invoice_\u202efdp.exe to billing@corp\u200b.com",
      call: { tool: "send_email", args, text: `send_email(to: "billing@corp\u200b.com")`, truncated: false },
      timeline: [{ text: "Asked by \u2066Finance\u2069 at 18:00", current: true }],
    };
    const send = vi.fn(async (): Promise<CommandOutcome> => ({ type: "queued", text: "" }) as unknown as CommandOutcome);
    const { container } = render(<DecisionCard card={card} send={send} />);
    const text = container.textContent ?? "";
    for (const hidden of ["\u200b", "\u202e", "\u202c", "\u2066", "\u2069"]) expect(text).not.toContain(hidden);
    expect(text).toContain('"billing@corp\\u{200b}.com"');
    expect(text).toContain('"invoice_\\u{202e}fdp.exe\\u{202c}"');
    expect(text).toContain("Send invoice_\\u{202e}fdp.exe to billing@corp\\u{200b}.com");
    expect(text).toContain("Asked by \\u{2066}Finance\\u{2069} at 18:00");

    await userEvent.click(screen.getByRole("button", { name: "Edit" }));
    const field = screen.getByRole("textbox") as HTMLTextAreaElement;
    expect(field.value).not.toContain("\u202e");
    expect(field.value).toContain("\\u202e");
    expect(JSON.parse(field.value)).toEqual(args);
  });

  it("answers through decide with the version it showed", async () => {
    const client = new FixtureClient();
    const send = sendThrough(client);
    render(<DecisionCard card={invoiceCard()} send={send} />);
    await userEvent.click(screen.getByRole("button", { name: "Allow once" }));
    expect(send).toHaveBeenCalledTimes(1);
    expect(send.mock.calls[0][0]).toEqual({
      type: "decide",
      decisionId: "apr_invoice",
      version: 1,
      answer: "allowOnce",
      expanded: true,
    });
    expect(send.mock.calls[0][1]).toEqual({ kind: "pointer" });
    // The card is one line with the answer; opened, the answer is the last step of how it got
    // here (§8.1 "Decided").
    const line = await screen.findByRole("button", { expanded: false });
    expect(within(line).getByText("You said yes in the home app at 07:40")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Allow once" })).toBeNull();
    await userEvent.click(line);
    const steps = screen.getByRole("list", { name: "How it got here" });
    expect(within(steps).getByText("You said yes in the home app at 07:40").getAttribute("aria-current")).toBe("step");
  });

  it("shows an answer given elsewhere first, and never retries it", async () => {
    const client = new FixtureClient();
    client.elsewhere.add("apr_invoice");
    const send = sendThrough(client);
    render(<DecisionCard card={invoiceCard()} send={send} />);
    await userEvent.click(screen.getByRole("button", { name: "Deny" }));
    const line = await screen.findByRole("button", { expanded: false });
    expect(within(line).getByText("You said yes in Telegram at 07:31")).toBeTruthy();
    await userEvent.click(line);
    const steps = screen.getByRole("list", { name: "How it got here" });
    expect(within(steps).getByText("You said yes in Telegram at 07:31").getAttribute("aria-current")).toBe("step");
    expect(screen.getByText("Decided")).toBeTruthy();
    expect(send).toHaveBeenCalledTimes(1);
  });

  it("never decides on Enter or Esc", async () => {
    const send = vi.fn();
    render(<DecisionCard card={invoiceCard()} send={send} />);
    const allow = screen.getByRole("button", { name: "Allow once" });
    allow.focus();
    await userEvent.keyboard("{Enter}");
    await userEvent.keyboard("{Escape}");
    screen.getByRole("button", { name: "Deny" }).focus();
    await userEvent.keyboard("{Enter}");
    expect(send).not.toHaveBeenCalled();
  });

  it("answers to the space bar as the keyboard", async () => {
    const client = new FixtureClient();
    const send = sendThrough(client);
    render(<DecisionCard card={invoiceCard()} send={send} />);
    screen.getByRole("button", { name: "Allow once" }).focus();
    await userEvent.keyboard(" ");
    await waitFor(() => expect(send).toHaveBeenCalledTimes(1));
    expect(send.mock.calls[0][1]).toEqual({ kind: "keyboard" });
  });

  it("holds the answers of a truncated call until it is shown in full", async () => {
    const card: Card = {
      ...invoiceCard(),
      call: { ...invoiceCard().call, truncated: true },
      allowed: {
        ...invoiceCard().allowed,
        options: invoiceCard().allowed.options.map((option) =>
          option.answer === "deny" ? option : { ...option, needsExpanded: true },
        ),
      },
    };
    const send = vi.fn();
    render(<DecisionCard card={card} send={send} />);
    expect((screen.getByRole("button", { name: "Allow once" }) as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByRole("button", { name: "Deny" }) as HTMLButtonElement).disabled).toBe(false);
  });

  it("edits the arguments, checks them, and Esc closes the editor without deciding", async () => {
    const client = new FixtureClient();
    const send = sendThrough(client);
    render(<DecisionCard card={invoiceCard()} send={send} />);
    await userEvent.click(screen.getByRole("button", { name: "Edit" }));
    const area = screen.getByRole("textbox");
    await userEvent.keyboard("{Escape}");
    expect(screen.queryByRole("textbox")).toBeNull();
    expect(send).not.toHaveBeenCalled();

    await userEvent.click(screen.getByRole("button", { name: "Edit" }));
    const edit = screen.getByRole("textbox") as HTMLTextAreaElement;
    expect(area).not.toBe(edit);
    await userEvent.clear(edit);
    await userEvent.click(edit);
    await userEvent.paste('{"to": "[accountant email]"}');
    await userEvent.click(screen.getByRole("button", { name: /allow/i }));
    expect(send.mock.calls[0][0]).toMatchObject({ type: "decide", answer: "edit", editedArgs: { to: "[accountant email]" } });
  });

  it("answers with the version the person saw when a newer one arrives, and shows the change", async () => {
    const client = new FixtureClient();
    const send = sendThrough(client);
    const seen = invoiceCard();
    const { rerender } = render(<DecisionCard card={seen} send={send} />);
    // Another approver changed the call: the feed sends version 2, which looks the same.
    rerender(<DecisionCard card={{ ...seen, version: 2 }} send={send} />);
    client.hold({ ...seen, version: 2 });
    await userEvent.click(screen.getByRole("button", { name: "Allow once" }));
    expect(send.mock.calls[0][0]).toMatchObject({ type: "decide", version: 1 });
    expect(await screen.findByText("This decision changed since you saw it. Check it again.")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Allow once" })).toBeTruthy();
  });

  it("shows what a take over asks for, and sends what the person typed to the call that asked", async () => {
    const card = takeOverCard();
    const provide = vi.fn(
      async (shown: Card, _value: string, _origin: Origin): Promise<CommandOutcome> => ({
        type: "answered",
        card: { ...shown, version: shown.version + 1, status: "decided", statusText: "Decided", allowed: { options: [], openInHome: false } },
      }),
    );
    const send = vi.fn();
    render(<DecisionCard card={card} send={send} takeOver={{ provide }} />);
    const article = screen.getByRole("article", { name: "Type the code your bank sent" });
    expect(within(article).getByText("bank_sign_in")).toBeTruthy();
    expect(within(article).getByText("https://bank.example.com/login")).toBeTruthy();
    expect(within(article).queryByText("harness:take-over")).toBeNull();
    expect(within(article).queryByRole("button", { name: "Allow once" })).toBeNull();
    await userEvent.click(within(article).getByRole("button", { name: "Take over" }));
    const field = within(article).getByPlaceholderText("Type it here");
    await userEvent.type(field, "731905{Enter}");
    expect(provide).not.toHaveBeenCalled();
    await userEvent.click(within(article).getByRole("button", { name: "Done" }));
    expect(provide).toHaveBeenCalledTimes(1);
    expect(provide.mock.calls[0][0]).toMatchObject({ id: "apr_code", version: 3 });
    expect(provide.mock.calls[0][1]).toBe("731905");
    await within(article).findByText("Decided");
    expect(send).not.toHaveBeenCalled();
  });

  it("says why a take over did not go through, and keeps the card open", async () => {
    const provide = vi.fn(async (): Promise<CommandOutcome> => {
      throw new ApiError(404, "no tool call in this process waits for take over apr_code");
    });
    render(<DecisionCard card={takeOverCard()} send={vi.fn()} takeOver={{ provide }} />);
    await userEvent.click(screen.getByRole("button", { name: "Take over" }));
    await userEvent.type(screen.getByPlaceholderText("Type it here"), "731905");
    await userEvent.click(screen.getByRole("button", { name: "Done" }));
    expect((await screen.findByRole("alert")).textContent).toContain("no tool call in this process waits");
    expect(screen.getByRole("button", { name: "Deny" })).toBeTruthy();
  });

  it("shows why a high-risk answer needs the person to confirm it is them", async () => {
    const client = new FixtureClient();
    render(<DecisionCard card={releaseCard()} send={sendThrough(client)} />);
    await userEvent.click(screen.getByRole("button", { name: "Confirm it's you and approve" }));
    expect((await screen.findByRole("alert")).textContent).toContain("confirm it's you first");
  });
});
