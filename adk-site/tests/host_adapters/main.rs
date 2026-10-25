//! Host adapters under the host conformance suite (spec 3.5 §7.8 "Hosts", §7.8.3, §11
//! "Conformance"). Each adapter runs against an in-process fake of its provider's API that
//! answers in the provider's recorded response shapes and checks request authentication
//! independently, or against a local `git` remote or `rsync` target. Runs against real
//! providers and emulators are `#[ignore]`d and read their endpoints and credentials from
//! the environment; each says how to run it.
#![cfg(any(
    feature = "host-s3",
    feature = "host-gcs",
    feature = "host-azure",
    feature = "host-netlify",
    feature = "host-github-pages",
    feature = "host-rsync"
))]

mod support;

#[cfg(feature = "host-azure")]
mod azure;
#[cfg(feature = "host-cloudflare")]
mod cloudflare;

#[cfg(feature = "host-gcs")]
mod gcs;
#[cfg(feature = "host-github-pages")]
mod github_pages;
#[cfg(feature = "host-netlify")]
mod netlify;
#[cfg(any(feature = "host-s3", feature = "host-gcs", feature = "host-azure"))]
mod object;
#[cfg(feature = "host-rsync")]
mod rsync;
#[cfg(feature = "host-s3")]
mod s3;
