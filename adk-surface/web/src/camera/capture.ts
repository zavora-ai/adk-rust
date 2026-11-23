/**
 * This browser's camera for a camera that takes the frames a screen captures (spec 3.7 §8.2):
 * the camera page opens it on "Use this browser's camera" and sends a JPEG every so often. The
 * page takes it as one object, so the tests give their own.
 */
export interface CameraCapture {
  /** Opens the camera; the browser asks the person the first time. */
  open(): Promise<MediaStream>;
  /** A JPEG of `stream` every `everyMs` until the returned stop is called. */
  frames(stream: MediaStream, everyMs: number, jpeg: (frame: Blob) => void): () => void;
}

/** The widest a frame is sent. */
const FRAME_WIDTH = 1280;

/** The browser's own camera. */
export function browserCapture(): CameraCapture {
  return {
    open: () => navigator.mediaDevices.getUserMedia({ video: { width: { ideal: FRAME_WIDTH } }, audio: false }),
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
            // A frame grabbed before the camera went off is never sent after it.
            if (blob && live) jpeg(blob);
          },
          "image/jpeg",
          0.8,
        );
      }, everyMs);
      return () => {
        live = false;
        window.clearInterval(timer);
        video.srcObject = null;
      };
    },
  };
}
