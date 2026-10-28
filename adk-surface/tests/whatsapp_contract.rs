//! The kernel's chat channel contract (spec 3.3 §10 K1), run against the WhatsApp adapter
//! (spec 3.5 §10 X12) over a fake Graph API: what the suite reads as the wire is every
//! message the adapter sent to a person, and what it types and presses goes through the
//! adapter's own reading of webhook payloads — a press is a reply button the card offers —
//! with the suite's account names as the sender's WhatsApp id and its thread names as the
//! contact's, which replies go to.

#![cfg(feature = "whatsapp")]

mod chat_support;
mod fake_whatsapp;

use std::sync::Arc;

use adk_kernel::channels::{
    ChannelAnswer, ChannelInbound, ChannelNotice, ChannelSink, DecisionCard, StopNotice,
};
use adk_kernel::conformance::{ChannelAdapter, ChannelAdapterHarness};
use adk_surface::channels::WhatsAppAdapter;
use async_trait::async_trait;
use fake_whatsapp::{APP_SECRET, FakeWhatsApp, PHONE_NUMBER_ID, TOKEN, VERIFY_TOKEN};

struct WhatsApp {
    adapter: WhatsAppAdapter,
    api: FakeWhatsApp,
}

#[async_trait]
impl ChannelSink for WhatsApp {
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
impl ChannelAdapter for WhatsApp {
    async fn wire(&self, thread: &str) -> Vec<String> {
        self.api.sent_to(thread).iter().map(|sent| sent.body.to_string()).collect()
    }

    async fn typed(&self, account: &str, thread: &str, text: &str) -> ChannelInbound {
        let payload = self.api.text(account, thread, text);
        self.adapter.inbound(&payload).pop().expect("a text message reaches the gateway")
    }

    async fn pressed(
        &self,
        account: &str,
        thread: &str,
        answer: ChannelAnswer,
    ) -> Option<ChannelInbound> {
        let payload = self.api.press(account, thread, answer.label())?;
        self.adapter.inbound(&payload).pop()
    }

    async fn pairing_code(&self, thread: &str) -> Option<String> {
        self.api.texts(thread).iter().rev().find_map(|text| chat_support::code_in(text))
    }
}

struct Harness;

#[async_trait]
impl ChannelAdapterHarness for Harness {
    async fn adapter(&self) -> Arc<dyn ChannelAdapter> {
        let api = FakeWhatsApp::start().await;
        let adapter = WhatsAppAdapter::builder(PHONE_NUMBER_ID, TOKEN)
            .app_secret(APP_SECRET)
            .verify_token(VERIFY_TOKEN)
            .api_base(api.url())
            .build()
            .unwrap();
        Arc::new(WhatsApp { adapter, api })
    }
}

adk_kernel::channel_contract_tests!(harness = Harness);
