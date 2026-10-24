//! The kernel's chat channel contract (spec 3.3 §10 K1), run against the Telegram adapter
//! over a fake Bot API: what the suite reads as the wire is every call the adapter made to
//! Telegram, and what it types and presses goes through the adapter's own reading of
//! Telegram updates. The suite forges some answers with its own account and thread names, so
//! the fake's updates carry those names as the account's and the chat's ids.

#![cfg(feature = "channels")]

mod fake_telegram;

use std::sync::Arc;

use adk_kernel::channels::{
    ChannelAnswer, ChannelInbound, ChannelNotice, ChannelSink, DecisionCard, StopNotice,
};
use adk_kernel::conformance::{ChannelAdapter, ChannelAdapterHarness};
use adk_surface::channels::TelegramAdapter;
use async_trait::async_trait;
use fake_telegram::{FakeTelegram, TOKEN};
use serde_json::{Value, json};

/// The adapter and the fake API it talks to.
struct Telegram {
    adapter: TelegramAdapter,
    api: FakeTelegram,
}

impl Telegram {
    /// The calls addressed to the suite's thread `thread`.
    fn calls(&self, thread: &str) -> Vec<Value> {
        self.api.to_chat(&json!(thread)).into_iter().map(|call| call.body).collect()
    }
}

#[async_trait]
impl ChannelSink for Telegram {
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
impl ChannelAdapter for Telegram {
    async fn wire(&self, thread: &str) -> Vec<String> {
        self.calls(thread).iter().map(Value::to_string).collect()
    }

    async fn typed(&self, account: &str, thread: &str, text: &str) -> ChannelInbound {
        let update = self.api.text_update(account, thread, text);
        self.adapter.inbound(&update).expect("a private text message reaches the gateway")
    }

    async fn pressed(
        &self,
        account: &str,
        thread: &str,
        answer: ChannelAnswer,
    ) -> Option<ChannelInbound> {
        let update = self.api.press_update(account, thread, answer.label())?;
        self.adapter.inbound(&update)
    }

    /// The code as the person reads it in the pairing message.
    async fn pairing_code(&self, thread: &str) -> Option<String> {
        self.calls(thread).iter().rev().find_map(|call| {
            let text = call["text"].as_str()?;
            let code = text.split("adk-rust pair accept ").nth(1)?;
            let code: String =
                code.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
            (!code.is_empty()).then_some(code)
        })
    }
}

struct Harness;

#[async_trait]
impl ChannelAdapterHarness for Harness {
    async fn adapter(&self) -> Arc<dyn ChannelAdapter> {
        let api = FakeTelegram::start().await;
        let adapter = TelegramAdapter::builder(TOKEN).api_base(api.url()).build().unwrap();
        Arc::new(Telegram { adapter, api })
    }
}

adk_kernel::channel_contract_tests!(harness = Harness);
