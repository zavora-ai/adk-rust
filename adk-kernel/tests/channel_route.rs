//! The gateway's inbound route (spec 3.5 §10 X12, §7.19, A-I24): a route takes a stranger's
//! message before the gateway, so it gets no pairing code and starts no turn, while a message
//! the route leaves — such as an explicit `/pair` — is the gateway's as before; a paired
//! account's message with no text starts no turn, and a message's attachments ride with it.

#![cfg(all(feature = "channels", feature = "conformance"))]

use std::sync::{Arc, Mutex};

use adk_kernel::channels::{
    ChannelAttachment, ChannelBinding, ChannelBindingStore, ChannelGateway, ChannelInbound,
    InMemoryChannel, InMemoryChannelBindingStore, InboundBody, InboundOutcome, InboundRoute,
};
use adk_kernel::conformance::ContractHost;
use async_trait::async_trait;
use chrono::Utc;

/// Takes every stranger's message except `/pair`, noting what it took.
#[derive(Default)]
struct Strangers {
    taken: Mutex<Vec<ChannelInbound>>,
}

#[async_trait]
impl InboundRoute for Strangers {
    async fn take(
        &self,
        inbound: &ChannelInbound,
        binding: Option<&ChannelBinding>,
    ) -> adk_kernel::Result<bool> {
        let pair = matches!(&inbound.body, InboundBody::Text { text } if text.trim() == "/pair");
        if binding.is_some() || pair {
            return Ok(false);
        }
        self.taken.lock().unwrap().push(inbound.clone());
        Ok(true)
    }
}

#[tokio::test]
async fn a_route_takes_strangers_and_leaves_pair_and_paired_accounts_to_the_gateway() {
    let host = Arc::new(ContractHost::new());
    let chat = Arc::new(InMemoryChannel::new("chat"));
    let bindings = Arc::new(InMemoryChannelBindingStore::new());
    bindings
        .put_binding(ChannelBinding::new("chat", "ana", "t-ana", "ana", Utc::now()))
        .await
        .unwrap();
    let route = Arc::new(Strangers::default());
    let gateway = ChannelGateway::builder(host.clone(), bindings.clone())
        .sink(chat.clone())
        .route(route.clone())
        .build();
    gateway.start().await.unwrap();

    let photo = ChannelAttachment::new("image/png", "a.png", vec![1, 2, 3]);
    let hello = ChannelInbound::text("chat", "eve", "t-eve", "m-1", "do you bake rye?")
        .with_attachments([photo.clone()]);
    assert_eq!(gateway.receive(hello).await.unwrap(), InboundOutcome::Routed);
    assert!(chat.wire("t-eve").is_empty(), "no pairing code");
    assert!(host.turns().is_empty(), "no turn");
    assert_eq!(route.taken.lock().unwrap()[0].attachments, std::slice::from_ref(&photo));

    let pair = ChannelInbound::text("chat", "eve", "t-eve", "m-2", "/pair");
    assert_eq!(gateway.receive(pair).await.unwrap(), InboundOutcome::PairingRequested);
    assert!(chat.pairing_code("t-eve").is_some(), "an explicit /pair gets a code");

    let paired = ChannelInbound::text("chat", "ana", "t-ana", "m-3", "add the seeded rye");
    assert!(matches!(gateway.receive(paired).await.unwrap(), InboundOutcome::TurnStarted { .. }));
    let photo_alone =
        ChannelInbound::text("chat", "ana", "t-ana", "m-4", " ").with_attachments([photo]);
    assert_eq!(gateway.receive(photo_alone).await.unwrap(), InboundOutcome::Empty);
    assert_eq!(host.turns().len(), 1, "a photo alone starts no turn");
    // Attachments never ride in a turn's input.
    assert!(host.turns()[0].1.input.get("attachments").is_none());
}

#[test]
fn attachments_never_serialize_their_bytes() {
    let photo = ChannelAttachment::new("image/png", "a.png", vec![9; 16]).with_caption("Rye");
    let inbound = ChannelInbound::text("chat", "ana", "t", "m", "hi").with_attachments([photo]);
    let json = serde_json::to_value(&inbound).unwrap();
    assert_eq!(
        json["attachments"][0],
        serde_json::json!({ "mediaType": "image/png", "name": "a.png", "caption": "Rye" })
    );
    let plain = serde_json::to_value(ChannelInbound::text("chat", "ana", "t", "m", "hi")).unwrap();
    assert!(plain.get("attachments").is_none(), "existing payloads are unchanged");
}
