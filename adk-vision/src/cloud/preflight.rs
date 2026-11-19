//! [`Preflight`]: what a describer call can cost, estimated before its request (spec 3.7
//! §7.9).

use std::sync::Arc;

use adk_core::{Llm, LlmRequest, LlmResponseStream, Part, SchemaAdapter};
use adk_model::pricing::{PricingCatalog, request_input_token_bound};
use async_trait::async_trait;

use super::adapter::RegionAdapter;

/// The estimate a describer call holds against the run's budget before its request.
///
/// Images count by the provider's formula ([`RegionAdapter::image_tokens`]) instead of
/// one token per byte of the encoded image, so a screenshot is held at the thousands of
/// tokens it costs rather than the millions its PNG bytes would bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Preflight {
    /// Input tokens of the images, by the provider's formula.
    pub image_tokens: u64,
    /// Input tokens of the whole request: the images, and the text bounded at one token
    /// per byte.
    pub input_tokens: u64,
    /// The output cap the estimate assumes: the request's, else the model's maximum.
    pub output_tokens: Option<u64>,
    /// The cost bound in micro-USD, or `None` when the catalog does not price the model.
    pub max_cost_micro_usd: Option<u64>,
}

impl Preflight {
    pub(crate) fn estimate(
        model: &str,
        request: &LlmRequest,
        adapter: Option<&dyn RegionAdapter>,
        sizes: &[(u32, u32)],
    ) -> Self {
        let image_tokens: u64 = match adapter {
            Some(adapter) => {
                let formula = adapter.image_tokens();
                sizes.iter().map(|s| formula.tokens(*s)).sum()
            }
            // No formula: the generic bound of one token per byte stands.
            None => 0,
        };
        let mut text_only = request.clone();
        for content in &mut text_only.contents {
            content.parts.retain(|p| !matches!(p, Part::InlineData { .. }));
        }
        let text_tokens = request_input_token_bound(&text_only).unwrap_or(0);
        let input_tokens = text_tokens.saturating_add(image_tokens);
        let catalog = PricingCatalog::standard();
        let price = catalog.lookup(None, model);
        let output_tokens = request
            .config
            .as_ref()
            .and_then(|c| c.max_output_tokens)
            .and_then(|t| u64::try_from(t).ok())
            .or_else(|| price.and_then(|p| p.max_output_tokens));
        let max_cost_micro_usd =
            match (adapter, price, output_tokens) {
                (Some(_), Some(price), Some(output)) => {
                    let today = chrono::Utc::now().date_naive();
                    let tomorrow = today.succ_opt().unwrap_or(today);
                    let usd = price
                        .max_cost_usd(input_tokens, output, today)
                        .max(price.max_cost_usd(input_tokens, output, tomorrow));
                    adk_core::usd_to_micro_usd(usd).map(|m| m.saturating_add(1))
                }
                _ => None,
            };
        Self { image_tokens, input_tokens, output_tokens, max_cost_micro_usd }
    }
}

/// The runtime's preflight for a priced model: its image-token formula and catalog rates,
/// at the highest input rate (cache writes included) so the hold bounds the call. `None`
/// when the catalog does not price the model.
pub(crate) fn token_estimate(
    model: &str,
    images: crate::ImageTokens,
    max_output_tokens: Option<u32>,
) -> Option<crate::TokenEstimate> {
    let price = PricingCatalog::standard().lookup(None, model)?;
    let rates = price.rates_for(0, chrono::Utc::now().date_naive());
    let input = rates.input.max(rates.cache_write_5m).max(rates.cache_write_1h);
    let per_million = |usd: f64| (usd * 1_000_000.0).ceil() as u64;
    Some(
        crate::TokenEstimate::new(images, per_million(input), per_million(rates.output))
            .with_max_output_tokens(max_output_tokens.unwrap_or(4_096)),
    )
}

/// The describer's model, with its spend hold sized by the preflight estimate.
pub(crate) struct Preflighted {
    inner: Arc<dyn Llm>,
    bound: Option<u64>,
}

impl Preflighted {
    pub(crate) fn new(inner: Arc<dyn Llm>, bound: Option<u64>) -> Self {
        Self { inner, bound }
    }
}

#[async_trait]
impl Llm for Preflighted {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn generate_content(
        &self,
        req: LlmRequest,
        stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        self.inner.generate_content(req, stream).await
    }

    fn schema_adapter(&self) -> &dyn SchemaAdapter {
        self.inner.schema_adapter()
    }

    fn uses_interactions_api(&self) -> bool {
        self.inner.uses_interactions_api()
    }

    fn max_cost_micro_usd(&self, request: &LlmRequest) -> Option<u64> {
        self.bound.or_else(|| self.inner.max_cost_micro_usd(request))
    }

    fn context_window(&self) -> Option<u64> {
        self.inner.context_window()
    }

    fn data_locality(&self) -> adk_core::DataLocality {
        self.inner.data_locality()
    }
}
