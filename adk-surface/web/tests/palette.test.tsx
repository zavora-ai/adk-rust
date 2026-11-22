// The command palette (⌘K): find a section, an agent's thread, an open decision or an action,
// and go. It opens a decision's card and never answers one (spec 3.3 §7.4).
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { App } from "../src/App";
import { score } from "../src/components/CommandPalette";
import { FixtureClient } from "../src/fixture/fixture";

async function opened() {
  const client = new FixtureClient();
  const command = vi.spyOn(client, "command");
  render(<App client={client} initialSection="today" />);
  await screen.findByRole("navigation", { name: "Sections" });
  fireEvent.keyDown(window, { key: "k", metaKey: true });
  const palette = await screen.findByRole("dialog", { name: "Search and jump" });
  return { client, command, palette, search: within(palette).getByRole("combobox") };
}

describe("finding things", () => {
  it("ranks word starts over inner matches over letters in order", () => {
    expect(score("cod", "Code's thread")).toBe(3);
    expect(score("thread code", "Code's thread")).toBe(3);
    expect(score("ode", "Code's thread")).toBe(2);
    expect(score("cth", "Code's thread")).toBe(1);
    expect(score("zz", "Code's thread")).toBe(0);
    expect(score("", "anything")).toBe(1);
  });
});

describe("the command palette", () => {
  it("opens with ⌘K and goes to an agent's thread", async () => {
    const { palette, search } = await opened();
    await userEvent.type(search, "code");
    const first = within(palette).getAllByRole("option")[0];
    expect(first.textContent).toContain("Code's thread");
    expect(first.getAttribute("aria-selected")).toBe("true");
    await userEvent.keyboard("{Enter}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Search and jump" })).toBeNull());
    expect(await screen.findByRole("heading", { name: "Code" })).toBeTruthy();
    expect(screen.getByText("Commands run in the sandbox (Seatbelt)")).toBeTruthy();
  });

  it("opens a decision's card and never answers it", async () => {
    const { palette, search, command } = await opened();
    const decisions = within(palette).getByRole("group", { name: "Decisions waiting for you" });
    expect(within(decisions).getAllByRole("option").map((option) => option.querySelector(".palette-label")?.textContent)).toEqual([
      "Send the October invoice to [CLIENT]",
      "Publish a release",
    ]);
    await userEvent.type(search, "publish");
    await userEvent.keyboard("{Enter}");
    expect(await screen.findByRole("heading", { name: "Publish a release" })).toBeTruthy();
    expect(command.mock.calls.some(([sent]) => sent.type === "decide")).toBe(false);
  });

  it("stops every agent from the keyboard", async () => {
    const { search, command } = await opened();
    await userEvent.type(search, "stop");
    await userEvent.keyboard("{Enter}");
    await waitFor(() => expect(command).toHaveBeenCalledWith({ type: "stopAll" }, { kind: "keyboard" }));
  });

  it("moves with the arrows, says when nothing matches, and closes with Escape", async () => {
    const { palette, search } = await opened();
    await userEvent.keyboard("{ArrowDown}");
    expect(within(palette).getAllByRole("option")[1].getAttribute("aria-selected")).toBe("true");
    await userEvent.type(search, "zzzz");
    expect(within(palette).getByText("Nothing matches.")).toBeTruthy();
    await userEvent.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Search and jump" })).toBeNull());
  });

  it("opens from the header too", async () => {
    render(<App client={new FixtureClient()} initialSection="today" />);
    await userEvent.click(await screen.findByRole("button", { name: "Search" }));
    expect(await screen.findByRole("dialog", { name: "Search and jump" })).toBeTruthy();
  });
});
