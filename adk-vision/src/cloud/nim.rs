//! NVIDIA NIM vision-language models (spec 3.7 §7.7), feature `nim`.

use std::sync::Arc;

use adk_model::openai::{OpenAICompatible, OpenAICompatibleConfig};

use super::LlmDescriber;
use crate::error::{Result, VisionError};
use crate::sent::SentImage;

/// Where an NVIDIA NIM serves its OpenAI-compatible routes, and which model to call.
///
/// The adapter is base-URL only: the caller supplies NVIDIA's hosted
/// `https://integrate.api.nvidia.com/v1` or a self-hosted NIM's URL, and nothing ships a
/// default. The hosted endpoint runs under NVIDIA's API Trial Terms and suits prototypes;
/// a self-hosted NIM is free for development, testing and research on up to 16 GPUs
/// across two nodes; serving end users needs NVIDIA AI Enterprise.
///
/// Images go inline as data URLs; NVIDIA's asset upload API is retired. NVIDIA publishes no
/// per-call price, so [`describer`](Self::describer) holds and commits 0 per call and unit
/// caps bound it.
///
/// | Model | Regions | Image tokens |
/// |-------|---------|--------------|
/// | `nvidia/nemotron-3.5-super-vl-120b-a12b` | 0–1000, x first, reasoning off | `⌈w/32⌉ × ⌈h/32⌉` |
/// | `google/gemma-4-31b-it` | 0–1000, x first | about 260 |
/// | `nvidia/nemotron-3-nano-omni-30b-a3b-reasoning` | 0–1000, x first, coarse, reasoning off | `⌈w/32⌉ × ⌈h/32⌉` |
///
/// # Example
///
/// ```
/// use adk_vision::cloud::nim::NimConfig;
/// use adk_vision::{AxisOrder, CoordFrame, Describer, Locality};
///
/// let config = NimConfig::new(
///     "https://integrate.api.nvidia.com/v1",
///     "nvapi-key",
///     "nvidia/nemotron-3.5-super-vl-120b-a12b",
/// );
/// let describer = config.describer().unwrap();
/// let descriptor = describer.descriptor();
/// assert_eq!(descriptor.frame, CoordFrame::Normalized { scale: 1000, order: AxisOrder::Xy });
/// assert_eq!(
///     descriptor.locality,
///     Locality::Remote { hosts: vec!["integrate.api.nvidia.com".into()] }
/// );
/// ```
#[derive(Clone)]
pub struct NimConfig {
    base_url: String,
    api_key: String,
    model: String,
}

impl std::fmt::Debug for NimConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NimConfig")
            .field("base_url", &self.base_url)
            .field("api_key", &"[REDACTED]")
            .field("model", &self.model)
            .finish()
    }
}

impl NimConfig {
    /// A NIM at `base_url` (ending in `/v1`), called with `api_key` for `model`.
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self { base_url: base_url.into(), api_key: api_key.into(), model: model.into() }
    }

    /// The host the NIM is reached at.
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] for a URL with no host.
    pub fn host(&self) -> Result<String> {
        let rest = self.base_url.split_once("://").map_or(self.base_url.as_str(), |(_, r)| r);
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        let host = match authority.strip_prefix('[') {
            Some(v6) => v6.split(']').next().unwrap_or_default(),
            None => authority.split(':').next().unwrap_or_default(),
        };
        if host.is_empty() {
            return Err(VisionError::InvalidInput(format!(
                "the NIM base URL `{}` names no host",
                self.base_url
            )));
        }
        Ok(host.to_ascii_lowercase())
    }

    /// The OpenAI-compatible client for the NIM, reported to spend keys as `nvidia`.
    ///
    /// # Errors
    ///
    /// The client's construction error.
    pub fn client(&self) -> Result<OpenAICompatible> {
        let config = OpenAICompatibleConfig::new(self.api_key.clone(), self.model.clone())
            .with_provider_name("nvidia")
            .with_base_url(self.base_url.clone());
        Ok(OpenAICompatible::new(config)?)
    }

    /// A describer for a hosted or remote NIM: it takes only released images, and the
    /// egress policy checks the NIM's host.
    ///
    /// # Errors
    ///
    /// The client's construction error, or a URL with no host.
    pub fn describer(&self) -> Result<LlmDescriber> {
        Ok(LlmDescriber::remote(Arc::new(self.client()?), [self.host()?])?.with_unpriced_hold(0))
    }

    /// A describer for a NIM on a loopback address.
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] when the base URL is not a loopback address.
    pub fn local_describer(&self) -> Result<LlmDescriber<SentImage>> {
        LlmDescriber::local(Arc::new(self.client()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_are_read_from_the_base_url() {
        let host = |url: &str| NimConfig::new(url, "k", "m").host().unwrap();
        assert_eq!(host("https://integrate.api.nvidia.com/v1"), "integrate.api.nvidia.com");
        assert_eq!(host("http://10.0.0.5:8000/v1"), "10.0.0.5");
        assert_eq!(host("http://[::1]:8000/v1"), "::1");
        assert!(NimConfig::new("https:///v1", "k", "m").host().is_err());
    }

    #[test]
    fn a_loopback_nim_is_local_and_a_remote_one_is_not() {
        let local = NimConfig::new(
            "http://127.0.0.1:8000/v1",
            "k",
            "nvidia/nemotron-3.5-super-vl-120b-a12b",
        );
        assert!(local.local_describer().is_ok());
        let hosted =
            NimConfig::new("https://integrate.api.nvidia.com/v1", "k", "google/gemma-4-31b-it");
        assert!(hosted.local_describer().is_err());
        assert!(hosted.describer().is_ok());
    }

    #[test]
    fn the_key_is_never_printed() {
        let config = NimConfig::new("https://integrate.api.nvidia.com/v1", "nvapi-secret", "m");
        assert!(!format!("{config:?}").contains("nvapi-secret"));
    }
}
