# adk-vision-sam-codec

Decodes Meta `sam-3.1` mask payloads into rasters. A Rust port of the mask codec in Meta's
[`@meta-sam/parser`](https://www.npmjs.com/package/@meta-sam/parser) 0.1.0, distributed under
the **SAM License**, not under ADK-Rust's Apache-2.0.

> **Important:** this crate is a derivative work of Meta's SAM Materials. Using or distributing
> it binds you to the [SAM License](LICENSE), including its use restrictions. Read
> [NOTICE](NOTICE) for what was ported and from where.

## What it does

A `sam-3.1` mask token carries `data=H,W,<payload>`. The payload is base85 text whose first
character selects the encoding — `~` lossless (the API default) or `!` one_bit — around an
arithmetic-coded body. `decode_mask_to_raster` returns the row-major `H × W` raster of 0 and 1
bytes that Meta's `decodeMaskToRaster` returns, and refuses what Meta's decoder refuses.

| Item | Behaviour |
|------|-----------|
| Encodings | `lossless` (coverage bytes thresholded at 129) and `one_bit` |
| Checks | Meta's: alphabet, length prefix, canonical base85 groups and tail, coder finalization, canonical `one_bit` round trip |
| Bound | The caller's largest raster area, checked before any allocation |
| Dependencies | `thiserror` only; no I/O, no unsafe code |

## Example

```rust
use adk_vision_sam_codec::{EncodedMask, Encoding, decode_mask_to_raster};

let mask = EncodedMask {
    encoding: Encoding::OneBit,
    width: 5,
    height: 5,
    payload: "!!!!!(QO(0lu8?",
};
let raster = decode_mask_to_raster(&mask, 25).expect("a valid payload");
assert_eq!(raster.len(), 25);
```

## Using it from adk-vision

`adk-vision` reaches this crate only through its opt-in `sam-codec` feature, which is in no
`adk-rust` tier. `MaskDecoderChoice::Licensed` then decodes masks in process, and refuses with
"licence acceptance required" until a person records acceptance with
`adk-rust vision models accept sam-mask-codec`. See
[`docs/official_docs/vision/cloud.md`](../docs/official_docs/vision/cloud.md).

## Licence

[SAM License](LICENSE), Last Updated: November 19, 2025. Meta provides the SAM Materials "as is"
and offers no support for them; Meta may modify the licence. Research published using this crate
must acknowledge the use of SAM Materials.
