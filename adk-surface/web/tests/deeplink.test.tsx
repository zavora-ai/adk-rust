// A decision's deep link (spec 3.3 §7.2): the tray, the phone and voice open the home at
// `/ui/decisions/<id>` for the answers only the home gives. The home opens the decision in its
// thread once, and the address becomes the thread's.
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";
import { App } from "../src/App";
import { ApiError } from "../src/client";
import { FixtureClient } from "../src/fixture/fixture";
import { decisionOf, decisionPath, sectionOf, shellOf } from "../src/route";

afterEach(() => window.history.replaceState(null, "", "/"));

describe("the address of a decision", () => {
  it("names the decision, dots and escapes included, and nothing else", () => {
    expect(decisionOf("/ui/decisions/apr_release")).toBe("apr_release");
    expect(decisionOf("/ui/decisions/urn%3Aadk%3Aapr.v1")).toBe("urn:adk:apr.v1");
    expect(decisionOf(decisionPath("a/b.c"))).toBe("a/b.c");
    for (const path of ["/ui/", "/ui/decisions/", "/ui/today", "/decisions/apr_release", "/ui/decisions/%E0%A4%A"]) {
      expect(decisionOf(path)).toBeUndefined();
    }
    // A deep link is the home, on Threads.
    expect(sectionOf("/ui/decisions/apr_release")).toBe("threads");
    expect(shellOf({ pathname: "/ui/decisions/apr_release" })).toBe("home");
    expect(shellOf({ pathname: "/ui/" })).toBe("home");
  });
});

describe("the home opened at a decision", () => {
  it("opens it in its agent's thread and moves the address to the thread", async () => {
    window.history.replaceState(null, "", "/ui/decisions/apr_release");
    render(<App client={new FixtureClient()} />);
    const thread = screen.getByRole("main");
    expect(await within(thread).findByRole("article", { name: "Publish a release" })).toBeTruthy();
    await waitFor(() => expect(window.location.pathname).toBe("/ui/"));
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("says who answered a decision already decided", async () => {
    const client = new FixtureClient();
    const card = await client.decision("apr_invoice");
    await client.command(
      { type: "decide", decisionId: card.id, version: card.version, answer: "allowOnce", expanded: true },
      { kind: "pointer" },
    );
    window.history.replaceState(null, "", "/ui/decisions/apr_invoice");
    render(<App client={client} />);
    expect((await screen.findByRole("alert")).textContent).toBe("You said yes in the home app at 07:40");
    await waitFor(() => expect(window.location.pathname).toBe("/ui/"));
  });

  it("shows the problem line for a decision that does not exist", async () => {
    window.history.replaceState(null, "", "/ui/decisions/missing.v1");
    render(<App client={new FixtureClient()} />);
    expect((await screen.findByRole("alert")).textContent).toBe("decision 'missing.v1' not found");
    await waitFor(() => expect(window.location.pathname).toBe("/ui/"));
  });

  it("waits for the server's token, then opens it", async () => {
    window.history.replaceState(null, "", "/ui/decisions/apr_release");
    const client = new Locked();
    render(<App client={client} />);
    await screen.findByText("This page needs you signed in to the server it came from.");
    expect(window.location.pathname).toBe("/ui/decisions/apr_release");
    await userEvent.type(screen.getByLabelText("The token the server was started with"), "t0ken");
    await userEvent.click(screen.getByRole("button", { name: "Sign in" }));
    const thread = screen.getByRole("main");
    expect(await within(thread).findByRole("article", { name: "Publish a release" })).toBeTruthy();
    await waitFor(() => expect(window.location.pathname).toBe("/ui/"));
  });
});

/** The example behind a server started with a token. */
class Locked extends FixtureClient {
  token?: string;
  signIn(token: string) {
    this.token = token;
  }
  private check() {
    if (this.token !== "t0ken") throw new ApiError(401, "the surface API needs an authenticated caller");
  }
  override async status(session?: string) {
    this.check();
    return super.status(session);
  }
  override async agents() {
    this.check();
    return super.agents();
  }
  override async decision(id: string) {
    this.check();
    return super.decision(id);
  }
}
