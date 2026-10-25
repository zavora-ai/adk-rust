/**
 * What the voice view needs from the browser (spec 3.3 §5.6): the socket, the microphone,
 * the camera, the screen, the speaker and a chime. The view takes them as one object, so the
 * tests and the fixture give their own.
 */
import workletUrl from "./capture.worklet.js?url";

/** An open voice socket. */
export interface VoiceSocket {
  send(data: string | ArrayBuffer): void;
  close(): void;
}

/** What a socket reports. */
export interface SocketEvents {
  open(): void;
  text(text: string): void;
  binary(data: ArrayBuffer): void;
  close(code: number): void;
}

/** Plays the model's audio. */
export interface VoicePlayer {
  /** Schedules PCM16 mono after what is already playing. */
  play(pcm: ArrayBuffer): void;
  /** Stops what plays and drops what is scheduled: the person spoke over the model. */
  flush(): void;
  close(): void;
}

export interface VoiceDevices {
  /** Opens the socket at `url`. */
  connect(url: string, events: SocketEvents): VoiceSocket;
  /** The microphone, with echo cancellation. */
  microphone(): Promise<MediaStream>;
  camera(): Promise<MediaStream>;
  /** A screen or window the person picks. */
  screen(): Promise<MediaStream>;
  /** Streams `stream`'s audio as PCM16 mono at `rate` until the returned stop is called. */
  capture(stream: MediaStream, rate: number, chunk: (pcm: ArrayBuffer) => void): Promise<() => void>;
  /** A player of PCM16 mono at `rate`; `ended` runs when nothing is left to play. */
  player(rate: number, ended: () => void): VoicePlayer;
  /** A JPEG of `stream` every `everyMs` until the returned stop is called. */
  frames(stream: MediaStream, everyMs: number, jpeg: (data: ArrayBuffer) => void): () => void;
  /** A short sound, played here only: another agent asks something. */
  chime(): void;
}

/** The widest a camera or screen frame is sent. */
const FRAME_WIDTH = 1024;

/** The browser's own devices. */
export function browserDevices(): VoiceDevices {
  let context: AudioContext | undefined;
  const audio = () => {
    context ??= new AudioContext();
    if (context.state === "suspended") void context.resume();
    return context;
  };
  return {
    connect(url, events) {
      const socket = new WebSocket(url);
      socket.binaryType = "arraybuffer";
      socket.onopen = () => events.open();
      socket.onmessage = (message) => {
        if (typeof message.data === "string") events.text(message.data);
        else events.binary(message.data as ArrayBuffer);
      };
      socket.onclose = (close) => events.close(close.code);
      return {
        send: (data) => {
          if (socket.readyState === WebSocket.OPEN) socket.send(data);
        },
        close: () => socket.close(),
      };
    },
    microphone: () =>
      navigator.mediaDevices.getUserMedia({
        audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true, autoGainControl: true },
        video: false,
      }),
    camera: () => navigator.mediaDevices.getUserMedia({ video: { width: { ideal: 1280 } }, audio: false }),
    screen: () => navigator.mediaDevices.getDisplayMedia({ video: true, audio: false }),
    async capture(stream, rate, chunk) {
      const ctx = audio();
      await ctx.audioWorklet.addModule(workletUrl);
      const source = ctx.createMediaStreamSource(stream);
      const node = new AudioWorkletNode(ctx, "pcm-capture", { processorOptions: { rate } });
      node.port.onmessage = (message: MessageEvent<ArrayBuffer>) => chunk(message.data);
      source.connect(node);
      return () => {
        node.port.onmessage = null;
        source.disconnect();
        node.disconnect();
      };
    },
    player(rate, ended) {
      const ctx = audio();
      let next = 0;
      const playing = new Set<AudioBufferSourceNode>();
      return {
        play(pcm) {
          const samples = new Int16Array(pcm);
          if (samples.length === 0) return;
          const buffer = ctx.createBuffer(1, samples.length, rate);
          const channel = buffer.getChannelData(0);
          for (let i = 0; i < samples.length; i++) channel[i] = samples[i] / 0x8000;
          const source = ctx.createBufferSource();
          source.buffer = buffer;
          source.connect(ctx.destination);
          next = Math.max(next, ctx.currentTime);
          source.start(next);
          next += buffer.duration;
          playing.add(source);
          source.onended = () => {
            playing.delete(source);
            if (playing.size === 0) ended();
          };
        },
        flush() {
          for (const source of playing) {
            source.onended = null;
            source.stop();
          }
          playing.clear();
          next = 0;
          ended();
        },
        close() {
          this.flush();
        },
      };
    },
    frames(stream, everyMs, jpeg) {
      const video = document.createElement("video");
      video.muted = true;
      video.playsInline = true;
      video.srcObject = stream;
      void video.play().catch(() => undefined);
      const canvas = document.createElement("canvas");
      let live = true;
      const timer = window.setInterval(() => {
        if (video.readyState < 2 || video.videoWidth === 0) return;
        const scale = Math.min(1, FRAME_WIDTH / video.videoWidth);
        canvas.width = Math.round(video.videoWidth * scale);
        canvas.height = Math.round(video.videoHeight * scale);
        canvas.getContext("2d")?.drawImage(video, 0, 0, canvas.width, canvas.height);
        canvas.toBlob(
          (blob) => {
            // A frame grabbed before its switch went off is never sent after it.
            if (blob && live) void blob.arrayBuffer().then((data) => live && jpeg(data));
          },
          "image/jpeg",
          0.7,
        );
      }, everyMs);
      return () => {
        live = false;
        window.clearInterval(timer);
        video.srcObject = null;
      };
    },
    chime() {
      const ctx = audio();
      const tone = ctx.createOscillator();
      const gain = ctx.createGain();
      tone.frequency.value = 880;
      gain.gain.setValueAtTime(0.0001, ctx.currentTime);
      gain.gain.exponentialRampToValueAtTime(0.2, ctx.currentTime + 0.02);
      gain.gain.exponentialRampToValueAtTime(0.0001, ctx.currentTime + 0.25);
      tone.connect(gain).connect(ctx.destination);
      tone.start();
      tone.stop(ctx.currentTime + 0.3);
    },
  };
}
