/**
 * One voice conversation from the view's side (spec 3.3 §5.6): the ticket, the socket, the
 * microphone, the camera and the screen, and the model's audio. Decision cards are not on the
 * socket: the view reads them from the engine (§6). A switch turned off stops its track at
 * once and tells the server before anything else is sent.
 */
import type { InputSource, VoiceTicket, VoiceUpdate } from "../generated/api";
import type { SpokenAction } from "../verbs/VoiceAction";
import type { VoiceDevices, VoicePlayer, VoiceSocket } from "./devices";
import { voiceLabels } from "./labels";

/** The tags of the socket's binary frames (`adk_surface::voice::FRAME_*`). */
export const FRAME_AUDIO = 0x01;
export const FRAME_CAMERA = 0x02;
export const FRAME_SCREEN = 0x03;

/** How often a camera or screen frame is sent; the server forwards no faster than its model takes them. */
const FRAME_EVERY_MS = 1000;

/** The close code of a socket without a valid ticket. */
const CLOSE_NO_TICKET = 4401;

export type Phase = "connecting" | "listening" | "speaking" | "ended";

/** What the view shows of the conversation. */
export interface VoiceState {
  phase: Phase;
  sessionId?: string;
  heard?: string;
  prompted?: { id: string; version: number };
  pending?: { id: string; cancelUntil: number; text: string };
  outcome?: { id: string; decided: boolean; text?: string };
  needsScreen: string[];
  /** A screen action said aloud: waiting out its cancel window, or how it ended (spec 3.3 §5.6). */
  action?: SpokenAction;
  problem?: string;
  inputs: Record<InputSource, boolean>;
  /** What "What it can see" shows: the screen, else the camera. */
  preview?: MediaStream;
}

/** Where a ticket comes from: `POST /api/surface/voice/sessions`. */
export type TicketSource = (model: string, sessionId?: string) => Promise<VoiceTicket>;

/** A switch's live input: its stream and what to stop when it goes off. */
interface Live {
  stream: MediaStream;
  stop?: () => void;
}

export function initialState(): VoiceState {
  return { phase: "connecting", needsScreen: [], inputs: { mic: false, camera: false, screen: false } };
}

export class VoiceSession {
  private state: VoiceState = initialState();
  private socket?: VoiceSocket;
  private player?: VoicePlayer;
  private inputRate = 16_000;
  private live: Partial<Record<InputSource, Live>> = {};
  private closed = false;

  constructor(
    private readonly tickets: TicketSource,
    private readonly devices: VoiceDevices,
    private readonly onState: (state: VoiceState) => void,
    private readonly location: { protocol: string; host: string } = window.location,
  ) {}

  private set(change: Partial<VoiceState>) {
    this.state = { ...this.state, ...change };
    this.onState(this.state);
  }

  /** Opens the conversation on `model`, continuing the voice session `sessionId` when given. */
  async start(model: string, sessionId?: string) {
    let ticket: VoiceTicket;
    try {
      ticket = await this.tickets(model, sessionId);
    } catch (error) {
      this.set({ phase: "ended", problem: error instanceof Error ? error.message : String(error) });
      return;
    }
    if (this.closed) return;
    this.inputRate = ticket.inputRate;
    this.set({ sessionId: ticket.sessionId });
    const scheme = this.location.protocol === "https:" ? "wss" : "ws";
    this.socket = this.devices.connect(`${scheme}://${this.location.host}${ticket.socket}`, {
      open: () => this.socket?.send(JSON.stringify({ type: "hello", ticket: ticket.ticket })),
      text: (text) => this.receive(JSON.parse(text) as VoiceUpdate, ticket.outputRate),
      binary: (data) => {
        const bytes = new Uint8Array(data);
        if (bytes[0] === FRAME_AUDIO) this.player?.play(data.slice(1));
      },
      close: (code) => {
        this.release();
        this.set({ phase: "ended", ...(code === CLOSE_NO_TICKET ? { problem: voiceLabels.ticketRefused } : {}) });
      },
    });
  }

  private receive(update: VoiceUpdate, outputRate: number) {
    switch (update.type) {
      case "ready":
        this.player = this.devices.player(outputRate, () => this.text({ type: "playbackEnded" }));
        this.set({ phase: "listening", sessionId: update.sessionId });
        void this.switch("mic", true);
        break;
      case "listening":
        this.set({ phase: "listening" });
        break;
      case "speaking":
        this.set({ phase: "speaking" });
        break;
      case "heard":
        this.set({ heard: update.text });
        break;
      case "prompted":
        this.set({
          prompted:
            update.decisionId && update.version != null
              ? { id: update.decisionId, version: update.version }
              : undefined,
        });
        break;
      case "pending":
        this.set({
          pending: { id: update.decisionId, cancelUntil: Date.parse(update.cancelUntil), text: update.text },
          outcome: undefined,
        });
        break;
      case "outcome":
        this.set({
          pending: this.state.pending?.id === update.decisionId ? undefined : this.state.pending,
          outcome: { id: update.decisionId, decided: update.decided, text: update.text ?? undefined },
        });
        break;
      case "needsScreen":
        this.set({ needsScreen: update.decisionIds });
        break;
      case "pendingAction":
        this.set({
          action: {
            kind: "pending",
            actionId: update.actionId,
            heard: this.state.heard,
            heading: update.heading,
            cancelUntil: update.cancelUntil,
          },
        });
        break;
      case "actionOutcome":
        this.set({
          action: {
            kind: "ended",
            done: update.done,
            heard: this.state.action?.heard ?? this.state.heard,
            heading: update.heading ?? undefined,
            text: update.text,
          },
        });
        break;
      case "bargeIn":
        this.player?.flush();
        break;
      case "ended":
        this.set({ phase: "ended" });
        break;
      case "error":
        this.set({ problem: update.message });
        break;
    }
  }

  private text(message: object) {
    this.socket?.send(JSON.stringify(message));
  }

  private frame(tag: number, data: ArrayBuffer) {
    const frame = new Uint8Array(1 + data.byteLength);
    frame[0] = tag;
    frame.set(new Uint8Array(data), 1);
    this.socket?.send(frame.buffer);
  }

  private preview(): MediaStream | undefined {
    return this.live.screen?.stream ?? this.live.camera?.stream;
  }

  /**
   * Turns `source` on or off. Off tells the server first, then stops the input's tracks at
   * once — never just disables them — and clears its preview, so nothing of it is sent after.
   */
  async switch(source: InputSource, on: boolean) {
    if (!on) {
      const live = this.live[source];
      delete this.live[source];
      this.set({ inputs: { ...this.state.inputs, [source]: false }, preview: this.preview() });
      this.text({ type: "switch", source, on: false });
      live?.stop?.();
      live?.stream.getTracks().forEach((track) => track.stop());
      return;
    }
    if (this.live[source] || this.closed) return;
    let stream: MediaStream;
    try {
      stream =
        source === "mic"
          ? await this.devices.microphone()
          : source === "camera"
            ? await this.devices.camera()
            : await this.devices.screen();
    } catch {
      if (source === "mic") this.set({ problem: voiceLabels.noMicrophone });
      return;
    }
    if (this.closed) {
      stream.getTracks().forEach((track) => track.stop());
      return;
    }
    const live: Live = { stream };
    this.live[source] = live;
    // Ending the share in the browser's own controls turns the switch off.
    stream.getTracks().forEach((track) =>
      track.addEventListener("ended", () => {
        if (this.live[source] === live) void this.switch(source, false);
      }),
    );
    this.text({ type: "switch", source, on: true });
    this.set({ inputs: { ...this.state.inputs, [source]: true }, preview: this.preview() });
    if (source === "mic") {
      live.stop = await this.devices.capture(stream, this.inputRate, (pcm) => {
        if (this.live.mic === live) this.frame(FRAME_AUDIO, pcm);
      });
    } else {
      const tag = source === "camera" ? FRAME_CAMERA : FRAME_SCREEN;
      live.stop = this.devices.frames(stream, FRAME_EVERY_MS, (jpeg) => {
        if (this.live[source] === live) this.frame(tag, jpeg);
      });
    }
  }

  /** **Cancel** on a spoken screen action in its cancel window (spec 3.3 §5.6). */
  cancelAction(actionId: string) {
    this.text({ type: "cancelAction", actionId });
  }

  /** Ends the conversation: a spoken answer in its cancel window is cancelled by the server. */
  end() {
    if (this.closed) return;
    this.text({ type: "end" });
    this.socket?.close();
    this.release();
    this.set({ phase: "ended" });
  }

  private release() {
    this.closed = true;
    for (const source of ["mic", "camera", "screen"] as const) {
      const live = this.live[source];
      delete this.live[source];
      live?.stop?.();
      live?.stream.getTracks().forEach((track) => track.stop());
    }
    this.player?.close();
    this.player = undefined;
    this.state = { ...this.state, inputs: { mic: false, camera: false, screen: false }, preview: undefined };
  }
}
