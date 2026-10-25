/**
 * The voice view over the example of spec 3.3 §5.6: Finance's invoice prompted, a spoken
 * "allow once" counting down, the release that needs a screen. For the fixture page, the
 * screenshots beside the spec's images, and the tests. Not part of the built page.
 */
import type {
  Command,
  CommandOutcome,
  DecisionCard,
  InputSource,
  Origin,
  VoiceModels,
  VoiceTicket,
  VoiceUpdate,
} from "../generated/api";
import type { VoiceClient } from "../screens/Voice";
import type { SocketEvents, VoiceDevices, VoicePlayer, VoiceSocket } from "../voice/devices";
import { invoiceCard, releaseCard, status } from "./fixture";

/** `card` as voice sees it: allow once and deny for everyday calls, deny alone otherwise. */
export function voiceCard(card: DecisionCard): DecisionCard {
  const deny = { answer: "deny" as const, label: "Deny", needsPresence: false, needsExpanded: false };
  const everyday = card.risk !== "high" && !card.money;
  return {
    ...card,
    allowed: {
      options: everyday
        ? [{ answer: "allowOnce", label: "Allow once", needsPresence: false, needsExpanded: false }, deny]
        : [deny],
      openInHome: false,
      ...(everyday ? { cancelWindowSecs: 5 } : { note: "This always needs you, on a screen." }),
    },
  };
}

/** The voice routes over the example's two decisions. */
export class VoiceFixtureClient implements VoiceClient {
  readonly commands: { command: Command; origin: Origin }[] = [];
  cards: DecisionCard[] = [voiceCard(invoiceCard()), voiceCard(releaseCard())];
  stopped = false;

  async voiceModels(): Promise<VoiceModels> {
    return {
      agent: "Chief of staff",
      models: [
        { id: "gemini", name: "Gemini Live", inputRate: 16_000, outputRate: 24_000 },
        { id: "openai", name: "OpenAI Realtime", inputRate: 24_000, outputRate: 24_000 },
      ],
    };
  }

  async voiceTicket(model: string, sessionId?: string): Promise<VoiceTicket> {
    const rate = model === "openai" ? 24_000 : 16_000;
    return { ticket: "fixture", socket: "/voice/socket", sessionId: sessionId ?? "voice-1", inputRate: rate, outputRate: 24_000 };
  }

  async voiceDecisions() {
    return this.cards;
  }

  async voiceCommand(command: Command, origin: Origin): Promise<CommandOutcome> {
    this.commands.push({ command, origin });
    return command.type === "cancelAnswer"
      ? { type: "answerCancelled", decisionId: command.decisionId }
      : { type: "stopped", text: "All agents stopped" };
  }

  async status() {
    return status(this.stopped);
  }
}

/** A track whose stops and ends the tests read. */
export class FakeTrack extends EventTarget {
  stopped = 0;
  constructor(readonly kind: string) {
    super();
  }
  stop() {
    this.stopped += 1;
  }
  /** The browser's own control ended the share. */
  end() {
    this.dispatchEvent(new Event("ended"));
  }
}

/** A stream of one fake track. */
export function fakeStream(kind: string): MediaStream & { track: FakeTrack } {
  const track = new FakeTrack(kind);
  return { track, getTracks: () => [track] } as unknown as MediaStream & { track: FakeTrack };
}

/** Devices that record what the view sends and let a test play the server. */
export class FakeDevices implements VoiceDevices {
  /** Everything sent on the socket: text as parsed JSON, binary as its tag and length. */
  readonly sent: ({ text: unknown } | { tag: number; bytes: number })[] = [];
  readonly streams: Partial<Record<InputSource, MediaStream & { track: FakeTrack }>> = {};
  readonly captures: { stopped: boolean; chunk: (pcm: ArrayBuffer) => void }[] = [];
  readonly frameTimers: { stopped: boolean; jpeg: (data: ArrayBuffer) => void }[] = [];
  readonly played: number[] = [];
  flushes = 0;
  chimes = 0;
  private events?: SocketEvents;
  private ended?: () => void;

  connect(_url: string, events: SocketEvents): VoiceSocket {
    this.events = events;
    queueMicrotask(() => events.open());
    return {
      send: (data) => {
        if (typeof data === "string") this.sent.push({ text: JSON.parse(data) });
        else this.sent.push({ tag: new Uint8Array(data)[0], bytes: data.byteLength - 1 });
      },
      close: () => undefined,
    };
  }

  /** The server says `update`. */
  say(update: VoiceUpdate) {
    this.events?.text(JSON.stringify(update));
  }

  /** The server closes the socket with `code`. */
  close(code: number) {
    this.events?.close(code);
  }

  private stream(source: InputSource) {
    const stream = fakeStream(source === "mic" ? "audio" : "video");
    this.streams[source] = stream;
    return stream;
  }

  async microphone() {
    return this.stream("mic");
  }

  async camera() {
    return this.stream("camera");
  }

  async screen() {
    return this.stream("screen");
  }

  async capture(_stream: MediaStream, _rate: number, chunk: (pcm: ArrayBuffer) => void) {
    const capture = { stopped: false, chunk };
    this.captures.push(capture);
    return () => {
      capture.stopped = true;
    };
  }

  player(_rate: number, ended: () => void): VoicePlayer {
    this.ended = ended;
    return {
      play: (pcm) => this.played.push(pcm.byteLength),
      flush: () => {
        this.flushes += 1;
        this.ended?.();
      },
      close: () => undefined,
    };
  }

  frames(_stream: MediaStream, _everyMs: number, jpeg: (data: ArrayBuffer) => void) {
    const timer = { stopped: false, jpeg };
    this.frameTimers.push(timer);
    return () => {
      timer.stopped = true;
    };
  }

  chime() {
    this.chimes += 1;
  }
}

/**
 * Devices for the fixture page: the server of the example says what the concept shows, and the
 * screen switch shows a drawn window.
 */
export function fixtureDevices(): VoiceDevices {
  const devices = new FakeDevices();
  const connect = devices.connect.bind(devices);
  devices.connect = (url, events) => {
    const socket = connect(url, events);
    queueMicrotask(() => {
      devices.say({ type: "ready", sessionId: "voice-1", inputRate: 16_000, outputRate: 24_000 });
      devices.say({ type: "prompted", decisionId: "apr_invoice", version: 1 });
      devices.say({ type: "needsScreen", decisionIds: ["apr_release"] });
      devices.say({ type: "heard", text: "Allow once." });
      devices.say({
        type: "pending",
        decisionId: "apr_invoice",
        cancelUntil: new Date(Date.now() + 3_000).toISOString(),
        text: "Allowing once in 3 seconds",
      });
    });
    return socket;
  };
  (devices as VoiceDevices).screen = async () => sharedWindow();
  return devices;
}

/** A drawn stand-in for a shared window, "[Shared window: invoice.pdf]". */
function sharedWindow(): MediaStream {
  const canvas = document.createElement("canvas");
  canvas.width = 640;
  canvas.height = 360;
  const draw = canvas.getContext("2d");
  if (draw) {
    draw.fillStyle = "#f6f7f9";
    draw.fillRect(0, 0, 640, 360);
    draw.fillStyle = "#475569";
    draw.font = "22px Inter, sans-serif";
    draw.textAlign = "center";
    draw.fillText("[Shared window: invoice.pdf]", 320, 186);
  }
  return canvas.captureStream(1);
}
