//! A Telegram photo's bytes ride with its message (spec 3.5 §10 X12, §5 S18): the adapter
//! downloads its largest size within the limit with `getFile`, its caption the words; a
//! photo alone has none, and starts no turn.

#![cfg(feature = "channels")]

mod fake_telegram;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_kernel::channels::{
    ChannelBinding, ChannelBindingStore, ChannelGateway, ChannelInbound,
    InMemoryChannelBindingStore, InboundBody, InboundRoute,
};
use adk_kernel::conformance::ContractHost;
use adk_surface::channels::TelegramAdapter;
use async_trait::async_trait;
use fake_telegram::{FakeTelegram, PNG, TOKEN};

#[derive(Default)]
struct Capture(Mutex<Vec<ChannelInbound>>);

#[async_trait]
impl InboundRoute for Capture {
    async fn take(
        &self,
        inbound: &ChannelInbound,
        _: Option<&ChannelBinding>,
    ) -> adk_kernel::Result<bool> {
        self.0.lock().unwrap().push(inbound.clone());
        Ok(false)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_photos_bytes_ride_with_its_message() {
    let api = FakeTelegram::start().await;
    let telegram = TelegramAdapter::builder(TOKEN)
        .api_base(api.url())
        .poll_timeout(Duration::from_secs(1))
        .build()
        .unwrap();
    let host = Arc::new(ContractHost::new());
    let bindings = Arc::new(InMemoryChannelBindingStore::new());
    bindings
        .put_binding(ChannelBinding::new("telegram", "4217", "4217", "ana", chrono::Utc::now()))
        .await
        .unwrap();
    let capture = Arc::new(Capture::default());
    let gateway = ChannelGateway::builder(host.clone(), bindings)
        .sink(Arc::new(telegram.clone()))
        .route(capture.clone())
        .build();
    gateway.start().await.unwrap();
    let _polling = telegram.start(gateway);
    api.deliver(api.photo_update(4217, 4217, Some("Seeded rye on the counter")));
    api.deliver(api.photo_update(4217, 4217, None));
    let mut taken = Vec::new();
    for _ in 0..500 {
        taken = capture.0.lock().unwrap().clone();
        if taken.len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(taken.len(), 2, "{taken:?}");
    assert_eq!(taken[0].body, InboundBody::Text { text: "Seeded rye on the counter".into() });
    assert_eq!(taken[0].attachments[0].bytes, PNG);
    assert_eq!(taken[0].attachments[0].media_type, "image/png");
    assert_eq!(api.calls_of("getFile")[0].body["file_id"], "large-1", "the largest size");
    assert_eq!(taken[1].body, InboundBody::Text { text: String::new() });
    assert_eq!(taken[1].attachments.len(), 1);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(host.turns().len(), 1, "a photo alone starts no turn");
}
