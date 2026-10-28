// Agent-drawn UI in the web home (spec 3.3 §6.1 `genui`, §7.4 rule 5): a drawing on the card
// background with no decision colours or pills, its strings as text, a form's Send that posts
// the version seen and asks first, the decision under the form, the stale and refused states,
// the refused notice, and the developer panel's section; apps from MCP servers (§6.1
// `genui-apps`): framed sandboxed, labelled, and asking only — a press of the person's does it.
import { render, renderHook, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { ApiError } from "../src/client";
import type { Command, CommandOutcome, FeedEntry, Origin } from "../src/generated/api";
import { FORM_CHANGED_SEND_AGAIN } from "../src/generated/words";
import { Thread, lines } from "../src/components/Thread";
import { DrawnView } from "../src/genui/DrawnView";
import { GenuiProtocols } from "../src/genui/Protocols";
import { RefusedNotice } from "../src/genui/Refused";
import { firstFailure, holds } from "../src/genui/bindings";
import { useFormState } from "../src/genui/forms";
import { parseJsonl } from "../src/genui/parser";
import { sanitizeUrl } from "../src/genui/security";
import { SurfaceStore, getPath, setPath } from "../src/genui/store";
import { applyMessages } from "../src/genui/parser";
import { FixtureClient } from "../src/fixture/fixture";
import { GenuiFixtureClient, formCard, hoursTable, invoiceForm, refusal, timesheetCard } from "../src/fixture/genui";
import { AppsFixture, appCard, shownWeather, weatherApp } from "../src/fixture/genui";
import { AppFrame } from "../src/genui/AppFrame";
import type { BridgeHost, JsonRpc } from "../src/genui/apps";
import { AppBridge, frameable } from "../src/genui/apps";
import type { AppAnswer, AppPrompt, AppRequest } from "../src/generated/api";

/** A thread over `entries`, sending through `send` in session `s-finance`. */
function thread(entries: FeedEntry[], send = vi.fn(async (_command: Command, _origin: Origin) => ({ type: "formSent" }) as CommandOutcome), open = [] as ReturnType<typeof formCard>[]) {
  render(
    <Thread entries={entries} open={open} client={new GenuiFixtureClient("form")} send={send} onSettled={() => undefined} session="s-finance" />,
  );
  return send;
}

const drawn = (seq: number, version = 1): FeedEntry => ({
  seq,
  at: new Date().toISOString(),
  atText: "10:02",
  items: [{ kind: "drawn", surface: { ...invoiceForm(version), version } }],
});

describe("a drawing", () => {
  it("is drawn on the card background with no decision colours or pills", () => {
    const { container } = render(<DrawnView surface={invoiceForm()} onSend={async () => undefined} />);
    const card = container.querySelector(".drawn");
    expect(card).toBeTruthy();
    expect(within(card as HTMLElement).getByText("Drawn by Finance · 10:02")).toBeTruthy();
    expect(within(card as HTMLElement).getByText("Invoice INV-10")).toBeTruthy();
    expect(card?.querySelector('[class*="decision"], [class*="pill"]')).toBeNull();
    // The engine's line under the button, the tool in mono.
    const note = container.querySelector(".drawn-note");
    expect(note?.textContent).toBe("Sending runs send_invoice and asks you first");
    expect(note?.querySelector("code")?.textContent).toBe("send_invoice");
  });

  it("shows agent text as text: no markup, no control characters", () => {
    const surface = { ...timesheetCard(), title: "<b>Total</b>‮\u001b[31m" };
    const { container } = render(<DrawnView surface={surface} />);
    expect(container.querySelector("b")).toBeNull();
    const title = container.querySelector(".drawn-title")?.textContent ?? "";
    expect(title).toContain("<b>Total</b>");
    expect(title).not.toMatch(/[\u0000-\u001f‮]/);
  });

  it("cuts a long table at ten rows with Show all", async () => {
    const { container } = render(<DrawnView surface={hoursTable()} />);
    expect(container.querySelectorAll("tbody tr")).toHaveLength(10);
    await userEvent.click(screen.getByRole("button", { name: "Show all 42" }));
    expect(container.querySelectorAll("tbody tr")).toHaveLength(42);
  });

  it("collapses a version a newer one replaced, and says it changed", () => {
    render(
      <>
        <DrawnView surface={invoiceForm(1)} superseded />
        <DrawnView surface={invoiceForm(2, 10, 5)} changed />
      </>,
    );
    expect(screen.getByText("Earlier version · 10:02")).toBeTruthy();
    expect(screen.getByText("Finance changed this form at 10:05")).toBeTruthy();
  });
});

describe("a render call", () => {
  it("is shown as its drawing, not as a step; a same-named call without one stays a step", () => {
    const call = (seq: number, callId: string): FeedEntry => ({
      seq,
      at: new Date().toISOString(),
      atText: "10:02",
      items: [{ kind: "activity", tool: "render_form", text: "Render form Invoice INV-10", callId }],
    });
    const shown = lines([call(1, "c-draw"), drawn(2), call(3, "c-plain"), { ...drawn(4), items: [{ kind: "result", tool: "render_form", callId: "c-plain", ok: true, text: "Done" }] }]);
    expect(shown.map((line) => line.kind)).toEqual(["drawn", "activity"]);
  });
});

describe("a drawn form's Send", () => {
  it("posts the version seen and the typed values, and asks first", async () => {
    const send = thread([drawn(1, 2)]);
    const to = screen.getByLabelText("To");
    await userEvent.clear(to);
    await userEvent.type(to, "bo@example.com");
    await userEvent.click(screen.getByRole("checkbox", { name: "Send a copy to me" }));
    expect(screen.getByText("Not sent yet")).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() => expect(send).toHaveBeenCalled());
    expect(send.mock.calls[0]).toEqual([
      {
        type: "submitDrawn",
        sessionId: "s-finance",
        surfaceId: "invoice-10",
        version: 2,
        actionId: "send",
        values: { "/to": "bo@example.com", "/amount": 4200, "/due": "2026-11-15", "/copy": true },
      },
      { kind: "pointer" },
    ]);
  });

  it("says what is wrong with a field and sends nothing", async () => {
    const send = thread([drawn(1)]);
    const to = screen.getByLabelText("To");
    await userEvent.clear(to);
    await userEvent.type(to, "ana@");
    await userEvent.click(screen.getByRole("button", { name: "Send" }));
    expect(await screen.findByText("Enter an email address")).toBeTruthy();
    expect(send).not.toHaveBeenCalled();
  });

  it("says a form that changed must be checked and sent again", async () => {
    const send = vi.fn(async () => {
      throw new ApiError(409, FORM_CHANGED_SEND_AGAIN, "surface.drawn_changed");
    });
    thread([drawn(1)], send);
    await userEvent.click(screen.getByRole("button", { name: "Send" }));
    expect(await screen.findByText(FORM_CHANGED_SEND_AGAIN)).toBeTruthy();
  });

  it("is refused by the agent's rules in plain words, and can be sent again", () => {
    render(
      <DrawnView
        surface={invoiceForm()}
        state={{ surfaceId: "invoice-10", version: 1, status: "refused", text: "Finance can't use send_invoice here", locked: false }}
        onSend={async () => undefined}
      />,
    );
    expect(screen.getByText("Finance can't use send_invoice here")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Send" })).toBeTruthy();
  });

  it("shows its decision under it once sent, and what was sent, read only", async () => {
    const sent: FeedEntry = {
      seq: 2,
      at: new Date().toISOString(),
      atText: "10:03",
      items: [
        {
          kind: "drawnSent",
          sent: {
            surfaceId: "invoice-10",
            version: 1,
            actionId: "send",
            tool: "send_invoice",
            callId: "adk-call-requested-1",
            via: "form",
            decisionId: "apr_form",
            held: true,
            at: new Date().toISOString(),
            text: "Sent at 10:03",
          },
        },
        { kind: "decision", decisionId: "apr_form", tool: "send_invoice", text: "Waiting for you" },
      ],
    };
    const card = formCard({ call: { ...formCard().call, args: { to: "ana@example.com", amount: 4200, invoice: "INV-10" } } });
    const { container } = render(
      <Thread
        entries={[drawn(1), sent]}
        open={[card]}
        client={new GenuiFixtureClient("sent")}
        send={vi.fn()}
        onSettled={() => undefined}
        session="s-finance"
      />,
    );
    expect(await screen.findByText("Sent at 10:03 · waiting for you")).toBeTruthy();
    // One card, right under the form; the form's fields are read only and its Send is gone.
    expect(screen.getAllByRole("article", { name: "Send invoice INV-10 to ana@example.com" })).toHaveLength(1);
    const place = container.querySelector(".drawn-place") as HTMLElement;
    expect(within(place).getByRole("article", { name: "Send invoice INV-10 to ana@example.com" })).toBeTruthy();
    expect((screen.getByLabelText("To") as HTMLInputElement).disabled).toBe(true);
    expect(screen.queryByRole("button", { name: "Send" })).toBeNull();
    expect(within(screen.getByRole("article")).queryByRole("button", { name: /Always allow/ })).toBeNull();
  });
});

describe("a refused drawing", () => {
  it("is a plain notice; the technical reason stays out of the thread", () => {
    render(<RefusedNotice refusal={refusal()} />);
    expect(screen.getByText("Finance tried to show something that looks like a decision. It wasn't shown.")).toBeTruthy();
    expect(screen.queryByText(/catalog forms 1.0/)).toBeNull();
  });
});

describe("the developer panel's agent-drawn UI", () => {
  it("lists the claims, the catalog and the refused drawings with their reasons", async () => {
    render(<GenuiProtocols client={new GenuiFixtureClient("refused")} sessionId="s-finance" />);
    expect(await screen.findByText("Version 0.9.1")).toBeTruthy();
    expect(screen.getByText("No approval controls in drawn UI")).toBeTruthy();
    expect(screen.getByText("forms 1.0")).toBeTruthy();
    expect(await screen.findByText(/component c7 Button/)).toBeTruthy();
  });

  it("says so on a server built without it", async () => {
    render(<GenuiProtocols client={new FixtureClient()} sessionId="s-finance" />);
    expect(await screen.findByText("Agent-drawn UI is not enabled on this server.")).toBeTruthy();
  });
});

describe("the ported A2UI parts", () => {
  it("checks fields as the server does", () => {
    const email = { call: "email", args: { value: { path: "/to" } } };
    expect(holds(email, { "/to": "ana@example.com" })).toBe(true);
    expect(holds(email, { "/to": "ana@" })).toBe(false);
    expect(holds({ call: "required", args: { value: { path: "/to" } } }, { "/to": " " })).toBe(false);
    expect(holds({ call: "numeric", args: { value: { path: "/n" }, max: 10 } }, { "/n": 11 })).toBe(false);
    expect(holds({ call: "not", args: { value: { call: "length", args: { value: "abc", max: 2 } } } }, {})).toBe(true);
    expect(holds({ call: "regex", args: { value: "INV-10", pattern: "INV-\\d+" } }, {})).toBe(true);
    expect(firstFailure([{ condition: email, message: "Enter an email address" }], { "/to": "x" })).toBe("Enter an email address");
  });

  it("keeps a refused send's state while the page draws again, so Send stays busy", () => {
    const client = new FixtureClient();
    const sent = {
      actionId: "send",
      at: "2026-10-27T10:03:00Z",
      callId: "adk-call-requested-1",
      held: false,
      surfaceId: "invoice-10",
      text: "Finance can't use send_invoice here",
      tool: "send_invoice",
      version: 1,
      via: "form" as const,
    };
    // The thread hands the hook a new object on every render, as the feed does.
    const { result, rerender } = renderHook(() => useFormState(client, "s1", { ...sent }, undefined, false));
    const first = result.current;
    rerender();
    expect(result.current).toBe(first);
  });

  it("leaves a pattern to the server and passes a blank number, as the server does", () => {
    const code = { call: "regex", args: { value: { path: "/code" }, pattern: "(a+)+" } };
    const started = Date.now();
    expect(firstFailure([{ condition: code, message: "Check the code" }], { "/code": `${"a".repeat(40)}!` })).toBeUndefined();
    expect(firstFailure([{ condition: { call: "not", args: { value: code } }, message: "No" }], { "/code": "aaa" })).toBeUndefined();
    expect(Date.now() - started).toBeLessThan(1000);
    const quantity = { call: "numeric", args: { value: { path: "/qty" }, min: 1 } };
    expect(holds(quantity, { "/qty": "" })).toBe(true);
    expect(holds(quantity, { "/qty": "  " })).toBe(true);
    expect(holds(quantity, { "/qty": "0" })).toBe(false);
  });

  it("reads A2UI v0.9.1 JSON lines into surfaces", () => {
    const store = new SurfaceStore();
    applyMessages(
      store,
      parseJsonl(
        [
          '{"version":"v0.9.1","createSurface":{"surfaceId":"s"}}',
          '{"version":"v0.9.1","updateDataModel":{"surfaceId":"s","path":"/to","value":"ana"}}',
          "",
        ].join("\n"),
      ),
    );
    expect(store.get("s")?.dataModel).toEqual({ to: "ana" });
    expect(() => parseJsonl('{"callFunction":{}}')).toThrow(/line 1/);
    expect(getPath(setPath({ rows: [{ a: 1 }] }, "/rows/0/a", 2), "/rows/0/a")).toBe(2);
  });

  it("never follows a link to this computer or a script", () => {
    expect(sanitizeUrl("https://example.com/a", "anchor")).toBe("https://example.com/a");
    expect(sanitizeUrl("http://127.0.0.1:8080/", "anchor")).toBeNull();
    expect(sanitizeUrl("http://localhost/", "image")).toBeNull();
    expect(sanitizeUrl("javascript:alert(1)", "anchor")).toBeNull();
  });
});

/** A bridge over a fake page that answers each request with `answer`. */
function bridge(answer: (request: AppRequest) => Promise<AppAnswer>) {
  const posted: JsonRpc[] = [];
  const prompts: AppPrompt[] = [];
  const heights: number[] = [];
  const host: BridgeHost = {
    post: (message) => posted.push(message),
    ask: answer,
    resize: (height) => heights.push(height),
    prompt: (prompt) => prompts.push(prompt),
    theme: () => "dark",
    toolInput: { city: "Nairobi" },
    toolResult: { content: [{ type: "text", text: "24°" }], isError: false },
  };
  return { bridge: new AppBridge(host), posted, prompts, heights };
}

/** The app's frame in `container`, once opened. */
function framed(container: HTMLElement): Promise<HTMLIFrameElement> {
  return waitFor(() => {
    const frame = container.querySelector("iframe");
    expect(frame).toBeTruthy();
    return frame as HTMLIFrameElement;
  });
}

/** Posts `data` to the page as the frame `frame` would. */
function fromFrame(frame: HTMLIFrameElement, data: unknown) {
  window.dispatchEvent(new MessageEvent("message", { data, source: frame.contentWindow }));
}

describe("an app from an MCP server", () => {
  it("is framed on the card background, labelled, with scripts only and nothing of the home's", async () => {
    const { container } = render(<AppFrame app={weatherApp()} seam={new AppsFixture()} session="s-chief" />);
    const frame = await framed(container);
    expect({
      sandbox: frame.getAttribute("sandbox"),
      src: frame.getAttribute("src"),
      referrer: frame.getAttribute("referrerpolicy"),
      allow: frame.getAttribute("allow"),
    }).toEqual({ sandbox: "allow-scripts", src: (shownWeather() as { frameUrl: string }).frameUrl, referrer: "no-referrer", allow: "" });
    const app = container.querySelector(".drawn") as HTMLElement;
    expect(within(app).getByText("App from Weather · it can't approve anything")).toBeTruthy();
    expect(app.querySelector('[class*="decision"], [class*="pill"]')).toBeNull();
  });

  it("says it is opening, blocked, or not shown here with the tool's text", async () => {
    const opening = render(<AppFrame app={weatherApp()} seam={new AppsFixture("never")} session="s-chief" />);
    expect(opening.getByText("Opening the app…")).toBeTruthy();
    opening.unmount();
    const blocked = render(
      <AppFrame
        app={weatherApp()}
        seam={new AppsFixture({ state: "blocked", reason: "approvalButtons", text: "This app was blocked: it shows approval buttons", detail: "Allow once" })}
        session="s-chief"
      />,
    );
    await waitFor(() => expect(blocked.getByText("This app was blocked: it shows approval buttons")).toBeTruthy());
    expect(blocked.container.querySelector("iframe")).toBeNull();
    blocked.unmount();
    const elsewhere = render(<AppFrame app={weatherApp()} />);
    expect(elsewhere.getByText("Open the home on this computer to see this app")).toBeTruthy();
    expect(elsewhere.getByText("Nairobi: 24°, light rain from 15:00. Tomorrow 22°, dry.")).toBeTruthy();
    expect(elsewhere.container.querySelector("iframe")).toBeNull();
  });

  it("is never framed from another device when its origin is this computer", () => {
    expect([
      frameable("http://127.0.0.1:41234/app/t", "http://127.0.0.1:41234", "127.0.0.1"),
      frameable("http://127.0.0.1:41234/app/t", "http://127.0.0.1:41234", "192.168.1.20"),
      frameable("https://apps.example.ts.net/app/t", "https://apps.example.ts.net", "home.example.ts.net"),
      frameable("https://evil.example/app/t", "https://apps.example.ts.net", "home.example.ts.net"),
      frameable("javascript:alert(1)", "https://apps.example.ts.net", "127.0.0.1"),
    ]).toEqual([true, false, true, false, false]);
  });

  it("answers the app's start itself and relays its requests, one at a time", async () => {
    let release: (answer: AppAnswer) => void = () => undefined;
    const { bridge: relay, posted, heights } = bridge(
      (request) =>
        request.method === "ui/initialize"
          ? Promise.resolve({ result: { protocolVersion: "2026-01-26", hostContext: { platform: "web" } } })
          : new Promise((resolve) => {
              release = resolve;
            }),
    );
    await relay.receive({ jsonrpc: "2.0", id: 1, method: "ui/initialize", params: {} });
    await relay.receive({ jsonrpc: "2.0", method: "ui/notifications/initialized" });
    await relay.receive({ jsonrpc: "2.0", method: "ui/notifications/size-changed", params: { height: 312 } });
    const first = relay.receive({ jsonrpc: "2.0", id: 2, method: "tools/call", params: { name: "set_rain_alert" } });
    await relay.receive({ jsonrpc: "2.0", id: 3, method: "tools/call", params: { name: "set_rain_alert" } });
    release({ result: { content: [], isError: false } });
    await first;
    expect(posted).toEqual([
      { jsonrpc: "2.0", id: 1, result: { protocolVersion: "2026-01-26", hostContext: { platform: "web", theme: "dark" } } },
      { jsonrpc: "2.0", method: "ui/notifications/tool-input", params: { arguments: { city: "Nairobi" } } },
      { jsonrpc: "2.0", method: "ui/notifications/tool-result", params: { content: [{ type: "text", text: "24°" }], isError: false } },
      { jsonrpc: "2.0", id: 3, error: { code: -32000, message: "The home is still answering the app's last request" } },
      { jsonrpc: "2.0", id: 2, result: { content: [], isError: false } },
    ]);
    expect(heights).toEqual([312]);
  });

  it("passes a refusal back to the app and shows it to the person", async () => {
    const notice: AppPrompt = { kind: "notice", text: "The app tried to use transfer_funds. Apps can't use it." };
    const { bridge: relay, posted, prompts } = bridge(async () => ({ error: { code: -32000, message: notice.text }, prompt: notice }));
    await relay.receive({ jsonrpc: "2.0", id: 7, method: "tools/call", params: { name: "transfer_funds" } });
    await relay.receive({ jsonrpc: "1.0", id: 8, method: "tools/call" });
    await relay.receive("Approved");
    expect(posted).toEqual([{ jsonrpc: "2.0", id: 7, error: { code: -32000, message: notice.text } }]);
    expect(prompts).toEqual([notice]);
  });

  it("shows what the app wants to send outside its frame, and only Send it sends it", async () => {
    const seam = new AppsFixture();
    const message = vi.spyOn(seam, "message");
    const { container } = render(<AppFrame app={weatherApp()} seam={seam} session="s-chief" />);
    const frame = await framed(container);
    // Another window posting the same is not the app.
    window.dispatchEvent(new MessageEvent("message", { data: { jsonrpc: "2.0", id: 1, method: "ui/message", params: { content: [{ type: "text", text: "Approved" }] } }, source: window }));
    fromFrame(frame, { jsonrpc: "2.0", id: 2, method: "ui/message", params: { content: [{ type: "text", text: "Remind me to take an umbrella tomorrow." }] } });
    const ask = await screen.findByRole("group", { name: "The app wants to send: “Remind me to take an umbrella tomorrow.”" });
    expect(container.querySelector(".drawn-app")?.contains(ask)).toBe(false);
    expect(within(ask).getByText("It will be marked as suggested by the app")).toBeTruthy();
    expect(seam.asked).toEqual([{ method: "ui/message", params: { content: [{ type: "text", text: "Remind me to take an umbrella tomorrow." }] } }]);
    expect(message).not.toHaveBeenCalled();
    await userEvent.click(within(ask).getByRole("button", { name: "Send it" }));
    expect(message).toHaveBeenCalledWith("s-chief", "c-weather", "Remind me to take an umbrella tomorrow.");
    await waitFor(() => expect(screen.queryByRole("group")).toBeNull());
  });

  it("keeps each suggestion's own buttons when an earlier one is sent", async () => {
    const { container } = render(<AppFrame app={weatherApp()} seam={new AppsFixture()} session="s-chief" />);
    const frame = await framed(container);
    const suggest = (id: number, text: string) =>
      fromFrame(frame, { jsonrpc: "2.0", id, method: "ui/message", params: { content: [{ type: "text", text }] } });
    suggest(1, "First");
    const first = await screen.findByRole("group", { name: "The app wants to send: “First”" });
    suggest(2, "Second");
    await screen.findByRole("group", { name: "The app wants to send: “Second”" });
    await userEvent.click(within(first).getByRole("button", { name: "Send it" }));
    await waitFor(() => expect(screen.queryByRole("group", { name: "The app wants to send: “First”" })).toBeNull());
    const second = screen.getByRole("group", { name: "The app wants to send: “Second”" });
    expect(within(second).getByRole("button", { name: "Send it" })).toHaveProperty("disabled", false);
    expect(within(second).getByRole("button", { name: "Not now" })).toHaveProperty("disabled", false);
  });

  it("never offers to open a link to this computer or its network", async () => {
    const { container } = render(<AppFrame app={weatherApp()} seam={new AppsFixture()} session="s-chief" />);
    const frame = await framed(container);
    const urls = [
      "http://0.0.0.0:8080/",
      "http://localhost.:8080/ui/",
      "http://[::ffff:127.0.0.1]:8080/ui/",
      "http://192.168.1.1/apply.cgi",
      "http://10.0.0.4/",
      "http://172.16.0.1/",
      "http://169.254.169.254/latest",
      "https://printer.local/",
      "http://intranet/",
      "http://[fe80::1]/",
      "https://example.com/nairobi",
    ];
    const enabled: boolean[] = [];
    for (const [index, url] of urls.entries()) {
      fromFrame(frame, { jsonrpc: "2.0", id: index + 1, method: "ui/open-link", params: { url } });
      const ask = await waitFor(() => {
        const shown = screen.getAllByRole("group", { name: "Open example.com?" }).find((group) => within(group).queryByText(url));
        expect(shown).toBeTruthy();
        return shown as HTMLElement;
      });
      enabled.push(!(within(ask).getByRole("button", { name: "Open" }) as HTMLButtonElement).disabled);
    }
    expect(enabled).toEqual([...urls.slice(0, -1).map(() => false), true]);
  });

  it("asks before opening a link, and a tool apps can't use is a plain notice", async () => {
    const opened = vi.spyOn(window, "open").mockReturnValue(null);
    const { container } = render(<AppFrame app={weatherApp()} seam={new AppsFixture()} session="s-chief" />);
    const frame = await framed(container);
    fromFrame(frame, { jsonrpc: "2.0", id: 1, method: "tools/call", params: { name: "transfer_funds", arguments: { amount: 5000 } } });
    expect((await screen.findByRole("note")).textContent).toBe("The app tried to use transfer_funds. Apps can't use it.");
    fromFrame(frame, { jsonrpc: "2.0", id: 2, method: "ui/open-link", params: { url: "https://example.com/nairobi/radar" } });
    const ask = await screen.findByRole("group", { name: "Open example.com?" });
    expect(opened).not.toHaveBeenCalled();
    await userEvent.click(within(ask).getByRole("button", { name: "Open" }));
    expect(opened).toHaveBeenCalledWith("https://example.com/nairobi/radar", "_blank", "noopener,noreferrer");
    opened.mockRestore();
  });

  it("stands in the thread for its tool call, with the decision of a call it asked for under it", () => {
    const at = new Date().toISOString();
    const entries: FeedEntry[] = [
      { seq: 1, at, atText: "10:05", items: [{ kind: "activity", tool: "weather_show", text: "Weather show", callId: "c-weather" }] },
      { seq: 2, at, atText: "10:05", items: [{ kind: "app", app: weatherApp() }] },
      {
        seq: 3,
        at,
        atText: "10:06",
        items: [
          {
            kind: "drawnSent",
            sent: { surfaceId: "c-weather", version: 1, actionId: "set_rain_alert", tool: "set_rain_alert", callId: "adk-call-requested-3", decisionId: "apr_app", held: true, at, text: "The app asked · 10:06", via: "app" },
          },
          { kind: "decision", decisionId: "apr_app", tool: "set_rain_alert", text: "Waiting for you" },
        ],
      },
    ];
    expect(lines(entries).map((line) => (line.kind === "app" ? `app:${line.sends.map((one) => one.decisionId).join()}` : line.kind))).toEqual(["app:apr_app"]);
    const { container } = render(
      <Thread entries={entries} open={[appCard()]} client={new GenuiFixtureClient("app")} send={vi.fn()} onSettled={() => undefined} session="s-chief" />,
    );
    const place = container.querySelector(".app-place") as HTMLElement;
    const app = place.querySelector(".drawn-app") as HTMLElement;
    expect(within(place).getByText("Turn on rain alerts for Nairobi")).toBeTruthy();
    expect(app.querySelector('[class*="decision"]')).toBeNull();
    expect(screen.getAllByText("Turn on rain alerts for Nairobi")).toHaveLength(1);
  });
});
