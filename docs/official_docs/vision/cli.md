# The `adk-rust vision` CLI

`adk-rust vision` manages the model manifest and licence acceptance, runs one perception call
on an image — on the device first, through the egress gate when the backend is remote — and
shows what left the machine (spec 3.7 §8.1).

```bash
cargo install adk-cli --features vision-local   # the on-device backends
cargo install adk-cli --features vision         # cloud backends only, pure Rust
```

| Feature | Commands and backends |
|---------|-----------------------|
| `vision` | `models`, `egress log`, and `run` with the remote backends: Claude, Gemini and NVIDIA NIM describers, NVIDIA NIM's document reader, Meta `sam-3.1` |
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

An on-device describer over mistral.rs is not available yet, so `describe` always uses a
remote model.

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

## Related

- [Getting started: a Document desk](getting-started.md)
- [Vision overview](index.md): manifests, licences and the egress gate
- [Image Privacy](../security/image-privacy.md)
