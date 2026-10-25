// The voice view (spec 3.3 §5.6, §7.2, §11 S-I9): switches stop their tracks at once and tell
// the server first, Cancel is the view's own pointer answer, a high-risk call is decided on
// screen, the pill counts down, and the microphone is offered only in a secure context.
import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { App } from "../src/App";
import { Composer } from "../src/components/Composer";
import { FixtureClient } from "../src/fixture/fixture";
import { FakeDevices, VoiceFixtureClient } from "../src/fixture/voice";
import { Voice } from "../src/screens/Voice";
import { VoiceEntryContext, voiceOffered } from "../src/voice/context";
import { allowingIn, voiceCantApprove, wantsTo } from "../src/voice/labels";

/** The voice view, connected, with the example's two decisions. */
async function connected() {
  const client = new VoiceFixtureClient();
  const devices = new FakeDevices();
  render(<Voice client={client} devices={devices} onEnd={vi.fn()} />);
  await waitFor(() => expect(devices.sent[0]).toEqual({ text: { type: "hello", ticket: "fixture" } }));
  act(() => devices.say({ type: "ready", sessionId: "voice-1", inputRate: 16_000, outputRate: 24_000 }));
  await waitFor(() => expect(devices.streams.mic).toBeDefined());
  await waitFor(() => expect(devices.captures.length).toBe(1));
  return { client, devices };
}

const texts = (devices: FakeDevices) =>
  devices.sent.flatMap((sent) => ("text" in sent ? [sent.text as { type: string; source?: string; on?: boolean }] : []));

describe("the switches", () => {
  it("stop the right track at once and say so before any later frame", async () => {
    const { devices } = await connected();
    for (const [name, source] of [
      ["Camera off", "camera"],
      ["Screen off", "screen"],
    ] as const) {
      await userEvent.click(screen.getByRole("button", { name }));
      await waitFor(() => expect(devices.frameTimers.length).toBeGreaterThan(0));
      const timer = devices.frameTimers.at(-1)!;
      act(() => timer.jpeg(new Uint8Array([0xff, 0xd8]).buffer));
      const framesBefore = devices.sent.filter((sent) => "tag" in sent).length;
      await userEvent.click(screen.getByRole("button", { name: name.replace("off", "on") }));
      const track = devices.streams[source]!.track;
      expect(track.stopped).toBe(1);
      expect(timer.stopped).toBe(true);
      const last = devices.sent.at(-1);
      expect(last).toEqual({ text: { type: "switch", source, on: false } });
      // A frame grabbed before the switch went off is never sent after it.
      act(() => timer.jpeg(new Uint8Array([0xff, 0xd8]).buffer));
      expect(devices.sent.filter((sent) => "tag" in sent).length).toBe(framesBefore);
      expect(devices.sent.at(-1)).toEqual(last);
    }
    // The other track was never touched.
    expect(devices.streams.mic!.track.stopped).toBe(0);
  });

  it("turns off when the browser's own control ends the share", async () => {
    const { devices } = await connected();
    await userEvent.click(screen.getByRole("button", { name: "Screen off" }));
    await waitFor(() => expect(devices.streams.screen).toBeDefined());
    act(() => devices.streams.screen!.track.end());
    await screen.findByRole("button", { name: "Screen off" });
    expect(texts(devices).at(-1)).toEqual({ type: "switch", source: "screen", on: false });
  });

  it("mic off stops the capture and the microphone", async () => {
    const { devices } = await connected();
    act(() => devices.captures[0].chunk(new Int16Array(160).buffer));
    expect(devices.sent.at(-1)).toEqual({ tag: 0x01, bytes: 320 });
    await userEvent.click(screen.getByRole("button", { name: "Mic on" }));
    expect(devices.captures[0].stopped).toBe(true);
    expect(devices.streams.mic!.track.stopped).toBe(1);
    act(() => devices.captures[0].chunk(new Int16Array(160).buffer));
    expect(devices.sent.at(-1)).toEqual({ text: { type: "switch", source: "mic", on: false } });
  });
});

describe("a spoken answer", () => {
  it("counts down, and Cancel is the view's own answer, on the voice surface, by pointer", async () => {
    const { client, devices } = await connected();
    act(() => {
      devices.say({ type: "prompted", decisionId: "apr_invoice", version: 1 });
      devices.say({ type: "heard", text: "allow once" });
      devices.say({
        type: "pending",
        decisionId: "apr_invoice",
        cancelUntil: new Date(Date.now() + 3_000).toISOString(),
        text: "Allowing once in 5 seconds",
      });
    });
    const card = await screen.findByRole("article", { name: "Send the October invoice to [CLIENT]" });
    expect(within(card).getByText('Heard "allow once"')).toBeTruthy();
    expect(within(card).getByText(allowingIn(3))).toBeTruthy();
    await waitFor(() => expect(within(card).getByText(allowingIn(2))).toBeTruthy(), { timeout: 2_000 });
    await userEvent.click(within(card).getByRole("button", { name: "Cancel" }));
    expect(client.commands).toEqual([
      { command: { type: "cancelAnswer", decisionId: "apr_invoice" }, origin: { kind: "pointer" } },
    ]);
  });

  it("prompts the decision it would apply to with the engine's words", async () => {
    const { devices } = await connected();
    act(() => devices.say({ type: "prompted", decisionId: "apr_invoice", version: 1 }));
    expect(
      await screen.findByText(`${wantsTo("Finance", "Send the October invoice to [CLIENT]")} Say "allow once" or "deny".`),
    ).toBeTruthy();
  });

  it("shows what the person said as text, never as markup", async () => {
    const { devices } = await connected();
    act(() => devices.say({ type: "heard", text: "<img src=x onerror=alert(1)>" }));
    expect(await screen.findByText("<img src=x onerror=alert(1)>")).toBeTruthy();
    expect(document.querySelector("img")).toBeNull();
  });
});

describe("a high-risk decision", () => {
  it("shows Decide on screen and no Allow button", async () => {
    const { devices } = await connected();
    act(() => devices.say({ type: "needsScreen", decisionIds: ["apr_release"] }));
    const line = (await screen.findByText(voiceCantApprove("Publish a release", false), { exact: false })).closest("p")!;
    const link = within(line).getByRole("link", { name: "Decide on screen" });
    expect(link.getAttribute("href")).toBe("/ui/decisions/apr_release");
    expect(screen.queryByRole("button", { name: /allow/i })).toBeNull();
  });
});

describe("the microphone in the message box", () => {
  it("is offered only where the page may use the microphone and the server offers voice", () => {
    expect(voiceOffered(1, true)).toBe(true);
    expect(voiceOffered(1, false)).toBe(false);
    expect(voiceOffered(0, true)).toBe(false);
  });

  it("opens the voice view, and is absent without voice", async () => {
    const open = vi.fn();
    const { unmount } = render(
      <VoiceEntryContext.Provider value={{ open }}>
        <Composer placeholder="Message Finance" queued={[]} onSend={vi.fn()} />
      </VoiceEntryContext.Provider>,
    );
    await userEvent.click(screen.getByRole("button", { name: "Talk by voice" }));
    expect(open).toHaveBeenCalledTimes(1);
    unmount();
    render(<Composer placeholder="Message Finance" queued={[]} onSend={vi.fn()} />);
    expect(screen.queryByRole("button", { name: "Talk by voice" })).toBeNull();
  });

  it("is hidden by the app in an insecure context", async () => {
    const client = new FixtureClient();
    const withVoice = Object.assign(client, {
      voiceModels: async () => ({ agent: "Chief of staff", models: [{ id: "gemini", name: "Gemini Live", inputRate: 16_000, outputRate: 24_000 }] }),
      voiceTicket: vi.fn(),
      voiceDecisions: async () => [],
      voiceCommand: vi.fn(),
    });
    const secure = Object.getOwnPropertyDescriptor(globalThis, "isSecureContext");
    Object.defineProperty(globalThis, "isSecureContext", { value: false, configurable: true });
    try {
      render(<App client={withVoice} initialSection="threads" />);
      await screen.findByRole("textbox", { name: "Message Finance" });
      await new Promise((resolve) => setTimeout(resolve, 50));
      expect(screen.queryByRole("button", { name: "Talk by voice" })).toBeNull();
    } finally {
      if (secure) Object.defineProperty(globalThis, "isSecureContext", secure);
      else delete (globalThis as { isSecureContext?: boolean }).isSecureContext;
    }
  });
});
