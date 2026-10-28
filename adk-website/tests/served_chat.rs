//! Chat on a served site (spec 3.5 §10 X12, §7.19, §7.8.2 "Entry points", §5 S18, A-I24),
//! over the kernel's in-memory chat as the platform: a stranger's message is the Inbox's,
//! with no pairing code and no turn; `/pair` gets a code; the paired owner's message with
//! three photos is a turn of the Steward that drafts the product, the photos join that draft
//! through the media pipeline, and the owner confirms their alt text from the thread.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use adk_kernel::channels::{
    ChannelAttachment, ChannelBinding, ChannelBindingStore, ChannelGateway, ChannelInbound,
    ChannelSink, InMemoryChannel, InboundOutcome,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_site::pipeline::EntryPoint;
use adk_website::deploy::chat::ChatAdapters;
use adk_website::deploy::{ServeOptions, serve};
use adk_website::onboarding::scaffold::{ScaffoldOptions, scaffold};
use adk_website::themes::ThemeId;
use awp_types::SiteKind;
use base64::Engine as _;
use serde_json::json;

/// A 1×1 PNG.
const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";

/// The kernel's in-memory chat as the site's one platform.
struct InMemory(Arc<InMemoryChannel>);

#[async_trait::async_trait]
impl ChatAdapters for InMemory {
    fn sinks(&self) -> Vec<Arc<dyn ChannelSink>> {
        vec![self.0.clone()]
    }

    async fn start(&self, _gateway: ChannelGateway) -> adk_core::Result<axum::Router> {
        Ok(axum::Router::new())
    }
}

async fn eventually<T, F, Fut>(what: &str, mut probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    for _ in 0..1000 {
        if let Some(value) = probe().await {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn strangers_reach_the_inbox_and_the_owners_photos_reach_the_draft() {
    let dir = tempfile::tempdir().unwrap();
    let scaffolded = ScaffoldOptions {
        name: "Juniper Bakery".into(),
        domain: "juniper.example".into(),
        kind: SiteKind::Ecommerce,
        languages: vec!["en".parse().unwrap()],
        theme: ThemeId::Paper,
        currency: "KES".into(),
    };
    scaffold(dir.path(), &scaffolded).await.unwrap();
    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        vec![
            // The Inbox answers the stranger in their thread.
            ScriptedResponse::function_call(
                "reply_in_thread",
                json!({ "text": "Yes, we bake rye every morning." }),
            ),
            ScriptedResponse::text("Answered."),
            // The Steward drafts the owner's product.
            ScriptedResponse::function_call(
                "content_draft",
                json!({ "kind": "product", "slug": "seeded-rye", "language": "en",
                        "fields": { "title": "Seeded rye", "price": 520, "currency": "KES", "goal": "sales" },
                        "body": "Rye with seeds.", "job": "onboarding" }),
            ),
            ScriptedResponse::text("Drafted the seeded rye."),
        ],
    ));
    let chat = Arc::new(InMemoryChannel::new("chat"));
    let mut options = ServeOptions::new(dir.path(), "owner");
    let loopback: SocketAddr = "127.0.0.1:0".parse().unwrap();
    (options.edge, options.kernel) = (loopback, loopback);
    options.site.idle = Some(Duration::from_millis(100));
    options.site.model = Some(model.clone());
    options.site.chat = Some(Arc::new(InMemory(chat.clone())));
    let served = serve(options).await.unwrap();
    let gateway = served.chat().expect("the site runs a chat gateway").clone();
    served
        .stores
        .channel_bindings
        .put_binding(ChannelBinding::new("chat", "amina", "t-amina", "owner", chrono::Utc::now()))
        .await
        .unwrap();
    let photo = |n: usize| {
        let bytes = base64::engine::general_purpose::STANDARD.decode(PNG).unwrap();
        ChannelAttachment::new("image/png", format!("rye-{n}.png"), bytes)
    };

    // A stranger's message, photos and all, is the Inbox's: no code, no turn, nothing kept.
    let stranger = ChannelInbound::text("chat", "eve", "t-eve", "m-1", "Do you bake rye?")
        .with_attachments([photo(0)]);
    assert_eq!(gateway.receive(stranger).await.unwrap(), InboundOutcome::Routed);
    assert_eq!(chat.messages("t-eve"), ["Yes, we bake rye every morning."]);
    assert!(chat.pairing_code("t-eve").is_none(), "no pairing code");
    let processes = served.kernel().processes().ps(Default::default()).await.unwrap();
    assert!(processes.is_empty(), "no turn: {processes:?}");

    // `/pair` is the one way a stranger gets a code.
    let pair = ChannelInbound::text("chat", "eve", "t-eve", "m-2", "/pair");
    assert_eq!(gateway.receive(pair).await.unwrap(), InboundOutcome::PairingRequested);
    assert!(chat.pairing_code("t-eve").is_some());

    // The owner's message with three photos is the Steward's turn; the photos join its draft.
    let message =
        ChannelInbound::text("chat", "amina", "t-amina", "m-3", "New loaf: seeded rye, 520 KES")
            .with_attachments((0..3).map(photo));
    assert!(matches!(gateway.receive(message).await.unwrap(), InboundOutcome::TurnStarted { .. }));
    let added = eventually("the photos in the draft", || async {
        chat.messages("t-amina").into_iter().find(|text| text.starts_with("Added 3 photos"))
    })
    .await;
    assert!(added.contains("Seeded rye"), "{added}");
    let entry = EntryPoint::PairedDevice { device: ChannelBinding::id_for("chat", "amina") };
    let (_, items) = served.sites[0].runtime.newest_draft_from(&entry).await.expect("a draft");
    assert_eq!(items[0].id.0, "products/seeded-rye/en", "the draft entered through the device");
    eventually("the Steward's reply", || async {
        chat.messages("t-amina").into_iter().find(|text| text.contains("Drafted the seeded rye"))
    })
    .await;

    // The owner confirms the alt text from the thread; no turn starts for it.
    let yes = ChannelInbound::text("chat", "amina", "t-amina", "m-4", "yes");
    assert_eq!(gateway.receive(yes).await.unwrap(), InboundOutcome::Routed);
    assert!(chat.messages("t-amina").iter().any(|text| text.contains("confirmed")));
    assert_eq!(model.requests().len(), 4, "the Inbox's turn and the Steward's, nothing else");
    served.shutdown().await.unwrap();
}
