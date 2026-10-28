//! The kernel's chat channel contract (spec 3.3 §10 K1), run against the Slack adapter
//! (spec 3.5 §10 X12) over a fake Web API: what the suite reads as the wire is every call the
//! adapter made to Slack, and what it types and presses goes through the adapter's own reading
//! of Socket Mode envelopes, with the suite's account and thread names as Slack's user and
//! conversation ids.

#![cfg(feature = "slack")]

mod chat_support;
mod fake_slack;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use adk_kernel::channels::{
    ChannelAnswer, ChannelInbound, ChannelNotice, ChannelSink, DecisionCard, StopNotice,
};
use adk_kernel::conformance::{ChannelAdapter, ChannelAdapterHarness};
use adk_surface::channels::SlackAdapter;
use async_trait::async_trait;
use fake_slack::{APP_TOKEN, BOT_TOKEN, FakeSlack};

/// Slack's message timestamps, one per typed message.
static TS: AtomicU64 = AtomicU64::new(1);

struct Slack {
    adapter: SlackAdapter,
    api: FakeSlack,
}

#[async_trait]
impl ChannelSink for Slack {
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
impl ChannelAdapter for Slack {
    async fn wire(&self, thread: &str) -> Vec<String> {
        self.api.to_channel(thread).iter().map(|call| call.body.to_string()).collect()
    }

    async fn typed(&self, account: &str, thread: &str, text: &str) -> ChannelInbound {
        let ts = format!("{}.000100", TS.fetch_add(1, Ordering::SeqCst));
        let envelope = FakeSlack::message(account, thread, text, &ts);
        self.adapter.inbound(&envelope).expect("a direct message reaches the gateway")
    }

    async fn pressed(
        &self,
        account: &str,
        thread: &str,
        answer: ChannelAnswer,
    ) -> Option<ChannelInbound> {
        let envelope = self.api.press_envelope(account, thread, answer.label())?;
        self.adapter.inbound(&envelope)
    }

    async fn pairing_code(&self, thread: &str) -> Option<String> {
        self.api.texts(thread).iter().rev().find_map(|text| chat_support::code_in(text))
    }
}

struct Harness;

#[async_trait]
impl ChannelAdapterHarness for Harness {
    async fn adapter(&self) -> Arc<dyn ChannelAdapter> {
        let api = FakeSlack::start().await;
        let adapter =
            SlackAdapter::builder(APP_TOKEN, BOT_TOKEN).api_base(api.url()).build().unwrap();
        Arc::new(Slack { adapter, api })
    }
}

adk_kernel::channel_contract_tests!(harness = Harness);
