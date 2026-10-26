// The web home's three sections over the example client (spec 3.3 §5.1, §7.4, §8): one header
// with the status line and Stop all, the Decisions badge, typing queued while a decision is
// open, and Today and Memory & rules read-only where their kernel routes do not exist yet.
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { App } from "../src/App";
import { ApiError } from "../src/client";
import { FixtureClient, fixtureSeams } from "../src/fixture/fixture";
import type { DevicesSeam } from "../src/seams";

describe("the header", () => {
  it("names the three sections, the status line, a filled Decisions badge and Stop all", async () => {
    // The status line reads the local clock; noon is outside the fixture's 22:00–07:00 window.
    vi.useFakeTimers({ toFake: ["Date"], now: new Date(2026, 9, 26, 12, 0) });
    try {
      render(<App client={new FixtureClient()} initialSection="threads" />);
      const nav = screen.getByRole("navigation", { name: "Sections" });
      for (const name of ["Threads", "Today", "Memory & rules"]) {
        expect(within(nav).getByRole("link", { name: new RegExp(name) })).toBeTruthy();
      }
      expect(await screen.findByText("$1.84 of $5.00 today")).toBeTruthy();
      expect(screen.getByText("Quiet from 22:00")).toBeTruthy();
      const decisions = await screen.findByRole("heading", { name: /^Decisions/ });
      await waitFor(() => expect(within(decisions).getByText("2").className).toContain("badge"));
      expect(screen.getByRole("button", { name: "Stop all" })).toBeTruthy();
    } finally {
      vi.useRealTimers();
    }
  });

  it("stops every agent in one action, and resumes from the home", async () => {
    const client = new FixtureClient();
    render(<App client={client} initialSection="threads" />);
    await userEvent.click(screen.getByRole("button", { name: "Stop all" }));
    await screen.findByRole("button", { name: "Resume all" });
    expect(client.taken.commands.map(({ command }) => command.type)).toEqual(["stopAll"]);
    await userEvent.click(screen.getByRole("button", { name: "Resume all" }));
    await screen.findByRole("button", { name: "Stop all" });
  });
});

describe("Threads", () => {
  it("shows the roster with status words and the open agent's decision inline", async () => {
    render(<App client={new FixtureClient()} initialSection="threads" />);
    const roster = await screen.findByRole("navigation", { name: "Agents" });
    await within(roster).findByText("Asleep until 07:00");
    expect(within(roster).getAllByText("Waiting for you").length).toBe(2);
    const thread = screen.getByRole("main");
    expect(await within(thread).findByRole("article", { name: "Send the October invoice to [CLIENT]" })).toBeTruthy();
  });

  it("queues what is typed while a decision is open, and Enter never answers it", async () => {
    const client = new FixtureClient();
    render(<App client={client} initialSection="threads" />);
    const thread = screen.getByRole("main");
    await within(thread).findByRole("article", { name: "Send the October invoice to [CLIENT]" });
    const box = within(thread).getByRole("textbox", { name: "Message Finance" });
    await userEvent.type(box, "use the new rate{Enter}");
    await within(thread).findByText("use the new rate");
    expect(within(thread).getAllByText("Sent after this card is answered").length).toBeGreaterThan(0);
    const types = client.taken.commands.map(({ command }) => command.type);
    expect(types).toEqual(["send"]);
    expect(client.taken.commands[0].origin).toEqual({ kind: "keyboard" });
    // The decision is still waiting.
    expect(within(thread).getByRole("button", { name: "Allow once" })).toBeTruthy();
  });

  it("opens a decision from the Decisions list in its agent's thread", async () => {
    render(<App client={new FixtureClient()} initialSection="threads" />);
    const list = await screen.findByRole("region", { name: /^Decisions/ });
    await userEvent.click(await within(list).findByRole("button", { name: /Publish a release/ }));
    const thread = screen.getByRole("main");
    expect(await within(thread).findByRole("article", { name: "Publish a release" })).toBeTruthy();
  });
});

describe("Today", () => {
  it("reads what exists: spent, waiting, the briefing lines and today's routines", async () => {
    render(<App client={new FixtureClient()} initialSection="today" />);
    expect(await screen.findByRole("heading", { name: /While you were away/ })).toBeTruthy();
    expect(await screen.findByText("$0.62")).toBeTruthy();
    expect(await screen.findByText(/The backup to \[DRIVE\] failed/)).toBeTruthy();
    expect(screen.getByRole("heading", { name: "Routines today" })).toBeTruthy();
  });

  it("does not claim quiet time was respected without the route that knows", async () => {
    render(<App client={new FixtureClient()} initialSection="today" />);
    await screen.findByText("$0.62");
    expect(screen.queryByText("Respected")).toBeNull();
  });

  it("says quiet time was respected when the while-you-were-away route says so", async () => {
    render(<App client={new FixtureClient()} seams={fixtureSeams()} initialSection="today" />);
    expect(await screen.findByText("Respected")).toBeTruthy();
  });
});

describe("Memory & rules", () => {
  it("shows the three bordered lists, and the rules allowed without asking can be stopped", async () => {
    const client = new FixtureClient();
    render(<App client={client} initialSection="memory" />);
    expect(await screen.findByRole("heading", { name: "Agent Memory and Rules" })).toBeTruthy();
    for (const name of ["What your agents remember", "What your agents can do", "What your agents did"]) {
      expect(screen.getByRole("heading", { name })).toBeTruthy();
    }
    expect(screen.getByText(/spends money or publishes always asks you first/)).toBeTruthy();
    const stop = await screen.findAllByRole("button", { name: "Stop allowing" });
    const before = (await client.rules()).length;
    await userEvent.click(stop[0]);
    await waitFor(async () => expect((await client.rules()).length).toBe(before - 1));
  });

  it("renders quiet time and the spending limit read-only until their routes exist", async () => {
    render(<App client={new FixtureClient()} initialSection="memory" />);
    await screen.findByText("22:00–07:00");
    expect(screen.queryByRole("button", { name: "Change" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Undo" })).toBeNull();
  });

  it("offers Change and the memory answers through the routes when they exist", async () => {
    render(<App client={new FixtureClient()} seams={fixtureSeams()} initialSection="memory" />);
    expect((await screen.findAllByRole("button", { name: "Change" })).length).toBe(2);
    expect(await screen.findByRole("button", { name: "That's right" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Undo" })).toBeTruthy();
  });
});

/** The example, with a server that can go away. */
class Flaky extends FixtureClient {
  down = false;
  override async status() {
    if (this.down) throw new TypeError("Failed to fetch");
    return super.status();
  }
}

describe("when the server goes away", () => {
  it("says it cannot reach the agents on every section, not the last status it read", async () => {
    const client = new Flaky();
    render(<App client={client} initialSection="memory" />);
    expect(await screen.findByText("Kept on your computer")).toBeTruthy();
    const header = screen.getByRole("banner");
    await within(header).findByRole("button", { name: "Stop all" });
    client.down = true;
    await userEvent.click(within(header).getByRole("button", { name: "Stop all" }));
    expect(await screen.findByText("Can't reach your agents right now. Trying again.")).toBeTruthy();
    expect(screen.queryByText("Failed to fetch")).toBeNull();
  });
});

describe("Today, after a failed read", () => {
  it("clears the problem once the briefing reads again", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      const client = new FixtureClient();
      const briefing = client.briefing.bind(client);
      let fail = true;
      client.briefing = async () => {
        if (fail) throw new TypeError("Failed to fetch");
        return briefing();
      };
      render(<App client={client} initialSection="today" />);
      expect((await screen.findByRole("alert")).textContent).toBe("Can't reach your agents right now. Trying again.");
      fail = false;
      await vi.advanceTimersByTimeAsync(10_000);
      await screen.findByText(/The backup to \[DRIVE\] failed/);
      expect(screen.queryByRole("alert")).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("Where you can say yes from", () => {
  it("says why adding a phone or removing one did not work", async () => {
    const devices: DevicesSeam = {
      list: async () => [{ id: "device:1", name: "Ana's phone", may: "yes or no" }],
      remove: async () => {
        throw new ApiError(403, "removing a paired device needs the operator");
      },
      pair: async () => {
        throw new ApiError(403, "issuing a pairing code needs the operator");
      },
    };
    render(<App client={new FixtureClient()} seams={{ devices }} initialSection="memory" />);
    await userEvent.click(await screen.findByRole("button", { name: "Add a phone or app" }));
    expect((await screen.findByRole("alert")).textContent).toContain("issuing a pairing code needs the operator");
    await userEvent.click(screen.getByRole("button", { name: "Remove" }));
    await waitFor(() => expect(screen.getByRole("alert").textContent).toContain("removing a paired device"));
    expect(screen.getByText("Ana's phone")).toBeTruthy();
  });

  it("shows the pairing QR code, link and code, and sets up a passkey on this computer (spec 3.3 §8.2, §8.3)", async () => {
    let computer = false;
    const qr = vi.fn(async (url: string) => `data:image/svg+xml;charset=utf-8,${encodeURIComponent(`<svg data-url="${url}"/>`)}`);
    const devices: DevicesSeam = {
      list: async () => [
        { id: "device:1", name: "Your phone", may: "asks for your face or fingerprint" },
        ...(computer ? [{ id: "passkey:k1", name: "Face or fingerprint on this computer", may: "asks for your face or fingerprint" }] : []),
      ],
      remove: async () => {
        computer = false;
      },
      pair: async () => ({ code: "ABCD-2345", url: "https://home.example.ts.net/ui/pair#code=ABCD-2345" }),
      qr,
      canAddPasskey: async () => !computer,
      addPasskey: async () => {
        computer = true;
      },
    };
    render(<App client={new FixtureClient()} seams={{ devices }} initialSection="memory" />);
    await userEvent.click(await screen.findByRole("button", { name: "Add a phone or app" }));
    const image = await screen.findByRole("img", { name: /QR code of the pairing link/ });
    expect(qr).toHaveBeenCalledWith("https://home.example.ts.net/ui/pair#code=ABCD-2345");
    expect(decodeURIComponent(image.getAttribute("src") ?? "")).toContain("#code=ABCD-2345");
    expect(screen.getByText("https://home.example.ts.net/ui/pair#code=ABCD-2345")).toBeTruthy();
    expect(screen.getByText("ABCD-2345")).toBeTruthy();

    await userEvent.click(screen.getByRole("button", { name: "Use your face or fingerprint on this computer" }));
    expect(await screen.findByText("Face or fingerprint on this computer")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Use your face or fingerprint on this computer" })).toBeNull();
    const row = screen.getByText("Face or fingerprint on this computer").closest("li")!;
    await userEvent.click(within(row).getByRole("button", { name: "Remove" }));
    expect(await screen.findByRole("button", { name: "Use your face or fingerprint on this computer" })).toBeTruthy();
    expect(screen.queryByText("Face or fingerprint on this computer")).toBeNull();
  });
});

describe("the developer panel", () => {
  it("names its tabs in sentence case", async () => {
    render(<App client={new FixtureClient()} initialSection="developer" />);
    const tabs = await screen.findByRole("navigation", { name: "Developer panel" });
    expect(within(tabs).getAllByRole("button").map((tab) => tab.textContent)).toEqual([
      "Topology",
      "Protocols",
      "Telemetry",
      "State",
    ]);
  });
});

/** The example behind a server started with a token. */
class Locked extends FixtureClient {
  token?: string;
  signIn(token: string) {
    this.token = token;
  }
  override async status() {
    if (this.token !== "t0ken") throw new ApiError(401, "the surface API needs an authenticated caller");
    return super.status();
  }
  override async agents() {
    if (this.token !== "t0ken") throw new ApiError(401, "the surface API needs an authenticated caller");
    return super.agents();
  }
  override async rules() {
    if (this.token !== "t0ken") throw new ApiError(401, "401 Unauthorized");
    return super.rules();
  }
}

describe("a server started with a token", () => {
  it("asks for the token once, then reads with it", async () => {
    const client = new Locked();
    render(<App client={client} initialSection="threads" />);
    expect(await screen.findByText("This page needs you signed in to the server it came from.")).toBeTruthy();
    await userEvent.type(screen.getByLabelText("The token the server was started with"), "t0ken");
    await userEvent.click(screen.getByRole("button", { name: "Sign in" }));
    expect(await screen.findByText("$1.84 of $5.00 today")).toBeTruthy();
    await waitFor(() => expect(screen.queryByText("This page needs you signed in to the server it came from.")).toBeNull());
  });
});

describe("a server started with a token, on Memory & rules", () => {
  it("reads the rules again once signed in, without the earlier problem", async () => {
    const client = new Locked();
    render(<App client={client} initialSection="memory" />);
    expect(await screen.findByText("401 Unauthorized")).toBeTruthy();
    await userEvent.type(screen.getByLabelText("The token the server was started with"), "t0ken");
    await userEvent.click(screen.getByRole("button", { name: "Sign in" }));
    expect((await screen.findAllByRole("button", { name: "Stop allowing" })).length).toBeGreaterThan(0);
    expect(screen.queryByText("401 Unauthorized")).toBeNull();
  });
});
