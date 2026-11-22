# The `adk-rust vision` CLI

`adk-rust vision` manages the model manifest and licence acceptance, runs one perception call
on an image — on the device first, through the egress gate when the backend is remote — shows
what left the machine, and lists and probes frame sources (spec 3.7 §8.1).

```bash
cargo install adk-cli --features vision-local   # the on-device backends
cargo install adk-cli --features vision         # cloud backends only, pure Rust
```

| Feature | Commands and backends |
|---------|-----------------------|
| `vision` | `models`, `egress log`, `sources`, and `run` with the remote backends: Claude, Gemini and NVIDIA NIM describers, NVIDIA NIM's document reader, Meta `sam-3.1` |
| `vision-local` | `vision` plus the on-device backends — RF-DETR, EfficientSAM, SAM 2.1, PP-OCRv6, PaddleOCR-VL-1.6 — and the face and personal-text redactors the egress gate needs |

With the `adk-rust` umbrella crate, features `vision` and `cli` forward to the CLI's `vision`,
and `vision-local` to its `vision-local`.

## Where Things Live

Everything the CLI keeps belongs to a project: the current directory, or `--project <dir>`.

| Path | Holds |
|------|-------|
| `.adk/vision.toml` | The egress rules (`--config` names another file). No file means every release asks |
| `.adk/vision/licences.json` | Licence acceptances, recorded by `models accept` |
| `.adk/vision/egress.jsonl` | Every release and refusal, written by the gate before a request is sent |
| `.adk/vision/media/` | The egress copies: the exact bytes released |
| `.adk/vision/held/` | The image a held call would send, for you to inspect before approving |

Model weights live in the Hugging Face cache: `HF_HUB_CACHE`, else `$HF_HOME/hub`, else
`~/.cache/huggingface/hub`. `HF_HUB_OFFLINE=1` opens no socket; `HF_ENDPOINT` and `HF_TOKEN`
are read as `huggingface_hub` reads them.

## `vision models`

| Command | Does |
|---------|------|
| `adk-rust vision models list [--output table\|json]` | Every model in the built-in manifest and your own: task, licence class and id, size, whether it is cached, and acceptance for a use-restricted licence |
| `adk-rust vision models fetch <id>…` | Fetches each model by its pinned revision and checks every file's SHA-256; the licence gate runs first |
| `adk-rust vision models verify [<id>…]` | Checks every cached file's SHA-256 again, offline; with no id, every model that is fully cached |
| `adk-rust vision models accept <id>` | Shows a use-restricted model's licence URL and records your acceptance; it asks on a terminal and refuses without one, so no agent can accept a licence |

`--manifest <file or directory>` adds your own manifests, such as an RF-DETR fine-tune you
exported, beside the built-in one.

```bash
adk-rust vision models fetch pp-ocrv6-tiny-det pp-ocrv6-tiny-rec rf-detr-n
adk-rust vision models verify
```

## `vision run`

| Command | Default backend | Other backends |
|---------|-----------------|----------------|
| `adk-rust vision run detect <image> [--classes a,b] [--threshold 0.5]` | `rf-detr-n` | any RF-DETR manifest id (`rf-detr-n-q4f16`, `rf-detr-s`, `rf-detr-m`, your own), `sam-3.1` by concept |
| `adk-rust vision run segment <image> (--box x0,y0,x1,y1 \| --point x,y \| --concept <text>)` | `efficientsam-ti` | `sam2.1-hiera-tiny`, `sam2.1-hiera-tiny-q4`, `sam-3.1` |
| `adk-rust vision run read <image> [--markdown]` | `pp-ocrv6-tiny` | `pp-ocrv6-small`, `pp-ocrv6-medium`, `paddleocr-vl-1.6`, `nim` |
| `adk-rust vision run describe <image> [--question <text>] [--model <id>] [--regions]` | `claude` (`claude-haiku-5-5`) | `gemini` (`gemini-3.7-flash`), `nim` (`nvidia/nemotron-3.5-super-vl-120b-a12b`) |

Every `run` takes:

| Flag | Does |
|------|------|
| `--backend <id>` | A backend other than the task's default |
| `--output table\|json\|regions` | Aligned columns (default), the result and its backend calls as JSON, or the `RegionSet` JSON of `adk-vision/schema/region-set.json` |
| `--annotate <png>` | Writes a copy of the image with the regions drawn on it |
| `--crop x0,y0,x1,y1` | Only this region, in source pixels; a remote backend is sent nothing else, as data class `image_crop` |
| `--data-class <class>` | The class the egress rules are written per (default `image`, or `image_crop` with `--crop`) |
| `--approve <digest>` | Releases exactly this egress digest without asking again |
| `--config <file>` | Egress rules other than `.adk/vision.toml` |
| `--base-url <url>` | A remote backend's base URL: NVIDIA NIM, which ships no default URL, or a proxy |

The table ends with each backend call: where it ran, its duration, its cost, and whether
anything left the machine.

A `run` whose on-device model is not cached fetches it first, as `models fetch` does: by its
pinned revision, every file's SHA-256 checked, the licence gate first. The first call can
therefore download the model — `rf-detr-n` is 108 MB. With `HF_HUB_OFFLINE=1` an uncached model
is refused instead. `models fetch` does the same download ahead of time.

```bash
adk-rust vision run read invoice.png --annotate invoice.read.png
adk-rust vision run detect street.jpg --classes person,car --output regions
```

### Keys

| Backend | Key |
|---------|-----|
| `claude` | `ANTHROPIC_API_KEY` |
| `gemini` | `GEMINI_API_KEY` or `GOOGLE_API_KEY` |
| `nim` (describe) | `NVIDIA_API_KEY`, and `--base-url` or `NVIDIA_NIM_URL` (hosted: `https://integrate.api.nvidia.com/v1`, under NVIDIA's API Trial Terms) |
| `nim` (read) | `NVIDIA_API_KEY`, and `--base-url` or `NVIDIA_NIM_CV_URL` (hosted: `https://ai.api.nvidia.com/v1/cv`) |
| `sam-3.1` | `META_API_KEY` |

The CLI's `describe` has no on-device backend, so it always uses a remote model through the
egress gate. In your own program, `LlmDescriber::mistralrs` (`adk-vision` feature `mistralrs`)
runs Qwen3-VL in the process — see [Local Describer](local-describer.md).

### A Remote Call Goes Through the Egress Gate

For a remote backend the CLI builds the gate from the project's rules — faces and personal
text are always redacted, whatever the file says — and releases only what the rules allow:

1. The image is cropped (with `--crop`) and resized to the backend's limit.
2. Faces and personal text are covered, on the device. Without `vision-local` there is no
   redactor, and the gate refuses the release at its redact step.
3. The rule for the host and data class decides: `deny` refuses; `allow` releases; `ask`
   holds the call.
4. A held call writes the exact image to `.adk/vision/held/` and prints the host, data class,
   what was covered and the egress digest. On a terminal it asks `send this image to <host>?`;
   without one it exits with status 2, and `--approve <digest>` releases that image, and only
   that image, on the next run.
5. The release is written to `.adk/vision/egress.jsonl` and the copy to `.adk/vision/media/`
   before the request is sent.

```bash
adk-rust vision run describe id-card.png --crop 20,70,620,390 \
  --question "What kind of document is this?"
# held: this image would be sent to api.anthropic.com as image_crop
#   redacted: 1 faces, 5 personal_text
#   egress digest: sha256:…
#   the exact image: .adk/vision/held/sha256-….png
# send this image to api.anthropic.com? [y/N]
```

## `vision egress log`

```bash
adk-rust vision egress log [--since 2026-11-19] [--output table|json]
```

Lists every release and refusal of the project: when, backend, destination, data class,
egress digest, and for a release the decision (allowed by a rule or approved by a person),
the redactions by class and the estimated cost; for a refusal, its reason and the gate step
that refused.

## `vision sources`

| Command | Does |
|---------|------|
| `adk-rust vision sources list [--output table\|json]` | The cameras and screens a user-installed `ffmpeg` lists on this machine — AVFoundation on macOS, v4l2 on Linux, DirectShow on Windows — with the index, kind and name each opens by. Listing needs no camera permission |
| `adk-rust vision sources test <source>` | Opens the source, samples a few frames, and reports the time to the first frame, the rate, the frame size and the brightness. Frames stay in memory; nothing is stored or sent |

`<source>` is a device from `sources list` (its index, its name, or `/dev/videoN` on Linux), an
`rtsp://` URL, an MP4 or MOV file, or `testsrc` for ffmpeg's test pattern. A user name and
password in an RTSP URL never appear in the output or an error.

| Flag | Does |
|------|------|
| `--frames <n>` | Frames to sample (default 5) |
| `--fps <rate>` | Frames a second to sample (default 2) |
| `--max-long-edge <px>` | Scales frames down to this long edge |
| `--frame-rate <fps>` | A camera's capture rate, one it offers (default 30) |
| `--video-size <WxH>` | A camera's capture size, one it offers, such as `1280x720` |
| `--timeout <secs>` | Seconds to wait for each frame (default 20) |
| `--output table\|json` | Aligned lines (default) or JSON |

```bash
adk-rust vision sources list
# ffmpeg 8.0.1 through avfoundation:
# index  kind    name                             test with
# 0      camera  Huddly GO                        adk-rust vision sources test 0
# 1      camera  OBS Virtual Camera               adk-rust vision sources test 1
# 2      screen  Capture screen 0                 adk-rust vision sources test 2

adk-rust vision sources test 0 --video-size 1280x720 --max-long-edge 640
adk-rust vision sources test 'rtsp://user:pass@10.0.0.8/stream1'   # shown as rtsp://10.0.0.8/stream1
adk-rust vision sources test testsrc                                # no camera needed
```

> **Note:** on macOS the first capture from a camera asks for camera permission for the
> terminal application. Until it is allowed in System Settings → Privacy & Security → Camera,
> the camera refuses to open and `sources test` says so; run the command again once it is
> allowed. A screen needs Screen Recording permission instead.

`ffmpeg` is never downloaded or bundled: `ADK_VISION_FFMPEG` names the binary, and `ffmpeg` on
the `PATH` is used otherwise ([Streams](streams.md#ffmpeg)).

## Related

- [Getting started: a Document desk](getting-started.md)
- [Vision overview](index.md): manifests, licences and the egress gate
- [Streams](streams.md): frame sources, local cameras included
- [Image Privacy](../security/image-privacy.md)
