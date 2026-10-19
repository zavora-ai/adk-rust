# Multimodal: Video Input

Alongside the audio stream, a realtime session takes image frames from a camera or screen,
so the user can show the agent something — a damaged product for a return, an error on a
screen, a document — instead of describing it.

## The API

One method on the runner, mirroring `send_audio`:

```rust
use adk_realtime::integration::IntegratedRealtimeRunner;

async fn send_frame(runner: &IntegratedRealtimeRunner, base64_jpeg: &str) -> adk_realtime::Result<()> {
    runner.send_video_frame("image/jpeg", base64_jpeg).await
}
```

It is also available on `RealtimeRunner` and on `RealtimeSession`. The trait's default
implementation does nothing, so a session that does not accept visual input ignores frames.

## How each provider handles frames

The two backends treat vision differently, and that should shape how often you
send frames:

| Provider | Mechanism | Cadence |
|----------|-----------|---------|
| **Gemini Live** | A `realtimeInput.video` message per frame | A stream of frames, like audio |
| **OpenAI Realtime** | A conversation item with an `input_image` part per frame | Snapshots |

On OpenAI each frame becomes a conversation item that stays in the context, so the examples
send a frame every 2.5 seconds there and every 0.7 seconds to Gemini.

The example UIs encode this:

```js
const intervalMs = provider === 'gemini' ? 700 : 2500;  // ~1.4 fps vs ~0.4 fps
```

## Capturing frames in a browser

Grab the camera, draw the `<video>` to a canvas, and ship JPEG frames over your
WebSocket (the [server-side bridge](building-web-apps.md) forwards them to
`send_video_frame`):

```js
const stream = await navigator.mediaDevices.getUserMedia({ video: { width: 640, height: 480 } });
video.srcObject = stream;

const canvas = document.createElement('canvas');
setInterval(() => {
  if (ws.readyState !== WebSocket.OPEN) return;
  canvas.width = 640; canvas.height = 480;
  canvas.getContext('2d').drawImage(video, 0, 0, 640, 480);
  const data = canvas.toDataURL('image/jpeg', 0.6).split(',')[1];  // strip data: prefix
  ws.send(JSON.stringify({ type: 'video_frame', mime: 'image/jpeg', data }));
}, intervalMs);
```

On the server, the bridge forwards each `video_frame` message to `send_video_frame` and
treats a failed send as non-fatal (see [Building web apps](building-web-apps.md#server-the-axum-bridge)).
The examples send 640×480 JPEG frames at quality 0.6; smaller frames cost less to send and to
process.

## Prompting for vision

Tell the agent it can see, so it uses the camera naturally:

```text
You can hear the customer and SEE what they show their camera. When they show
you an item (e.g. a damaged product for a return), briefly describe what you see
and use it to help resolve the issue.
```

## Try it

The [`customer_service`](examples.md#customer_service--aria) example has a camera panel:
start the camera, hold up an object, and say *"I want to return this — can you
see it?"*. On OpenAI the agent sees the periodic snapshots.

Next: [Affective dialogue →](affective-dialogue.md)
