//! The chat channel contract (spec 3.3 §10 K1) on the in-memory test channel: the suite
//! every platform adapter passes.

#![cfg(feature = "channels")]

use std::sync::Arc;

use adk_kernel::channels::InMemoryChannel;
use adk_kernel::conformance::{ChannelAdapter, ChannelAdapterHarness};

struct InMemory;

#[async_trait::async_trait]
impl ChannelAdapterHarness for InMemory {
    async fn adapter(&self) -> Arc<dyn ChannelAdapter> {
        Arc::new(InMemoryChannel::new("chat"))
    }
}

mod in_memory {
    use super::*;

    adk_kernel::channel_contract_tests! { harness = InMemory }
}
