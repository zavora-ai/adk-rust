//! The NVIDIA NIM document reader (spec 3.7 §7.7), feature `nim`.
//!
//! NVIDIA's CV endpoints (`<base>/<org>/<model>`, hosted on `ai.api.nvidia.com/v1/cv` or
//! self-hosted) take a non-OpenAI body with inline data URIs and answer in 0–1 of the image
//! sent: quads for OCR, class-keyed boxes for page, table and chart elements.
//! [`NimDocumentReader`] composes them into one [`DocumentPage`](crate::DocumentPage).
//!
//! | Status | Meaning | Maps to |
//! |--------|---------|---------|
//! | 404 `text/plain` | no such route | `NotFound`, naming the URL |
//! | 404 problem | the key is not entitled to the model | `Forbidden` |
//! | 410 | the endpoint is retired | `NotFound`, with the retirement date |
//! | 422 | the body was refused | `InvalidInput` |
//! | 400 | the input was refused | `Unsupported` |
//! | 429 | rate limited | `RateLimited`, with `Retry-After` |
//! | 500, 502, 503 | a worker error, retried once | `Unavailable` |
//! | 504 | the gateway's 120 s timeout | `Timeout` |
//!
//! The hosted endpoints run under NVIDIA's API Trial terms and suit prototypes; a
//! self-hosted NIM is free for development and research, and serving end users needs
//! NVIDIA AI Enterprise.

mod client;
mod config;
mod reader;
mod wire;

pub use config::{
    DEFAULT_MAX_INLINE_BYTES, DEFAULT_REQUESTS_PER_MINUTE, DEFAULT_TIMEOUT, NimCvConfig,
};
pub use reader::{
    DEFAULT_MIN_LAYOUT_CONFIDENCE, GRAPHIC_ELEMENTS_MODEL, NIM_READER_ID, NimDocumentReader,
    NimOcrModel, PAGE_ELEMENTS_MODEL, TABLE_STRUCTURE_MODEL,
};
