//! The social suite: the driver contract and the outbox rules over it (spec 3.5 §7.10,
//! §7.13, A-I17, §11).

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;

use crate::error::DriverError;
use crate::outbound::{Disclosure, ReplyTemplates, Suppression, SuppressionScope};
use crate::social::{InMemorySocial, SocialDriver, SocialItem, SocialOutbox, social_key};

/// Something the account published, as the server holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    /// The item it answers, for a reply.
    pub in_reply_to: Option<String>,
    /// Its text.
    pub text: String,
    /// Its visibility.
    pub visibility: String,
}

/// The test side of a social service: what other accounts do, and what was published.
#[async_trait]
pub trait SocialServer: Send + Sync {
    /// `item` arrives: a mention, or a direct message when its visibility is `direct`.
    async fn receive(&self, item: &SocialItem);

    /// Everything the account published, in order.
    async fn published(&self) -> Vec<Published>;
}

/// Makes a fresh adapter and its server for [`social_driver_contract`].
#[async_trait]
pub trait SocialHarness: Send + Sync {
    /// A fresh adapter over an account with nothing received, and its server.
    async fn start(&self) -> (Arc<dyn SocialDriver>, Arc<dyn SocialServer>);
}

/// The suite's harness over [`InMemorySocial`].
#[derive(Debug, Default, Clone, Copy)]
pub struct InMemorySocialHarness;

struct InMemoryServer(Arc<InMemorySocial>);

#[async_trait]
impl SocialServer for InMemoryServer {
    async fn receive(&self, item: &SocialItem) {
        self.0.receive(item.clone());
    }

    async fn published(&self) -> Vec<Published> {
        self.0
            .published()
            .into_iter()
            .map(|(in_reply_to, item)| Published {
                in_reply_to,
                text: item.text,
                visibility: item.visibility,
            })
            .collect()
    }
}

#[async_trait]
impl SocialHarness for InMemorySocialHarness {
    async fn start(&self) -> (Arc<dyn SocialDriver>, Arc<dyn SocialServer>) {
        let social = Arc::new(InMemorySocial::new());
        (social.clone(), Arc::new(InMemoryServer(social)))
    }
}

fn fields(hours: &str) -> BTreeMap<String, String> {
    BTreeMap::from([("hours".to_string(), hours.to_string())])
}

/// Runs every social case (spec 3.5 §7.13, §7.10, A-I17).
///
/// | Part | Cases |
/// |---|---|
/// | Driver | Mentions and direct messages read apart, oldest first after a cursor; a reply answers its item, names its author and keeps a direct message direct; a post is public |
/// | Outbox | Replies and posts end with the disclosure; a reply answers only an item the inbox read; a revoked author gets no reply; a text too long with its disclosure is refused |
///
/// # Panics
///
/// When the adapter breaks the contract, naming the case.
pub async fn social_driver_contract(harness: &dyn SocialHarness) {
    let (social, server) = harness.start().await;
    server
        .receive(&SocialItem::new("101", "ana@social.example", "@site open on Sunday?", "public"))
        .await;
    server
        .receive(&SocialItem::new("102", "bob@social.example", "Can I order a cake?", "direct"))
        .await;
    server
        .receive(&SocialItem::new("103", "cy@social.example", "@site love the bread", "public"))
        .await;
    let mentions = social.mentions(None, 20).await.expect("mentions: read");
    let ids: Vec<&str> = mentions.items.iter().map(|item| item.id.as_str()).collect();
    assert_eq!(ids, ["101", "103"], "mentions: public mentions, oldest first");
    assert_eq!(mentions.items[0].author, "ana@social.example", "mentions: the author");
    assert!(
        mentions.items[0].text.contains("open on Sunday?"),
        "mentions: the text without markup"
    );
    let messages = social.messages(None, 20).await.expect("messages: read");
    assert_eq!(messages.items.len(), 1, "messages: the direct message only");
    assert!(messages.items[0].is_direct(), "messages: direct");
    let after =
        social.mentions(mentions.cursor.as_deref(), 20).await.expect("mentions: after the cursor");
    assert!(after.items.is_empty(), "mentions: nothing after the cursor");
    server.receive(&SocialItem::new("104", "dee@social.example", "@site hello", "public")).await;
    let next = social.mentions(mentions.cursor.as_deref(), 20).await.expect("mentions: the next");
    assert_eq!(
        next.items.iter().map(|item| item.id.as_str()).collect::<Vec<_>>(),
        ["104"],
        "mentions: only the new one"
    );

    social.reply(&messages.items[0], "Yes, order by Friday.").await.expect("reply: sent");
    social.post("Fresh sourdough today.").await.expect("post: sent");
    let published = server.published().await;
    assert_eq!(published.len(), 2, "publish: a reply and a post");
    assert_eq!(published[0].in_reply_to.as_deref(), Some("102"), "reply: answers its item");
    assert!(published[0].text.contains("@bob@social.example"), "reply: names its author");
    assert_eq!(published[0].visibility, "direct", "reply: a direct message stays direct");
    assert_eq!(published[1].in_reply_to, None, "post: answers nothing");
    assert_eq!(published[1].visibility, "public", "post: public");

    // The outbox rules.
    let (social, server) = harness.start().await;
    let disclosure = Disclosure::new("Wanjiru's Bakery").unwrap();
    let outbox = SocialOutbox::builder(social, disclosure.clone())
        .templates(ReplyTemplates::new().template("hours", "Yes, {hours} on Sundays."))
        .build();
    server
        .receive(&SocialItem::new("201", "ana@social.example", "@site open on Sunday?", "public"))
        .await;
    server.receive(&SocialItem::new("202", "eve@social.example", "@site hi", "public")).await;
    let error =
        outbox.reply("201", "hours", &fields("8–12")).await.expect_err("outbox: an unread item");
    assert!(
        matches!(error, DriverError::Refused { .. }),
        "outbox: only an item the inbox read: {error:?}"
    );
    let page = outbox.read_mentions(None, 20).await.expect("outbox: reads");
    assert_eq!(page.items.len(), 2, "outbox: two mentions");
    outbox.reply("201", "hours", &fields("8–12")).await.expect("outbox: reply sent");
    outbox.post("Hot cross buns are back.").await.expect("outbox: post sent");
    let published = server.published().await;
    assert!(
        published.iter().all(|p| disclosure.is_present(&p.text)),
        "outbox: every text ends with the disclosure: {published:?}"
    );
    outbox
        .suppressions()
        .suppress(Suppression::new(
            &social_key("eve@social.example"),
            SuppressionScope::All,
            "owner",
        ))
        .await
        .expect("outbox: revocation recorded");
    let error = outbox.reply("202", "hours", &fields("8–12")).await.expect_err("outbox: revoked");
    assert!(
        matches!(error, DriverError::Refused { .. }),
        "outbox: a revoked author gets no reply: {error:?}"
    );
    let long = "bread ".repeat(200);
    let error = outbox.post(&long).await.expect_err("outbox: too long");
    assert!(
        matches!(error, DriverError::Invalid { .. }),
        "outbox: too long with the disclosure: {error:?}"
    );
    assert_eq!(server.published().await.len(), 2, "outbox: nothing more published");
}
