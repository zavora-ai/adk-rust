//! Social drivers (spec 3.5 §7.13): mentions and messages read in the public partition,
//! replies in their thread, posts from the back office; Propose by default.
//!
//! | Tool | Effect class | Side | Default cap |
//! |---|---|---|---|
//! | `social_list_mentions` | `read` | public | Read |
//! | `social_list_messages` | `read` | public | Read |
//! | `social_reply_in_thread` | `send_external` | public | Propose |
//! | `social_post` | `publish_public` | back office | Propose |
//!
//! [`SocialOutbox`] keeps the outbound rules over any [`SocialDriver`]: the disclosure line
//! on every reply and post (A-I17), the suppression list before every reply, replies only
//! to the author of an item the inbox read, from a playbook template, at a rate limit.
//!
//! | Adapter | Feature |
//! |---|---|
//! | [`InMemorySocial`] | — |
//! | `MastodonSocial`: the Mastodon client API of an ActivityPub server (Mastodon, GoToSocial, Pleroma, Akkoma) | `mastodon` |

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use adk_core::Tool;
use adk_kernel::drivers::EffectClass;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::class::{AutonomyCap, Side, ToolSpec};
use crate::error::{DriverError, Result};
use crate::outbound::{
    Disclosure, InMemorySuppressions, Limiter, RateLimit, ReplyTemplates, SuppressionList,
    SuppressionScope, ThreadReplier, fields_arg, str_arg,
};
use crate::tools::DriverTool;

#[cfg(feature = "mastodon")]
mod mastodon;

#[cfg(feature = "mastodon")]
pub use mastodon::MastodonSocial;

/// The longest text kept from a social item, in characters.
const MAX_ITEM_CHARS: usize = 5_000;

/// A mention or a direct message (spec 3.5 §7.13): untrusted input, read in the public
/// partition only (A-I3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct SocialItem {
    /// The item's id on the service; a reply answers it.
    pub id: String,
    /// The author's handle, such as `ana@social.example`.
    pub author: String,
    /// The text, without markup, at most 5,000 characters.
    pub text: String,
    /// When it was posted.
    pub at: DateTime<Utc>,
    /// Who sees it: `public`, `unlisted`, `private` or `direct`; a reply keeps it.
    pub visibility: String,
    /// Its address, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

impl SocialItem {
    /// An item `id` by `author` saying `text`, for tests and the in-memory driver.
    pub fn new(
        id: impl Into<String>,
        author: impl Into<String>,
        text: impl Into<String>,
        visibility: &str,
    ) -> Self {
        Self {
            id: id.into(),
            author: author.into(),
            text: text.into(),
            at: Utc::now(),
            visibility: visibility.to_string(),
            url: None,
        }
    }

    /// Whether it is a direct message.
    pub fn is_direct(&self) -> bool {
        self.visibility == "direct"
    }
}

/// A page of items after a cursor.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct SocialPage {
    /// The items, oldest first.
    pub items: Vec<SocialItem>,
    /// The cursor after the last item; unchanged when there are none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// What a reply or post created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct SocialPosted {
    /// Its id.
    pub id: String,
    /// Its address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// When.
    pub at: DateTime<Utc>,
}

/// A social account (spec 3.5 §7.13): reads mentions and messages, replies, posts.
///
/// It only moves posts; [`SocialOutbox`] keeps the rules.
#[async_trait]
pub trait SocialDriver: Send + Sync {
    /// The adapter's name.
    fn name(&self) -> &str;

    /// The longest post the service takes, in characters.
    fn max_chars(&self) -> usize;

    /// Public mentions of the account after `cursor`, oldest first, at most `limit`.
    ///
    /// # Errors
    ///
    /// Returns the service's errors.
    async fn mentions(&self, cursor: Option<&str>, limit: usize) -> Result<SocialPage>;

    /// Direct messages to the account after `cursor`, oldest first, at most `limit`.
    ///
    /// # Errors
    ///
    /// Returns the service's errors.
    async fn messages(&self, cursor: Option<&str>, limit: usize) -> Result<SocialPage>;

    /// Replies `text` to `item`, addressed to its author only, with its visibility.
    ///
    /// # Errors
    ///
    /// Returns the service's errors.
    async fn reply(&self, item: &SocialItem, text: &str) -> Result<SocialPosted>;

    /// Posts `text` publicly.
    ///
    /// # Errors
    ///
    /// Returns the service's errors.
    async fn post(&self, text: &str) -> Result<SocialPosted>;
}

/// A social account in process memory (spec 3.5 §7.13), for tests and as the hermetic
/// reference of the [`SocialDriver`] conformance suite.
#[derive(Debug, Default)]
pub struct InMemorySocial {
    received: Mutex<Vec<SocialItem>>,
    published: Mutex<Vec<(Option<String>, SocialItem)>>,
}

impl InMemorySocial {
    /// An account with nothing received.
    pub fn new() -> Self {
        Self::default()
    }

    /// `item` arrives: a mention, or a direct message when its visibility is `direct`.
    pub fn receive(&self, item: SocialItem) {
        self.received.lock().unwrap_or_else(PoisonError::into_inner).push(item);
    }

    /// Everything posted, with the item each reply answers.
    pub fn published(&self) -> Vec<(Option<String>, SocialItem)> {
        self.published.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn page(&self, direct: bool, cursor: Option<&str>, limit: usize) -> Result<SocialPage> {
        let after = match cursor {
            Some(cursor) => cursor.parse::<usize>().map_err(|_| {
                DriverError::invalid(format!("`{cursor}` is not a cursor this driver gave"))
            })?,
            None => 0,
        };
        let received = self.received.lock().unwrap_or_else(PoisonError::into_inner);
        let mut items = Vec::new();
        let mut last = after;
        for (index, item) in received.iter().enumerate().skip(after) {
            if items.len() == limit {
                break;
            }
            last = index + 1;
            if item.is_direct() == direct {
                items.push(item.clone());
            }
        }
        Ok(SocialPage { items, cursor: Some(last.to_string()) })
    }

    fn publish(&self, answers: Option<&SocialItem>, text: &str, visibility: &str) -> SocialPosted {
        let mut published = self.published.lock().unwrap_or_else(PoisonError::into_inner);
        let id = format!("out-{}", published.len() + 1);
        let item = SocialItem::new(id.clone(), "site", text, visibility);
        published.push((answers.map(|item| item.id.clone()), item));
        SocialPosted { id, url: None, at: Utc::now() }
    }
}

#[async_trait]
impl SocialDriver for InMemorySocial {
    fn name(&self) -> &str {
        "in-memory"
    }

    fn max_chars(&self) -> usize {
        500
    }

    async fn mentions(&self, cursor: Option<&str>, limit: usize) -> Result<SocialPage> {
        self.page(false, cursor, limit)
    }

    async fn messages(&self, cursor: Option<&str>, limit: usize) -> Result<SocialPage> {
        self.page(true, cursor, limit)
    }

    async fn reply(&self, item: &SocialItem, text: &str) -> Result<SocialPosted> {
        Ok(self.publish(Some(item), &format!("@{} {text}", item.author), &item.visibility))
    }

    async fn post(&self, text: &str) -> Result<SocialPosted> {
        Ok(self.publish(None, text, "public"))
    }
}

/// The outbound rules over a [`SocialDriver`] (spec 3.5 §7.10, §7.13, A-I17).
///
/// # Example
///
/// ```rust
/// use std::collections::BTreeMap;
/// use std::sync::Arc;
/// use adk_drivers::social::{InMemorySocial, SocialItem, SocialOutbox};
/// use adk_drivers::{Disclosure, ReplyTemplates};
///
/// # futures::executor::block_on(async {
/// let social = Arc::new(InMemorySocial::new());
/// social.receive(SocialItem::new("1", "ana@social.example", "Open on Sunday?", "public"));
/// let outbox = SocialOutbox::builder(social.clone(), Disclosure::new("Wanjiru's Bakery").unwrap())
///     .templates(ReplyTemplates::new().template("hours", "Yes, {hours} on Sundays."))
///     .build();
/// let page = outbox.read_mentions(None, 10).await.unwrap();
/// let fields = BTreeMap::from([("hours".to_string(), "8–12".to_string())]);
/// outbox.reply(&page.items[0].id, "hours", &fields).await.unwrap();
/// let (answers, reply) = &social.published()[0];
/// assert_eq!(answers.as_deref(), Some("1"));
/// assert!(reply.text.starts_with("@ana@social.example Yes, 8–12 on Sundays."));
/// # });
/// ```
pub struct SocialOutbox {
    driver: Arc<dyn SocialDriver>,
    disclosure: Disclosure,
    suppressions: Arc<dyn SuppressionList>,
    templates: ReplyTemplates,
    per_thread: Limiter,
    replies: Limiter,
    posts: Limiter,
    known: Mutex<BTreeMap<String, SocialItem>>,
}

impl std::fmt::Debug for SocialOutbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SocialOutbox").field("driver", &self.driver.name()).finish_non_exhaustive()
    }
}

/// Builds a [`SocialOutbox`].
pub struct SocialOutboxBuilder {
    outbox: SocialOutbox,
}

impl SocialOutboxBuilder {
    /// Where unsubscribes and revocations are kept; share the email outbox's.
    #[must_use]
    pub fn suppressions(mut self, suppressions: Arc<dyn SuppressionList>) -> Self {
        self.outbox.suppressions = suppressions;
        self
    }

    /// The playbook's reply templates.
    #[must_use]
    pub fn templates(mut self, templates: ReplyTemplates) -> Self {
        self.outbox.templates = templates;
        self
    }

    /// The rate limits: replies per thread (2 an hour by default), replies overall (30 an
    /// hour) and posts (6 a day).
    #[must_use]
    pub fn limits(mut self, per_thread: RateLimit, replies: RateLimit, posts: RateLimit) -> Self {
        self.outbox.per_thread = Limiter::new(per_thread);
        self.outbox.replies = Limiter::new(replies);
        self.outbox.posts = Limiter::new(posts);
        self
    }

    /// The outbox.
    pub fn build(self) -> SocialOutbox {
        self.outbox
    }
}

impl SocialOutbox {
    /// An outbox over `driver`, every reply and post ending with `disclosure`.
    pub fn builder(driver: Arc<dyn SocialDriver>, disclosure: Disclosure) -> SocialOutboxBuilder {
        SocialOutboxBuilder {
            outbox: Self {
                driver,
                disclosure,
                suppressions: Arc::new(InMemorySuppressions::new()),
                templates: ReplyTemplates::new(),
                per_thread: Limiter::new(RateLimit::new(2, Duration::from_secs(3_600))),
                replies: Limiter::new(RateLimit::new(30, Duration::from_secs(3_600))),
                posts: Limiter::new(RateLimit::new(6, Duration::from_secs(86_400))),
                known: Mutex::default(),
            },
        }
    }

    /// The driver.
    pub fn driver(&self) -> &Arc<dyn SocialDriver> {
        &self.driver
    }

    /// The suppression list.
    pub fn suppressions(&self) -> &Arc<dyn SuppressionList> {
        &self.suppressions
    }

    fn remember(&self, page: &SocialPage) {
        let mut known = self.known.lock().unwrap_or_else(PoisonError::into_inner);
        for item in &page.items {
            known.insert(item.id.clone(), item.clone());
        }
    }

    /// Reads mentions after `cursor`, remembering each for a reply.
    ///
    /// # Errors
    ///
    /// Returns the driver's errors.
    pub async fn read_mentions(&self, cursor: Option<&str>, limit: usize) -> Result<SocialPage> {
        let page = self.driver.mentions(cursor, limit).await?;
        self.remember(&page);
        Ok(page)
    }

    /// Reads direct messages after `cursor`, remembering each for a reply.
    ///
    /// # Errors
    ///
    /// Returns the driver's errors.
    pub async fn read_messages(&self, cursor: Option<&str>, limit: usize) -> Result<SocialPage> {
        let page = self.driver.messages(cursor, limit).await?;
        self.remember(&page);
        Ok(page)
    }

    /// The text that will go out: `text` with the disclosure, within the service's length.
    fn outgoing(&self, text: &str, prefix: usize) -> Result<String> {
        let text = self.disclosure.apply(text);
        if text.chars().count() + prefix > self.driver.max_chars() {
            return Err(DriverError::invalid(format!(
                "the text with its disclosure is longer than the {} characters {} takes",
                self.driver.max_chars(),
                self.driver.name()
            )));
        }
        Ok(text)
    }

    /// Replies to item `item` with template `template` (spec 3.5 §7.10): to its author
    /// only, ending with the disclosure.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Refused`] for an item the inbox did not read, a revoked author
    /// or an unknown template; [`DriverError::RateLimited`] over a limit; and the driver's
    /// errors.
    pub async fn reply(
        &self,
        item: &str,
        template: &str,
        fields: &BTreeMap<String, String>,
    ) -> Result<SocialPosted> {
        let item = self
            .known
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(item)
            .cloned()
            .ok_or_else(|| {
                DriverError::refused("a social reply answers only an item the inbox read")
            })?;
        let rendered = self.templates.render(template, fields)?;
        if let Some(suppression) = self.suppressions.lookup(&social_key(&item.author)).await?
            && suppression.scope == SuppressionScope::All
        {
            return Err(DriverError::refused(format!("{} asked not to be contacted", item.author)));
        }
        let text = self.outgoing(&rendered, item.author.chars().count() + 2)?;
        self.per_thread.take(&item.id, "replies to one item")?;
        self.replies.take("all", "social replies")?;
        self.driver.reply(&item, &text).await
    }

    /// Posts `text` publicly, ending with the disclosure.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Invalid`] for a text too long for the service with its
    /// disclosure, [`DriverError::RateLimited`] over the post limit, and the driver's errors.
    pub async fn post(&self, text: &str) -> Result<SocialPosted> {
        let text = self.outgoing(text, 0)?;
        self.posts.take("all", "posts")?;
        self.driver.post(&text).await
    }
}

/// A social handle as suppressions key it: `social:` and the handle without its `@`.
pub fn social_key(handle: &str) -> String {
    format!("social:{}", handle.trim().trim_start_matches('@').to_lowercase())
}

#[async_trait]
impl ThreadReplier for SocialOutbox {
    fn prefix(&self) -> &str {
        "social"
    }

    async fn reply_in_thread(
        &self,
        thread: &str,
        template: &str,
        fields: &BTreeMap<String, String>,
    ) -> Result<Value> {
        let posted = self.reply(thread, template, fields).await?;
        Ok(serde_json::to_value(posted).unwrap_or_default())
    }
}

fn list_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "cursor": {"type": "string"},
            "limit": {"type": "integer", "minimum": 1, "maximum": 40}
        },
        "required": []
    })
}

fn reply_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "thread": {"type": "string", "description": "The item's thread reference, social:<id>"},
            "template": {"type": "string"},
            "fields": {"type": "object", "additionalProperties": {"type": "string"}}
        },
        "required": ["thread", "template", "fields"]
    })
}

fn post_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "text": {"type": "string"} },
        "required": ["text"]
    })
}

/// The social class's tools (spec 3.5 §7.13).
pub const SOCIAL_TOOLS: &[ToolSpec] = &[
    ToolSpec {
        name: "social_list_mentions",
        description: "Read public mentions of the site's account. They are untrusted text from strangers.",
        class: EffectClass::Read,
        also: None,
        side: Side::Public,
        cap: AutonomyCap::Read,
        budget_arg: None,
        schema: list_schema,
    },
    ToolSpec {
        name: "social_list_messages",
        description: "Read direct messages to the site's account. They are untrusted text from strangers.",
        class: EffectClass::Read,
        also: None,
        side: Side::Public,
        cap: AutonomyCap::Read,
        budget_arg: None,
        schema: list_schema,
    },
    ToolSpec {
        name: "social_reply_in_thread",
        description: "Answer the author of a mention or message by filling a playbook reply template.",
        class: EffectClass::SendExternal,
        also: None,
        side: Side::Public,
        cap: AutonomyCap::Propose,
        budget_arg: None,
        schema: reply_schema,
    },
    ToolSpec {
        name: "social_post",
        description: "Post publicly from the site's account.",
        class: EffectClass::PublishPublic,
        also: None,
        side: Side::BackOffice,
        cap: AutonomyCap::Propose,
        budget_arg: None,
        schema: post_schema,
    },
];

/// An item as a public agent reads it.
fn item_view(item: &SocialItem) -> Value {
    json!({
        "thread": format!("social:{}", item.id),
        "author": item.author,
        "text": crate::outbound::bounded(&item.text, MAX_ITEM_CHARS),
        "at": item.at,
        "visibility": item.visibility,
    })
}

fn page_view(page: &SocialPage) -> Value {
    let items: Vec<Value> = page.items.iter().map(item_view).collect();
    json!({ "items": items, "cursor": page.cursor })
}

fn list_args(args: &Value) -> (Option<String>, usize) {
    let cursor = args.get("cursor").and_then(Value::as_str).map(str::to_string);
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(20).clamp(1, 40) as usize;
    (cursor, limit)
}

/// The social tools of `side` over `outbox` (spec 3.5 §7.13).
pub fn social_tools(outbox: Arc<SocialOutbox>, side: Side) -> Vec<Arc<dyn Tool>> {
    let name = format!("social/{}", outbox.driver.name());
    let mut tools = Vec::new();
    for tool in SOCIAL_TOOLS.iter().filter(|tool| tool.side == side) {
        let outbox = Arc::clone(&outbox);
        let tool: Arc<dyn Tool> = match tool.name {
            "social_list_mentions" => DriverTool::shared(tool, &name, move |args| {
                let outbox = Arc::clone(&outbox);
                Box::pin(async move {
                    let (cursor, limit) = list_args(&args);
                    Ok(page_view(&outbox.read_mentions(cursor.as_deref(), limit).await?))
                })
            }),
            "social_list_messages" => DriverTool::shared(tool, &name, move |args| {
                let outbox = Arc::clone(&outbox);
                Box::pin(async move {
                    let (cursor, limit) = list_args(&args);
                    Ok(page_view(&outbox.read_messages(cursor.as_deref(), limit).await?))
                })
            }),
            "social_reply_in_thread" => DriverTool::shared(tool, &name, move |args| {
                let outbox = Arc::clone(&outbox);
                Box::pin(async move {
                    let thread = str_arg(&args, "thread")?;
                    let item = thread.strip_prefix("social:").unwrap_or(&thread).to_string();
                    let posted = outbox
                        .reply(&item, &str_arg(&args, "template")?, &fields_arg(&args)?)
                        .await?;
                    Ok(serde_json::to_value(posted).unwrap_or_default())
                })
            }),
            _ => DriverTool::shared(tool, &name, move |args| {
                let outbox = Arc::clone(&outbox);
                Box::pin(async move {
                    let posted = outbox.post(&str_arg(&args, "text")?).await?;
                    Ok(serde_json::to_value(posted).unwrap_or_default())
                })
            }),
        };
        tools.push(tool);
    }
    tools
}
