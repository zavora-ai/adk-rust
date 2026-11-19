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
  /**
   * Screen actions said aloud (spec 3.3 §5.6): each still in its cancel window, then the last
   * outcome. An outcome replaces only the action it ends, so another's Cancel stays.
   */
  actions: SpokenAction[];
  problem?: string;
  inputs: Record<InputSource, boolean>;
  /** What "What it can see" shows: the screen, else the camera. */
  preview?: MediaStream;
  /** The captions, oldest first: what it heard, and each time the person spoke over the model. */
  captions: Caption[];
  /** The last frames sent to the model, oldest first: exactly what it saw. */
  sent: SentFrame[];
}

/** A camera or screen frame as it was sent, for the person to see what the model saw. */
export interface SentFrame {
  source: "camera" | "screen";
  at: number;
  /** A `blob:` URL of the JPEG, revoked when the frame leaves the list. */
  url: string;
}

/** A caption: what it heard, or that the person spoke over the model, which then stopped. */
export type Caption = { at: number } & ({ kind: "heard"; text: string } | { kind: "spokeOver" });

/** Loudness of what the mic hears and of what the model says, 0 to 1. */
export type Levels = { input: number; output: number };

/** How many sent frames the view keeps. */
const FRAMES_KEPT = 3;

/** How many captions the view keeps. */
const CAPTIONS_KEPT = 4;

/** The loudness of PCM16 mono, 0 to 1, on a curve that suits speech. */
export function loudness(pcm: ArrayBuffer): number {
  const samples = new Int16Array(pcm, 0, Math.floor(pcm.byteLength / 2));
  if (samples.length === 0) return 0;
  let sum = 0;
  for (let index = 0; index < samples.length; index += 4) sum += samples[index] * samples[index];
  const rms = Math.sqrt(sum / Math.ceil(samples.length / 4)) / 32768;
  return Math.min(1, Math.sqrt(rms) * 1.6);
}

/** Where a ticket comes from: `POST /api/surface/voice/sessions`. */
export type TicketSource = (model: string, sessionId?: string) => Promise<VoiceTicket>;

/** A switch's live input: its stream and what to stop when it goes off. */
interface Live {
  stream: MediaStream;
  stop?: () => void;
}

export function initialState(): VoiceState {
  return { phase: "connecting", needsScreen: [], actions: [], inputs: { mic: false, camera: false, screen: false }, captions: [], sent: [] };
}

export class VoiceSession {
  private state: VoiceState = initialState();
  private socket?: VoiceSocket;
  private player?: VoicePlayer;
  private inputRate = 16_000;
  private live: Partial<Record<InputSource, Live>> = {};
  private closed = false;
  private levels: Levels = { input: 0, output: 0 };
  /** Hears the loudness as it changes, outside React's renders. */
  onLevels?: (levels: Levels) => void;

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

  /** Eases toward `value` quickly when louder, slowly when quieter, so the meter reads calm. */
  private level(which: keyof Levels, value: number) {
    const previous = this.levels[which];
    const next = value > previous ? previous + (value - previous) * 0.6 : previous + (value - previous) * 0.15;
    this.levels = { ...this.levels, [which]: next };
    this.onLevels?.(this.levels);
  }

  private caption(line: Caption, change: Partial<VoiceState> = {}) {
    this.set({ ...change, captions: [...this.state.captions, line].slice(-CAPTIONS_KEPT) });
  }

  /** Keeps the frame sent last, and lets go of the oldest. */
  private keep(source: "camera" | "screen", jpeg: ArrayBuffer) {
    // The frame is already sent; a thumbnail the browser can't make is only left out.
    let url: string;
    try {
      url = URL.createObjectURL(new Blob([jpeg], { type: "image/jpeg" }));
    } catch {
      return;
    }
    const sent = [...this.state.sent, { source, at: Date.now(), url }];
    for (const old of sent.splice(0, Math.max(0, sent.length - FRAMES_KEPT))) URL.revokeObjectURL(old.url);
    this.set({ sent });
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
        if (bytes[0] === FRAME_AUDIO) {
          const pcm = data.slice(1);
          this.level("output", loudness(pcm));
          this.player?.play(pcm);
        }
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
        this.level("output", 0);
        this.set({ phase: "listening" });
        break;
      case "speaking":
        this.set({ phase: "speaking" });
        break;
      case "heard":
        this.caption({ at: Date.now(), kind: "heard", text: update.text }, { heard: update.text });
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
          actions: [
            ...this.state.actions.filter((action) => action.kind === "pending"),
            {
              kind: "pending",
              actionId: update.actionId,
              heard: this.state.heard,
              heading: update.heading,
              cancelUntil: update.cancelUntil,
            },
          ],
        });
        break;
      case "actionOutcome": {
        const actionId = update.actionId ?? undefined;
        const ended = this.state.actions.find((action) => action.kind === "pending" && action.actionId === actionId);
        this.set({
          actions: [
            ...this.state.actions.filter((action) => action.kind === "pending" && action !== ended),
            {
              kind: "ended",
              actionId,
              done: update.done,
              heard: ended?.heard ?? this.state.heard,
              heading: update.heading ?? undefined,
              text: update.text,
            },
          ],
        });
        break;
      }
      case "bargeIn":
        this.player?.flush();
        this.level("output", 0);
        this.caption({ at: Date.now(), kind: "spokeOver" });
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
      // Its thumbnails go with it: what was shown stops being shown.
      const sent = this.state.sent.filter((frame) => frame.source !== source);
      for (const frame of this.state.sent) if (frame.source === source) URL.revokeObjectURL?.(frame.url);
      this.set({ inputs: { ...this.state.inputs, [source]: false }, preview: this.preview(), sent });
      if (source === "mic") this.level("input", 0);
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
        if (this.live.mic !== live) return;
        this.frame(FRAME_AUDIO, pcm);
        this.level("input", loudness(pcm));
      });
    } else {
      const tag = source === "camera" ? FRAME_CAMERA : FRAME_SCREEN;
      live.stop = this.devices.frames(stream, FRAME_EVERY_MS, (jpeg) => {
        if (this.live[source] !== live) return;
        this.frame(tag, jpeg);
        this.keep(source, jpeg);
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
    for (const frame of this.state.sent) URL.revokeObjectURL?.(frame.url);
    this.levels = { input: 0, output: 0 };
    this.onLevels?.(this.levels);
    this.state = { ...this.state, inputs: { mic: false, camera: false, screen: false }, preview: undefined, sent: [] };
  }
}
