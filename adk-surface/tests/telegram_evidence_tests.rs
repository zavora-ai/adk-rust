//! A Telegram card for a call with an image (spec 3.7 §8.2, board choice A17): with the
//! artifact service, the exact image that would leave goes first and Allow once sits beside
//! it; without it, the card names the digest, links to the home and offers Deny only.

#![cfg(feature = "channels")]

mod fake_telegram;

use std::sync::Arc;

use adk_artifact::{ArtifactService, InMemoryArtifactService, SaveRequest};
use adk_core::{
    ApprovalPolicy, ApprovalRequest, ApprovalScope, ApprovalService, ApproverSelector,
    ArtifactLocator, Evidence, ImageEvidence, InMemoryApprovalStore, Part, StoreApprovalService,
    ToolConfirmationRequest,
};
use adk_kernel::channels::{ChannelSink, DecisionCard};
use adk_surface::channels::TelegramAdapter;
use fake_telegram::{FakeTelegram, PNG, TOKEN};
use serde_json::json;
use sha2::Digest as _;

async fn held(
    artifacts: &InMemoryArtifactService,
    service: &Arc<dyn ApprovalService>,
) -> ApprovalRequest {
    let saved = artifacts
        .save(SaveRequest {
            app_name: "desk".into(),
            user_id: "ana".into(),
            session_id: "s1".into(),
            file_name: "vision-egress.png".into(),
            part: Part::InlineData {
                mime_type: "image/png".into(),
                data: PNG.to_vec(),
                uri: None,
                annotations: None,
            },
            version: None,
        })
        .await
        .unwrap();
    let digest = format!("sha256:{:x}", sha2::Sha256::digest(PNG));
    let image = ImageEvidence::new(
        digest,
        ArtifactLocator::new("desk", "ana", "s1", "vision-egress.png", saved.version),
        "image/png",
        1,
        1,
    )
    .with_caption("to api.anthropic.com as document_crop · 1 face · the whole person is covered");
    let call = ToolConfirmationRequest::new("vision_describe", json!({ "image": "id-card.png" }))
        .with_evidence([Evidence::from(image)]);
    let request = ApprovalRequest::new(
        "dec-7c41e0",
        ApprovalScope::new("desk", "ana", "s1"),
        &call,
        ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
    );
    service.submit(request).await.unwrap()
}

fn service() -> Arc<dyn ApprovalService> {
    Arc::new(StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())))
}

#[tokio::test]
async fn the_card_attaches_the_exact_image_and_offers_allow_once_beside_it() {
    let telegram = FakeTelegram::start().await;
    let artifacts = Arc::new(InMemoryArtifactService::new());
    let approvals = service();
    let request = held(&artifacts, &approvals).await;
    let adapter = TelegramAdapter::builder(TOKEN)
        .api_base(telegram.url())
        .approvals(approvals)
        .artifacts(artifacts.clone() as Arc<dyn ArtifactService>)
        .build()
        .unwrap();

    adapter
        .send_decision(
            "42",
            &DecisionCard::from_request(&request, Some("http://127.0.0.1:8080/ui/")),
        )
        .await
        .unwrap();

    let calls = telegram.calls();
    assert_eq!(calls[0].method, "sendPhoto", "the image goes first");
    assert_eq!(calls[0].body["png"], true);
    let caption = calls[0].body["caption"].as_str().unwrap();
    assert!(
        caption.starts_with("The exact image that would leave: to api.anthropic.com"),
        "{caption}"
    );
    assert!(caption.contains("sha256:"), "{caption}");
    let card = &calls[1];
    assert_eq!(card.method, "sendMessage");
    let buttons = card.body["reply_markup"].to_string();
    assert!(buttons.contains("Allow once") && buttons.contains("Deny"), "{buttons}");
}

#[tokio::test]
async fn without_the_image_the_card_names_the_digest_and_offers_deny_only() {
    let telegram = FakeTelegram::start().await;
    let artifacts = InMemoryArtifactService::new();
    let approvals = service();
    let request = held(&artifacts, &approvals).await;
    let adapter = TelegramAdapter::builder(TOKEN)
        .api_base(telegram.url())
        .approvals(approvals)
        .build()
        .unwrap();

    adapter
        .send_decision(
            "42",
            &DecisionCard::from_request(&request, Some("http://127.0.0.1:8080/ui/")),
        )
        .await
        .unwrap();

    let calls = telegram.calls();
    assert!(calls.iter().all(|call| call.method != "sendPhoto"));
    let card = calls.iter().find(|call| call.method == "sendMessage").unwrap();
    let text = card.body["text"].as_str().unwrap();
    assert!(text.contains("can't show the image"), "{text}");
    assert!(text.contains("sha256:"), "{text}");
    let buttons = card.body["reply_markup"].to_string();
    assert!(!buttons.contains("Allow once") && buttons.contains("Deny"), "{buttons}");
}
