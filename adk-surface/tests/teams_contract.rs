//! The kernel's chat channel contract (spec 3.3 §10 K1), run against the Microsoft Teams
//! adapter over a fake Bot Connector: what the suite reads as the wire is every activity the
//! adapter posted to a conversation, and what a person types and presses goes through the
//! adapter's own reading of Bot Framework activities — a press is the `adaptiveCard/action`
//! invoke of an `Action.Execute` the card offers — with the suite's account names as the
//! sender's Teams id and its thread names as the one-on-one conversation's id.

#![cfg(feature = "teams")]

mod chat_support;
mod fake_teams;

use std::sync::Arc;

use adk_kernel::channels::{
    ChannelAnswer, ChannelInbound, ChannelNotice, ChannelSink, DecisionCard, StopNotice,
};
use adk_kernel::conformance::{ChannelAdapter, ChannelAdapterHarness};
use adk_surface::channels::TeamsAdapter;
use async_trait::async_trait;
use fake_teams::FakeTeams;

struct Teams {
    adapter: TeamsAdapter,
    api: FakeTeams,
}

#[async_trait]
impl ChannelSink for Teams {
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
impl ChannelAdapter for Teams {
    async fn wire(&self, thread: &str) -> Vec<String> {
        self.api.sent_to(thread).iter().map(|sent| sent.body.to_string()).collect()
    }

    async fn typed(&self, account: &str, thread: &str, text: &str) -> ChannelInbound {
        let activity = self.api.message(account, thread, text);
        self.adapter.inbound(&activity).expect("a message reaches the gateway")
    }

    async fn pressed(
        &self,
        account: &str,
        thread: &str,
        answer: ChannelAnswer,
    ) -> Option<ChannelInbound> {
        let activity = self.api.press(account, thread, answer.label())?;
        self.adapter.inbound(&activity)
    }

    async fn pairing_code(&self, thread: &str) -> Option<String> {
        self.api.texts(thread).iter().rev().find_map(|text| chat_support::code_in(text))
    }
}

struct Harness;

#[async_trait]
impl ChannelAdapterHarness for Harness {
    async fn adapter(&self) -> Arc<dyn ChannelAdapter> {
        let api = FakeTeams::start().await;
        let adapter = api.builder().build().unwrap();
        Arc::new(Teams { adapter, api })
    }
}

adk_kernel::channel_contract_tests!(harness = Harness);
