//! The kernel's chat channel contract (spec 3.3 §10 K1), run against the Matrix adapter
//! (spec 3.5 §10 X12) over a fake homeserver: what the suite reads as the wire is every event
//! the adapter sent to a room, and what it types and presses goes through the adapter's own
//! reading of room events — a press is a reaction with the key the card offers — with the
//! suite's account and thread names as Matrix user and room ids.

#![cfg(feature = "matrix")]

mod chat_support;
mod fake_matrix;

use std::sync::Arc;

use adk_kernel::channels::{
    ChannelAnswer, ChannelInbound, ChannelNotice, ChannelSink, DecisionCard, StopNotice,
};
use adk_kernel::conformance::{ChannelAdapter, ChannelAdapterHarness};
use adk_surface::channels::MatrixAdapter;
use async_trait::async_trait;
use fake_matrix::{BOT, FakeMatrix, TOKEN};

struct Matrix {
    adapter: MatrixAdapter,
    homeserver: FakeMatrix,
}

#[async_trait]
impl ChannelSink for Matrix {
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
impl ChannelAdapter for Matrix {
    async fn wire(&self, thread: &str) -> Vec<String> {
        self.homeserver.sent_to(thread).iter().map(|sent| sent.content.to_string()).collect()
    }

    async fn typed(&self, account: &str, thread: &str, text: &str) -> ChannelInbound {
        let event = self.homeserver.text_event(account, text);
        self.adapter.inbound(thread, &event).await.expect("a text message reaches the gateway")
    }

    async fn pressed(
        &self,
        account: &str,
        thread: &str,
        answer: ChannelAnswer,
    ) -> Option<ChannelInbound> {
        let (card, key) = self.homeserver.key_for(thread, answer.label())?;
        let event = self.homeserver.reaction_event(account, &card, &key);
        self.adapter.inbound(thread, &event).await
    }

    async fn pairing_code(&self, thread: &str) -> Option<String> {
        self.homeserver.bodies(thread).iter().rev().find_map(|body| chat_support::code_in(body))
    }
}

struct Harness;

#[async_trait]
impl ChannelAdapterHarness for Harness {
    async fn adapter(&self) -> Arc<dyn ChannelAdapter> {
        let homeserver = FakeMatrix::start().await;
        let adapter = MatrixAdapter::builder(homeserver.url(), TOKEN).user_id(BOT).build().unwrap();
        Arc::new(Matrix { adapter, homeserver })
    }
}

adk_kernel::channel_contract_tests!(harness = Harness);
