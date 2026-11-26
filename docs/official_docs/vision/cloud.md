# Cloud Vision Backends

`adk-vision` reaches cloud providers through three features: `cloud` turns any
`adk_core::Llm` that takes images into a `Describer`, `nim` adds NVIDIA NIM
vision-language models, and `sam` adds Meta's `sam-3.1` segmenter. Every one of them is a
remote backend: it takes only images the [egress gate](index.md#the-egress-gate) cropped,
redacted, decided and recorded, and every region it returns is mapped to source pixels by
the runtime.

```toml
[dependencies]
adk-vision = { version = "3.0.0", features = ["cloud", "nim", "sam"] }
```

| Feature | Backend | Trait | Answers in |
|---------|---------|-------|------------|
| `cloud` | `LlmDescriber` over Gemini, Claude, Muse or any `Llm` | `Describer` | The region adapter's frame |
| `nim` | `NimConfig::describer` for NVIDIA-hosted or self-hosted NIMs | `Describer` | 0–1000, x first, fixed by the prompt |
| `sam` | `SamSegmenter` for Meta `sam-3.1` | `Segmenter`, `Detector` | Pixels of the image as sent |

## Describing With Any Model

`LlmDescriber::remote` wraps a model and takes the hosts and providers it sends to from the
model (`Llm::data_destinations`): every `adk-model` provider names the host of its configured
endpoint — AI Studio or a Vertex AI location, a Bedrock region, an Azure resource, a custom
base URL — so the egress policy and the `host` and `provider` rules check where the images
actually go. `LlmDescriber::remote_with_hosts` names the hosts instead, for a model that names
none, such as your own `Llm`. The describer sends the released images as inline parts, the
caller's schema as the response schema and, when regions are asked for, the region format of
the adapter picked by model ID:

```rust,ignore
use std::sync::Arc;

use adk_core::Effort;
use adk_model::anthropic::AnthropicConfig;
use adk_model::AnthropicClient;
use adk_vision::{DescribeSpec, LlmDescriber, VisionRuntime};

let claude = AnthropicClient::new(AnthropicConfig::new(api_key, "claude-sonnet-5-5"))?;
let describer = LlmDescriber::remote(Arc::new(claude))?
    .with_effort(Effort::Low)
    .with_max_output_tokens(4_000);
let runtime = VisionRuntime::builder()
    .remote_describer(Arc::new(describer))
    .egress_gate(gate)
    .build()?;

let answer = runtime
    .describe(&cx, &[&screenshot], &DescribeSpec::new("Where is the Save button?").with_regions())
    .await?;
for region in &answer.value.regions {
    // Source pixels of the screenshot, whatever Claude was sent.
    println!("{:?} {:?}", region.label, region.shape.bbox());
}
```

A model with no adapter answers in text and values only; asking it for regions fails with
`Unsupported` rather than reading coordinates in a guessed frame. `LlmDescriber::local`
wraps a model in this process or on a loopback address, such as `adk-mistralrs` or a
`mistralrs serve` endpoint, and refuses a model whose `Llm::data_locality` is `Remote`.

### Region Adapters

| Model ID | Adapter | Frame | Limit the gate downscales to |
|----------|---------|-------|------------------------------|
| `gemini-*` | `GeminiBox2d` | `box_2d` as `[ymin, xmin, ymax, xmax]` on 0–1000 | none |
| `claude-*` 4.7 and later, Sonnet 5 and later, Haiku 5.5 | `ClaudePixels` | Pixels of the image as sent | 2,576 px long edge, 3,605,000 pixels |
| Earlier `claude-*` | `ClaudePixels` | Pixels of the image as sent | 1,568 px long edge, 1,150,000 pixels |
| `nvidia/nemotron-*` vision-language models | `Normalized1000::nemotron` | `[x_min, y_min, x_max, y_max]` on 0–1000, fixed by the prompt | none |
| `google/gemma-4-*` | `Normalized1000::gemma4` | the same | none |
| `muse-*` | `Normalized1000::muse` | the same | none |
| `qwen3-vl-*` (on this machine) | `Normalized1000::qwen3_vl` | the same, Qwen3-VL's own frame; no response schema | none — see [Local Describer](local-describer.md) |

- **Claude.** Claude answers in pixels of the image it processed, after resizing an image
  above its tier. The adapter's limit keeps every released image inside the tier, so Claude
  never resizes and the gate's resize is on the transform chain. 3,605,000 pixels is the
  most for which `⌈w/28⌉ × ⌈h/28⌉` stays within 4,784 visual tokens on any aspect ratio.
- **Gemini.** Masks are read in both documented forms when an answer carries one: a `mask`
  PNG probability map inside the box, thresholded at 127, or a `polygon` of `[y, x]` points
  on 0–1000. `GeminiBox2d::with_masks` asks for one form; none is asked for by default.
- **Prompt-fixed grids.** NVIDIA's models and Muse answer in different frames for
  different aspect ratios when left to choose, so the prompt states the 0–1000 grid. The
  prompt is versioned (`PROMPT_VERSION`), and answers may name the box `bbox`, `bbox_2d`,
  `box_2d` or `box`.

`with_adapter` replaces the adapter, for a model the table does not list.

### Spend and Preflight

A remote describer is token-billed. Before an image is released, the runtime holds the
describer's `TokenEstimate` — the provider's image-token formula at the catalog's rates,
with the highest input rate and the call's output cap — so a call the run's budget cannot
cover is refused before anything leaves. The model call then runs under the call's
`VisionCx` scope, holds its own bound sized the same way, and commits the tokens the
response reports; the runtime records the call's images as units at zero cost.

| Provider | Image tokens (`ImageTokens`) |
|----------|------------------------------|
| Claude 4.7 and later | `Claude`: `⌈w/28⌉ × ⌈h/28⌉` |
| Earlier Claude | `Fixed`: 1,568, the most `w × h / 750` reaches within the tier's limit |
| Gemini | `GeminiTiles`: 258 for an image up to 384 px a side, otherwise 258 per 768 × 768 tile |
| NVIDIA Nemotron VL and Nano Omni, Muse | `Tiles { side: 32 }`: `⌈w/32⌉ × ⌈h/32⌉`, an upper bound for images above about 3.4 megapixels, which the services downscale |
| Gemma 4 on NVIDIA | `Fixed`: 260 |

`LlmDescriber::preflight` returns the estimate for given image sizes. The Claude formula
matches `count_tokens` on `claude-sonnet-5-5` and `claude-haiku-5-5`, which add 3 tokens
for the image block.

## NVIDIA NIM

`NimConfig` takes the base URL, the key and the model; nothing ships a default URL. The
hosted `https://integrate.api.nvidia.com/v1` runs under NVIDIA's API Trial Terms and suits
prototypes. A self-hosted NIM is free for development, testing and research on up to 16
GPUs across two nodes; serving end users needs NVIDIA AI Enterprise.

```rust,ignore
use std::sync::Arc;
use adk_vision::cloud::nim::NimConfig;

let config = NimConfig::new(
    "https://integrate.api.nvidia.com/v1",
    std::env::var("NVIDIA_API_KEY")?,
    "nvidia/nemotron-3.5-super-vl-120b-a12b",
);
let describer = config.describer()?; // remote, host integrate.api.nvidia.com
```

- **Reasoning off.** Nemotron models reason by default, and their reasoning counts toward
  `max_tokens`; perception calls send `chat_template_kwargs.enable_thinking: false`. An
  empty answer at the output cap is an error, not an empty description.
- **No price.** NVIDIA publishes no per-call price, so a NIM describer holds and commits 0
  per call (`with_unpriced_hold`); cap it with a per-unit `Images` cap.
- **Inline images.** Images go as data URLs; NVIDIA's asset upload API is retired.

## Meta `sam-3.1`

`SamSegmenter` sends one `POST /v1/responses` request per concept phrase with the released
image, streams the answer through `response.completed`, and parses Meta's special-token
lines: per object, a box in source pixels and a mask covering that box. Box and point
prompts go in one request in Meta's object-prompt grammar; mask prompts are refused.

```rust,ignore
use std::sync::Arc;
use adk_vision::{SamConfig, SamSegmenter, SegmentPrompt};

let sam = SamSegmenter::new(SamConfig::from_env()?)?; // META_API_KEY, Muse's base URL
let runtime = VisionRuntime::builder()
    .remote_segmenter(Arc::new(sam))
    .egress_gate(gate)
    .build()?;
let found = runtime
    .segment(&cx, &page, &[SegmentPrompt::Concept { text: "stamp".into() }])
    .await?;
```

| Item | Behaviour |
|------|-----------|
| Credentials | The Meta Model API key Muse uses: `META_API_KEY`, else `MODEL_API_KEY` |
| Price | $2.50 per 1,000 images from the `adk-model` catalog: 2,500 µUSD and one image unit per request |
| Several concepts | One request each; the runtime holds all of them before the first is sent, so a budget that cannot cover them refuses the call |
| Rate limits | On 429 the client waits for `Retry-After`, or backs off exponentially with jitter, up to three retries, then fails with `RateLimited` and the hint |
| No match | An empty answer, still billed |
| Confidence | `SamConfig::with_confidence(true)` returns each object's confidence as its score |

### Masks

Each mask is base85 text around an arithmetic-coded body whose model only Meta's
`@meta-sam/parser` defines, and Meta publishes that parser under the SAM License, not an
open-source licence. ADK-Rust's Apache-2.0 crates therefore contain no SAM-licensed code, and
masks come through one of three choices:

| Choice | Each object is | What you need | Licence you take on |
|--------|----------------|---------------|---------------------|
| `boxes` (the default) | Its box in source pixels | Nothing | None |
| `external` | Its mask, decoded by a program you installed, such as Meta's parser in Node.js | The program | The program's own; Meta's parser is under the SAM License |
| `licensed` | Its mask, decoded in this process by `adk-vision-sam-codec`, a Rust port of Meta's codec | Feature `sam-codec` (CLI: `vision-sam-codec`) and a recorded acceptance | The SAM License |

**Choosing `licensed`.** The SAM License lets anyone port and redistribute Meta's code, on
its terms: the port must be distributed under the SAM License with a copy of it (clause
1.b.i), and its use must not be for the purposes clause 1.b.v forbids (military, warfare,
nuclear, espionage, weapons, or anything Trade Controls prohibit). So the port lives in its
own crate, `adk-vision-sam-codec`, whose licence file is the SAM License and whose `NOTICE`
names what was ported from where; `adk-vision` reaches it only through the opt-in
`sam-codec` feature, which is in no `adk-rust` tier. A program you build with that feature
contains SAM-licensed code, and distributing it binds you to the SAM License. The decoder
refuses until a person accepts the licence from a terminal, the same way a use-restricted
model is accepted:

```text
$ adk-rust vision models accept sam-mask-codec
sam-mask-codec is under LicenseRef-SAM (use-restricted).
Read the licence before you accept it: https://github.com/meta-models/meta-sam/blob/5f2e6e7f1da5b41dcd10bec971201e5910efc18a/LICENSE
Type the model id to accept its licence as alice: sam-mask-codec
```

Until then, building the segmenter fails with `licence acceptance required`, naming the
licence URL and that command. The acceptance is recorded in `.adk/vision/licences.json`.

**Choosing `external`.** The program is started once per answer with its arguments. It reads
a JSON array of `{"encoding", "width", "height", "payload"}` on standard input, where
`encoding` is `lossless` or `one_bit` and `payload` is the mask text exactly as the API sent
it, and writes a JSON array with one base64 string per mask: `height × width` bytes,
row-major, nonzero for a set pixel. The segmenter checks that the program exists when it is
built, stops it after `timeout_secs` (60 by default), and refuses an answer when the program
exits non-zero (with its first line of standard error), writes something other than that
array, or writes a raster of the wrong size. `ExternalMaskDecoder::meta_parser` runs Meta's
parser in a user-installed Node.js, the way the video backends use a user-installed
`ffmpeg`:

```rust,ignore
use std::sync::Arc;
use adk_vision::sam::ExternalMaskDecoder;

// After `npm install @meta-sam/parser`, which accepts its licence.
let sam = SamSegmenter::new(SamConfig::from_env()?)?.with_mask_decoder(Arc::new(
    ExternalMaskDecoder::meta_parser("node_modules/@meta-sam/parser"),
));
```

**Configuring the choice.** In a program, `SamConfig::with_mask_decoder` takes a
`MaskDecoderChoice`, and `SamSegmenter::new` builds and checks the decoder:

```rust,ignore
use std::sync::Arc;
use adk_vision::{FileAcceptanceStore, MaskDecoderChoice, SamConfig, SamSegmenter};

let licensed = SamConfig::from_env()?
    .with_mask_decoder(MaskDecoderChoice::Licensed)
    .with_licence_acceptances(Arc::new(FileAcceptanceStore::in_project(&project)));
let sam = SamSegmenter::new(licensed)?; // LicenceAcceptanceRequired until accepted

let external = SamConfig::from_env()?
    .with_mask_decoder(MaskDecoderChoice::external("/opt/sam/bin/sam-mask-decode", ["--json"]));
```

In `.adk/vision.toml`, the `[sam.masks]` table chooses for `adk-rust vision` and anything else
that reads the file:

```toml
[sam.masks]
decoder = "external"          # "boxes" (default), "external" or "licensed"
program = "/opt/sam/bin/sam-mask-decode"
args = ["--json"]
timeout_secs = 30
```

On the command line, `--mask-decoder boxes|external|licensed`, `--mask-decoder-program`,
`--mask-decoder-arg` and `--mask-decoder-timeout` override the file for one call:

```bash
adk-rust vision run segment shapes.png --backend sam-3.1 --concept "blue circle" \
  --mask-decoder licensed --annotate shapes.masks.png
```

**Placement.** A raster covers its box: small boxes come back at source resolution, large
ones downscaled (a 1699 × 398 box arrived as 1024 × 240), and the segmenter stretches each
raster over its box into a mask the size of the source image.

**Accuracy.** The port decodes all 16 masks of the recorded streams to the same rasters,
bit for bit, as Meta's parser 0.1.0 (their SHA-256 digests are the test oracle), and Meta's
own conformance vectors to their published rasters. Against the drawn shapes of the
960 × 540 test image, the recorded masks score:

| Shape | Mask IoU | Its box alone |
|-------|----------|---------------|
| Red rectangle | 0.986 | 0.990 |
| Blue circle | 0.992 | 0.792 |
| Green triangle | 0.976 | 0.503 |

A rectangle's box is already its shape; for the circle and the triangle, only a decoded mask
describes the object.

## Live Results

Recorded on 2026-11-19 against the 3024 × 1964 S5 screenshot (twelve drawn widgets) and a
960 × 540 image of three shapes and two text lines, with the largest edge error in source
pixels:

| Backend | S5 widgets found | S5 worst edge error | Shapes worst edge error | Cost of both calls |
|---------|------------------|---------------------|-------------------------|--------------------|
| `claude-sonnet-5-5` | 12 / 12 | 0.8 px | 0 px (shapes), 6 px (text) | $0.021 |
| `claude-haiku-5-5` | 12 / 12 | 0.9 px | 0 px (shapes), 5 px (text) | $0.0003 |
| `sam-3.1` (`rectangle`, then one concept per shape) | 12 / 12 | 2 px | 1 px | $0.010 |
| `nvidia/nemotron-3.5-super-vl-120b-a12b` | 12 / 12 | 21 px | 12 px | no price |
| `google/gemma-4-31b-it` | 12 / 12, one mislabelled | 695 px | 9 px | no price |
| `muse-spark-1.3` | 12 / 12 | 39 px | 56 px | $0.016 |

The crate's contract tests replay these recordings through the real `adk-model` clients,
so the PR tier checks them without keys.

## Related

- [Vision](index.md)
- [Egress control](../security/egress.md)
- [Spec 3.7](../../design/spec-3.7-vision.md)
