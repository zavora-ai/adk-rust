// Agents a person adds on the web home (spec 3.3 §5.7): the roster's New agent slot, the New
// agent screen with both tabs and the live preview, an agent's settings with "Was … Now …",
// Copy one of your rules, Pause and the remove confirmation, the definition on a card, in the
// home and on the phone, and every agent in Memory & rules.
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { AgentCard } from "../src/agents/AgentCard";
import { AgentSettings } from "../src/agents/AgentSettings";
import { agentsPlaceOf, newAgentPath } from "../src/agents/route";
import { NewAgent } from "../src/agents/NewAgent";
import { RosterFoot } from "../src/agents/RosterFoot";
import { App } from "../src/App";
import { FixtureClient, fixtureSeams } from "../src/fixture/fixture";
import { FixtureAgents, suggestionCard, withAgents } from "../src/fixture/agents";
import { FixturePhoneClient } from "../src/fixture/phone";
import { Decide } from "../src/phone/Decide";
import type { AgentRow } from "../src/generated/api";

const chief: AgentRow = {
  agent: { name: "adk-rust", displayName: "ADK-Rust", avatar: { monogram: "A", color: "#C03A00" } },
  status: "working",
  statusText: "Working",
};

describe("the roster's foot", () => {
  it("offers New agent and says no agent of your own is there yet", () => {
    render(
      <ul>
        <RosterFoot rows={[chief]} />
      </ul>,
    );
    expect(screen.getByText("No agents of your own yet. Add one, or ask ADK-Rust to suggest one.")).toBeTruthy();
    expect(screen.getByRole("button", { name: "New agent" })).toBeTruthy();
  });

  it("drops the empty text once the person has an agent, and opens the New agent screen", async () => {
    const seam = new FixtureAgents();
    render(
      <ul>
        <RosterFoot rows={[chief, ...seam.rows()]} />
      </ul>,
    );
    expect(screen.queryByText(/No agents of your own yet/)).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "New agent" }));
    expect(window.location.pathname).toBe("/ui/agents/new");
    expect(agentsPlaceOf(window.location)).toEqual({ kind: "new" });
  });

  it("is absent where agents cannot be added", () => {
    const { container } = render(
      <ul>
        <RosterFoot rows={[chief]} canAdd={false} />
      </ul>,
    );
    expect(container.querySelector("li")).toBeNull();
  });
});

describe("the New agent screen", () => {
  it("describes it in a thread with ADK-Rust", async () => {
    const onDescribe = vi.fn();
    render(<NewAgent seam={new FixtureAgents(false)} onDescribe={onDescribe} onAdded={vi.fn()} onCancel={vi.fn()} />);
    expect(screen.getByRole("tab", { name: "Describe it", selected: true })).toBeTruthy();
    const ask = screen.getByRole("button", { name: "Ask ADK-Rust to suggest one" }) as HTMLButtonElement;
    expect(ask.disabled).toBe(true);
    await userEvent.type(screen.getByLabelText("What should it do?"), "Read my invoices and draft reminders");
    await userEvent.click(ask);
    expect(onDescribe).toHaveBeenCalledWith(
      "Suggest a new agent for this: Read my invoices and draft reminders",
      expect.objectContaining({ kind: expect.any(String) }),
    );
  });

  it("sets it up by hand beside a live preview, and adds it with the preview's digest", async () => {
    const seam = new FixtureAgents(false);
    const onAdded = vi.fn();
    render(<NewAgent seam={seam} onDescribe={vi.fn()} onAdded={onAdded} onCancel={vi.fn()} />);
    await userEvent.click(screen.getByRole("tab", { name: "Set it up yourself" }));
    await userEvent.type(await screen.findByLabelText("Name"), "Finance");
    const preview = await screen.findByRole("complementary", { name: "Here is what Finance can do" });
    await within(preview).findByText("It asks you first before it changes anything.");
    expect(within(preview).getByText("Medium risk")).toBeTruthy();

    // The preview follows the form.
    const files = screen.getByRole("radiogroup", { name: "Files" });
    await userEvent.click(within(files).getByRole("radio", { name: "Read" }));
    await userEvent.type(screen.getByLabelText("Folder"), "Invoices");
    await within(preview).findByText("It can read files in Invoices.");

    // Go ahead is looser than Home's Ask first, so it cannot be chosen there.
    const mode = screen.getByRole("radiogroup", { name: "How much it asks first" });
    expect((within(mode).getByRole("radio", { name: "Go ahead" }) as HTMLButtonElement).disabled).toBe(true);

    const add = screen.getByRole("button", { name: "Add Finance" }) as HTMLButtonElement;
    await waitFor(() => expect(add.disabled).toBe(false));
    await userEvent.click(add);
    await waitFor(() => expect(onAdded).toHaveBeenCalled());
    const sent = seam.taken.at(-1)!.command;
    if (sent.type !== "addAgent") throw new Error(`sent ${sent.type}`);
    const shown = await seam.preview({ action: "create", draft: sent.draft });
    expect(sent.digest).toBe(shown.digest);
    expect(onAdded.mock.calls[0][0].id).toBe("finance");
  });

  it("holds a suggestion for Change it first, replacing it", async () => {
    const seam = new FixtureAgents(false);
    const card = suggestionCard();
    render(
      <NewAgent
        seam={seam}
        initial={card.addsAgent.draft}
        replaces={card.addsAgent.id}
        onDescribe={vi.fn()}
        onAdded={vi.fn()}
        onCancel={vi.fn()}
      />,
    );
    expect(screen.getByRole("tab", { name: "Set it up yourself", selected: true })).toBeTruthy();
    expect(((await screen.findByLabelText("Name")) as HTMLInputElement).value).toBe("Finance");
    const add = screen.getByRole("button", { name: "Add Finance" }) as HTMLButtonElement;
    await waitFor(() => expect(add.disabled).toBe(false));
    await userEvent.click(add);
    await waitFor(() => expect(seam.taken.length).toBe(1));
    expect(seam.taken[0].command).toMatchObject({ type: "addAgent", replaces: "finance" });
  });
});

describe("an agent's settings", () => {
  it("says how it was added and shows Was … Now … before saving", async () => {
    const seam = new FixtureAgents();
    const onChanged = vi.fn();
    render(<AgentSettings seam={seam} id="finance" onChanged={onChanged} />);
    expect(await screen.findByText("Suggested by ADK-Rust, added by you on 25 Oct")).toBeTruthy();
    const name = (await screen.findByLabelText("Name")) as HTMLInputElement;
    await userEvent.clear(name);
    await userEvent.type(name, "Bills");
    const mode = screen.getByRole("radiogroup", { name: "How much it asks first" });
    await userEvent.click(within(mode).getByRole("radio", { name: "Suggest" }));
    const wasNow = await screen.findByLabelText("Was · Now");
    expect(within(wasNow).getByText(/Finance/)).toBeTruthy();
    expect(within(wasNow).getByText(/Bills/)).toBeTruthy();
    const save = screen.getByRole("button", { name: "Save changes" }) as HTMLButtonElement;
    await waitFor(() => expect(save.disabled).toBe(false));
    await userEvent.click(save);
    await waitFor(() => expect(onChanged).toHaveBeenCalled());
    expect(seam.details.get("finance")!.draft).toMatchObject({ displayName: "Bills", mode: "suggest" });
  });

  it("shows who changed it first instead of overwriting", async () => {
    const seam = new FixtureAgents();
    render(<AgentSettings seam={seam} id="finance" />);
    const name = (await screen.findByLabelText("Name")) as HTMLInputElement;
    // The terminal changes it meanwhile.
    const current = seam.details.get("finance")!;
    seam.details.set("finance", { ...current, version: current.version + 1 });
    await userEvent.type(name, "!");
    const save = screen.getByRole("button", { name: "Save changes" }) as HTMLButtonElement;
    await waitFor(() => expect(save.disabled).toBe(false));
    await userEvent.click(save);
    expect(await screen.findByText("Finance was changed in the terminal at 10:01")).toBeTruthy();
  });

  it("copies one of your rules to it", async () => {
    const seam = new FixtureAgents();
    render(<AgentSettings seam={seam} id="finance" />);
    const select = await screen.findByRole("combobox", { name: "Your rules" });
    await userEvent.selectOptions(select, "r-read");
    await userEvent.click(screen.getByRole("button", { name: "Copy one of your rules" }));
    expect(await screen.findByText("Copied: Reading files in this workspace")).toBeTruthy();
    expect(seam.taken.at(-1)!.command).toEqual({ type: "copyRuleToAgent", agent: "finance", ruleId: "r-read" });
  });

  it("pauses and resumes it", async () => {
    const seam = new FixtureAgents();
    render(<AgentSettings seam={seam} id="finance" />);
    await userEvent.click(await screen.findByRole("button", { name: "Pause" }));
    expect(await screen.findByRole("button", { name: "Resume" })).toBeTruthy();
    expect(screen.getByText("Paused")).toBeTruthy();
  });

  it("names what removing it ends, and Keep changes nothing", async () => {
    const seam = new FixtureAgents();
    seam.running.finance = { work: 1, decisions: 2, routines: 1 };
    const onRemoved = vi.fn();
    render(<AgentSettings seam={seam} id="finance" onRemoved={onRemoved} />);
    await userEvent.click(await screen.findByRole("button", { name: "Remove" }));
    const dialog = await screen.findByRole("alertdialog", { name: "Remove Finance?" });
    expect(
      within(dialog).getByText(
        "Removing Finance stops 1 piece of work, ends 2 decisions waiting for you and 1 routine. Its threads stay.",
      ),
    ).toBeTruthy();
    await userEvent.click(within(dialog).getByRole("button", { name: "Keep" }));
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(seam.details.has("finance")).toBe(true);
    await userEvent.click(screen.getByRole("button", { name: "Remove" }));
    await userEvent.click(within(await screen.findByRole("alertdialog")).getByRole("button", { name: "Remove" }));
    await waitFor(() => expect(onRemoved).toHaveBeenCalledWith("Finance"));
    expect(seam.details.has("finance")).toBe(false);
  });

  it("shows only the mode and rules of the primary agent", async () => {
    render(
      <AgentSettings
        seam={new FixtureAgents()}
        primary={{ agent: chief.agent, mode: { mode: "askFirst", label: "Ask first", status: "Asks first" } }}
      />,
    );
    expect(screen.getByText("Ask first")).toBeTruthy();
    expect(screen.queryByLabelText("Name")).toBeNull();
    expect(screen.getByText("Its name, instructions and model are set where ADK-Rust was started.")).toBeTruthy();
  });
});

describe("the decision that adds an agent", () => {
  it("shows the definition in full and Change it first opens the form", async () => {
    const card = suggestionCard();
    render(<AgentCard view={card.addsAgent} decisionId={card.id} expanded={false} onExpand={vi.fn()} />);
    expect(screen.getByText("Reads files in Invoices")).toBeTruthy();
    expect(screen.getByText("claude-sonnet-5-5").className).toContain("mono");
    expect(screen.getByText("It can do less than ADK-Rust: no shell, no web.")).toBeTruthy();
    expect(screen.getByText("Nothing yet")).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Change it first" }));
    expect(`${window.location.pathname}${window.location.search}`).toBe(newAgentPath(card.id));
  });

  it("needs long instructions expanded before they are shown in full", async () => {
    const card = suggestionCard();
    const view = { ...card.addsAgent, instructions: "Short…", instructionsTruncated: true };
    const onExpand = vi.fn();
    const { rerender } = render(<AgentCard view={view} decisionId={card.id} expanded={false} onExpand={onExpand} />);
    expect(screen.getByText("Short…")).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Show all the instructions" }));
    expect(onExpand).toHaveBeenCalled();
    rerender(<AgentCard view={view} decisionId={card.id} expanded onExpand={onExpand} />);
    expect(screen.getByText(card.addsAgent.draft.instructions)).toBeTruthy();
  });

  it("escapes the names and instructions it shows", () => {
    const card = suggestionCard();
    const view = { ...card.addsAgent, instructions: "Read‮evil" };
    render(<AgentCard view={view} decisionId={card.id} expanded={false} onExpand={vi.fn()} />);
    expect(screen.getByText("Read\\u{202e}evil")).toBeTruthy();
  });
});

describe("the web home's hooks", () => {
  it("draws a suggested agent's card with its definition in place of the call", async () => {
    const { DecisionCard } = await import("../src/components/DecisionCard");
    const card = suggestionCard();
    render(<DecisionCard card={card} send={vi.fn()} />);
    const article = screen.getByRole("article", { name: "Add an agent called Finance" });
    expect(within(article).getByText("Reads files in Invoices")).toBeTruthy();
    expect(within(article).queryByText(/kernel:agent\.create\(/)).toBeNull();
    expect(within(article).getByRole("button", { name: "Add this agent" })).toBeTruthy();
    expect(within(article).getByRole("button", { name: "Don't add" })).toBeTruthy();
    expect(within(article).queryByRole("button", { name: /Always allow/ })).toBeNull();
    expect(within(article).getByRole("button", { name: "Change it first" })).toBeTruthy();
  });

  it("ends the roster with New agent and offers Settings on each row", async () => {
    const { Roster } = await import("../src/components/Roster");
    const seam = new FixtureAgents();
    render(<Roster agents={[chief, ...seam.rows()]} onSelect={vi.fn()} canAdd />);
    expect(screen.getByRole("button", { name: "New agent" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Settings: Finance" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Settings: ADK-Rust" })).toBeTruthy();
  });

  it("leaves the roster as it was where agents cannot be added", async () => {
    const { Roster } = await import("../src/components/Roster");
    render(<Roster agents={[chief]} onSelect={vi.fn()} />);
    expect(screen.queryByRole("button", { name: "New agent" })).toBeNull();
    expect(screen.queryByRole("button", { name: /Settings/ })).toBeNull();
  });
});

describe("the phone", () => {
  it("shows a suggested agent's definition, Add this agent and Don't add, and no Change it first", async () => {
    const client = new FixturePhoneClient();
    const card = suggestionCard();
    client.cards = [card];
    render(<Decide client={client} id={card.id} onBack={() => undefined} onSettled={() => undefined} />);
    expect(await screen.findByText("Reads files in Invoices")).toBeTruthy();
    expect(screen.queryByText(/kernel:agent\.create\(/)).toBeNull();
    expect(screen.getByRole("button", { name: "Add this agent" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Don't add" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Change it first" })).toBeNull();
  });
});

describe("Memory & rules", () => {
  it("lists every agent with its area, how much it asks first, and Settings", async () => {
    window.history.replaceState(null, "", "/ui/memory");
    render(<App client={withAgents(new FixtureClient())} seams={fixtureSeams()} initialSection="memory" />);
    const list = await screen.findByRole("list", { name: "Your agents" });
    const finance = within(list)
      .getAllByRole("listitem")
      .find((item) => within(item).queryByText("Finance") && within(item).queryByText(/Home · Asks first/));
    expect(finance).toBeTruthy();
    expect(within(list).getByRole("button", { name: "Settings: Chief of staff" })).toBeTruthy();
    await userEvent.click(within(finance!).getByRole("button", { name: "Settings: Finance" }));
    expect(window.location.pathname).toBe("/ui/agents/finance");
  });

  it("lists no agents where the home keeps none", async () => {
    render(<App client={new FixtureClient()} seams={fixtureSeams()} initialSection="memory" />);
    await screen.findByRole("heading", { name: "What your agents can do" });
    expect(screen.queryByRole("list", { name: "Your agents" })).toBeNull();
  });
});

