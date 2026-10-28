//! The kernel's chat channel contract (spec 3.3 §10 K1), run against the Discord adapter
//! (spec 3.5 §10 X12) over a fake REST API: what the suite reads as the wire is every call the
//! adapter made to a channel, and what it types and presses goes through the adapter's own
//! reading of Gateway dispatches, with the suite's account and thread names as Discord's user
//! and channel ids.

#![cfg(feature = "discord")]

mod chat_support;
mod fake_discord;

use std::sync::Arc;

use adk_kernel::channels::{
    ChannelAnswer, ChannelInbound, ChannelNotice, ChannelSink, DecisionCard, StopNotice,
};
use adk_kernel::conformance::{ChannelAdapter, ChannelAdapterHarness};
use adk_surface::channels::DiscordAdapter;
use async_trait::async_trait;
use fake_discord::{FakeDiscord, TOKEN};
use serde_json::json;

struct Discord {
    adapter: DiscordAdapter,
    api: FakeDiscord,
}

#[async_trait]
impl ChannelSink for Discord {
    fn channel(&self) -> &str {
        self.adapter.channel()
    }

    async fn send_message(&self, thread: &str, text: &str) -> adk_kernel::Result<()> {
        self.adapter.send_message(thread, text).await
    }

    async fn send_decision(&self, thread: &str, card: &DecisionCard) -> adk_kernel::Result<()> {
        self.adapter.send_decision(thread, card).await
    }

    async fn send_stopped(&self, thread: &str, notice: &StopNotice) -> adk_kernel::Result<()> {
        self.adapter.send_stopped(thread, notice).await
    }

    async fn send_notice(&self, thread: &str, notice: &ChannelNotice) -> adk_kernel::Result<()> {
        self.adapter.send_notice(thread, notice).await
    }
}

#[async_trait]
impl ChannelAdapter for Discord {
    async fn wire(&self, thread: &str) -> Vec<String> {
        let route = format!("/channels/{thread}/");
        let calls = self.api.calls().into_iter().filter(|call| call.route.starts_with(&route));
        calls.map(|call| call.body.to_string()).collect()
    }

    async fn typed(&self, account: &str, thread: &str, text: &str) -> ChannelInbound {
        let d = self.api.message_event(account, thread, text);
        let dispatch = json!({ "op": 0, "s": 1, "t": "MESSAGE_CREATE", "d": d });
        self.adapter.inbound(&dispatch).expect("a direct message reaches the gateway")
    }

    async fn pressed(
        &self,
        account: &str,
        thread: &str,
        answer: ChannelAnswer,
    ) -> Option<ChannelInbound> {
        let d = self.api.press_event(account, thread, answer.label())?;
        self.adapter.inbound(&json!({ "op": 0, "s": 2, "t": "INTERACTION_CREATE", "d": d }))
    }

    async fn pairing_code(&self, thread: &str) -> Option<String> {
        self.api.contents(thread).iter().rev().find_map(|text| chat_support::code_in(text))
    }
}

struct Harness;

#[async_trait]
impl ChannelAdapterHarness for Harness {
    async fn adapter(&self) -> Arc<dyn ChannelAdapter> {
        let api = FakeDiscord::start().await;
        let adapter = DiscordAdapter::builder(TOKEN).api_base(api.url()).build().unwrap();
        Arc::new(Discord { adapter, api })
    }
}

adk_kernel::channel_contract_tests!(harness = Harness);
