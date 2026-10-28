//! The Inbox: the site's messages from people who are not paired to it (spec 3.5 §7.19,
//! §7.10, §10 X12, A-I2, A-I3, A-I24).
//!
//! | What wakes it | What reaches the Inbox |
//! |---|---|
//! | The site's email | Each message the email driver's inbox reads, as channel `email`; a served site reads the inbox while it serves ([`SiteInbox::read_email`]) |
//! | A chat channel the kernel has | A [`ChannelInbound`] that a [`ChannelSink`] adapter received from an account no person paired ([`SiteInbox::receive_inbound`]); a paired account's message is the owner's, for the kernel's channel gateway |
//!
//! The Inbox runs in the public partition over the edge's public tools — the declared
//! capabilities, `search_site`, `reply_in_thread` and `emit_signal` — under the partition's
//! manifest policy, as the Concierge does. A turn's `reply_in_thread` answers the message's
//! own thread, `<channel>:<thread>`, through the inbox drivers' repliers: the email outbox
//! for `email`, and [`ChannelReplier`] over a chat channel's sink. When the screen flags a
//! message or the policy refuses one of the Inbox's calls, and the Inbox filed no signal,
//! the site files an `inbound_message` signal for it, so the back office hears of every such
//! message and reads none of its text (A-I3).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use adk_awp::v2::capability::META_TIER;
use adk_core::audit_log::AuditTrail;
use adk_core::{
    AdkError, Agent, Content, Llm, Part, PolicyCall, PolicyDecision, RequestContext, Result,
    RunConfig, SessionId, ToolPolicy, ToolPolicyRequest, UserId,
};
use adk_drivers::ThreadReplier;
use adk_harness::website::SitePartitions;
use adk_kernel::Partitions;
use adk_kernel::channels::{ChannelBindingStore, ChannelInbound, ChannelSink, InboundBody};
use adk_runner::Runner;
use adk_server::awp::{
    ConversationScreen, InjectionScreen, META_BINDING, META_LIMIT_KEY, META_THREAD,
    PublicPartitionPolicy, PublicTools, SignalEmitter,
};
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use awp_types::MessageCategory;
use futures::StreamExt as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::team::{AgentRole, SiteTeam};

/// The longest message text the Inbox reads, in characters.
const MAX_TEXT_CHARS: usize = 8_000;

/// One message for the Inbox (spec 3.5 §7.19): untrusted input from a person who is not
/// paired to the site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxMessage {
    /// The channel, such as `email`, `telegram` or `social`: the prefix of its thread
    /// reference.
    pub channel: String,
    /// The sending account on the channel.
    pub account: String,
    /// The thread the message is in, which a reply answers.
    pub thread: String,
    /// What the person wrote.
    pub text: String,
}

impl InboxMessage {
    /// The thread reference `reply_in_thread` answers: `<channel>:<thread>`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_website::inbox::InboxMessage;
    ///
    /// let message = InboxMessage {
    ///     channel: "email".into(),
    ///     account: "wanjiru@mail.example".into(),
    ///     thread: "m-1@mail.example".into(),
    ///     text: "When do you open?".into(),
    /// };
    /// assert_eq!(message.thread_ref(), "email:m-1@mail.example");
    /// ```
    pub fn thread_ref(&self) -> String {
        format!("{}:{}", self.channel, self.thread)
    }
}

/// What one message did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "outcome", rename_all_fields = "camelCase")]
pub enum InboxOutcome {
    /// The Inbox read it.
    Read {
        /// The Inbox's last words in the turn.
        reply: String,
        /// The tools the policy refused in the turn.
        refused: Vec<String>,
        /// Whether the site filed the turn's `inbound_message` signal because the Inbox
        /// filed none for a flagged or refused message.
        site_signal: bool,
    },
    /// A paired account's message: the owner's, for the kernel's channel gateway (A-I24).
    Owner,
    /// Not a message: a pressed answer or a stop, which only the gateway takes.
    Ignored,
}

/// The public partition's policy, noting the calls it refuses in the current turn.
#[derive(Debug)]
struct WatchedPolicy {
    inner: PublicPartitionPolicy,
    refused: Arc<std::sync::Mutex<Vec<String>>>,
}

#[async_trait]
impl ToolPolicy for WatchedPolicy {
    async fn evaluate(&self, request: &ToolPolicyRequest) -> PolicyDecision {
        self.evaluate_call(request, &PolicyCall::new()).await
    }

    async fn evaluate_call(
        &self,
        request: &ToolPolicyRequest,
        call: &PolicyCall<'_>,
    ) -> PolicyDecision {
        let decision = self.inner.evaluate_call(request, call).await;
        if matches!(decision, PolicyDecision::Deny { .. }) {
            self.refused
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(request.tool_name.clone());
        }
        decision
    }

    fn policy_ref(&self) -> Option<String> {
        self.inner.policy_ref()
    }
}

/// What [`SiteInbox::new`] builds the Inbox from.
pub struct InboxParts {
    /// The site's team; the Inbox's instruction carries the kind's public instruction.
    pub team: SiteTeam,
    /// The site's partitions.
    pub partitions: SitePartitions,
    /// The kernel's partition table.
    pub table: Partitions,
    /// The model the Inbox reads with.
    pub model: Arc<dyn Llm>,
    /// The edge's public tools.
    pub tools: PublicTools,
    /// The Inbox's workspace, which holds its playbooks.
    pub workspace: PathBuf,
    /// Where the site's own `inbound_message` signals go: the edge's signals into the back
    /// office.
    pub signals: Arc<dyn SignalEmitter>,
    /// Where refusals are audited.
    pub audit: Option<AuditTrail>,
}

/// The site's Inbox agent (spec 3.5 §7.19).
pub struct SiteInbox {
    runner: Runner,
    sessions: Arc<dyn SessionService>,
    app: String,
    principal: String,
    signals: Arc<dyn SignalEmitter>,
    screen: Arc<dyn ConversationScreen>,
    refused: Arc<std::sync::Mutex<Vec<String>>>,
    bindings: Option<Arc<dyn ChannelBindingStore>>,
    turn: Mutex<()>,
}

impl std::fmt::Debug for SiteInbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SiteInbox").field("app", &self.app).finish_non_exhaustive()
    }
}

fn digest(text: &str) -> String {
    adk_site::release::file_digest(text.as_bytes()).hex()[..24].to_string()
}

impl SiteInbox {
    /// The Inbox of `parts.team`'s site over the edge's public tools (spec 3.5 §7.19,
    /// §7.10): the website preset in the public partition with the Inbox's instruction, and
    /// exactly the tools the edge's public policy admits.
    ///
    /// # Errors
    ///
    /// Returns the harness's or the runner's error when the agent does not build.
    pub fn new(parts: InboxParts) -> Result<Self> {
        let InboxParts { team, partitions, table, model, tools, workspace, signals, audit } = parts;
        let instruction = match team.public_instruction() {
            "" => AgentRole::Inbox.prompt().to_string(),
            kind => format!("{}\n\n{kind}", AgentRole::Inbox.prompt()),
        };
        let served = adk_harness::website::ServedPublicTools {
            partitions,
            table,
            search_site: tools.search_site.clone(),
            reply_in_thread: Some(tools.reply_in_thread.clone()),
            emit_signal: tools.emit_signal.clone(),
            capabilities: tools.capabilities.iter().map(|tool| (tool.clone(), false)).collect(),
        };
        let agent: Arc<dyn Agent> = Arc::new(
            adk_harness::HarnessAgent::builder(AgentRole::Inbox.as_str())
                .model(model)
                .workspace(adk_devtools::Workspace::new(&workspace))
                .website_public_served(served)
                .instruction(instruction)
                .build()?,
        );
        let refused = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
        let app = format!("{}-inbox", team.site().name);
        let mut builder = Runner::builder()
            .app_name(app.clone())
            .agent(agent)
            .session_service(sessions.clone())
            .tool_policy(Arc::new(WatchedPolicy {
                inner: tools.policy(),
                refused: refused.clone(),
            }));
        if let Some(audit) = &audit {
            builder = builder.audit_log(audit.recorder().clone());
        }
        Ok(Self {
            runner: builder.build()?,
            sessions,
            app,
            principal: AgentRole::Inbox.principal(team.site()),
            signals,
            screen: Arc::new(InjectionScreen),
            refused,
            bindings: None,
            turn: Mutex::new(()),
        })
    }

    /// Routes chat messages with the kernel's channel `bindings`: a paired account's message
    /// is the owner's and the Inbox does not read it (A-I24). Without them every message
    /// handed to the Inbox is a stranger's.
    #[must_use]
    pub fn with_bindings(mut self, bindings: Arc<dyn ChannelBindingStore>) -> Self {
        self.bindings = Some(bindings);
        self
    }

    /// Reads `inbound` from a chat channel the kernel has (spec 3.5 §7.19, A-I24): a typed
    /// message from an account no person paired is the Inbox's; a paired account's is
    /// [`InboxOutcome::Owner`]; a pressed answer or a stop is [`InboxOutcome::Ignored`].
    ///
    /// # Errors
    ///
    /// The channel bindings' error, or [`receive`](Self::receive)'s.
    pub async fn receive_inbound(&self, inbound: &ChannelInbound) -> Result<InboxOutcome> {
        let InboundBody::Text { text } = &inbound.body else { return Ok(InboxOutcome::Ignored) };
        if let Some(bindings) = &self.bindings {
            let router = adk_harness::website::AuthoringRouter::new(bindings.clone());
            if matches!(
                router.route(inbound).await?,
                adk_harness::website::AuthoringRoute::Owner { .. }
            ) {
                return Ok(InboxOutcome::Owner);
            }
        }
        self.receive(InboxMessage {
            channel: inbound.channel.clone(),
            account: inbound.account.clone(),
            thread: inbound.thread.clone(),
            text: text.clone(),
        })
        .await
    }

    /// Runs one turn of the Inbox on `message` (spec 3.5 §7.19): its replies go to the
    /// message's own thread, and a flagged or refused message the Inbox filed no signal for
    /// gets the site's `inbound_message` signal.
    ///
    /// # Errors
    ///
    /// The session service's or the runner's error.
    pub async fn receive(&self, message: InboxMessage) -> Result<InboxOutcome> {
        let _turn = self.turn.lock().await;
        let text: String = message.text.chars().take(MAX_TEXT_CHARS).collect();
        let thread_ref = message.thread_ref();
        let user = format!("inbox-{}", digest(&format!("{}:{}", message.channel, message.account)));
        let session = format!("thread-{}", digest(&thread_ref));
        self.ensure_session(&user, &session).await?;
        self.refused.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
        let metadata = std::collections::HashMap::from([
            (META_TIER.to_string(), "public".to_string()),
            (META_THREAD.to_string(), thread_ref.clone()),
            (META_BINDING.to_string(), message.channel.clone()),
            (META_LIMIT_KEY.to_string(), format!("{}:{}", message.channel, message.account)),
        ]);
        let config = RunConfig {
            request_context: Some(RequestContext {
                user_id: user.clone(),
                scopes: Vec::new(),
                metadata,
            }),
            ..RunConfig::default()
        };
        let mut events = self
            .runner
            .run_with_config(
                UserId::try_from(user)?,
                SessionId::try_from(session)?,
                Content::new("user").with_text(text.clone()),
                Some(config),
            )
            .await?;
        let (mut reply, mut signalled) = (String::new(), false);
        while let Some(event) = events.next().await {
            let event = event?;
            if event.author == "user" || event.llm_response.partial {
                continue;
            }
            let Some(content) = &event.llm_response.content else { continue };
            for part in &content.parts {
                match part {
                    Part::FunctionResponse { function_response, .. }
                        if function_response.name == "emit_signal"
                            && function_response.response.get("error").is_none() =>
                    {
                        signalled = true;
                    }
                    _ => {}
                }
            }
            let said: String = content
                .parts
                .iter()
                .filter_map(|part| match part {
                    Part::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            if !said.is_empty() {
                reply = said;
            }
        }
        let refused = std::mem::take(
            &mut *self.refused.lock().unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        let flagged = self.screen.flags(&text);
        let site_signal = (flagged || !refused.is_empty()) && !signalled;
        if site_signal {
            self.file_signal(&message.channel, &thread_ref).await;
        }
        tracing::info!(
            inbox.channel = %message.channel,
            inbox.flagged = flagged,
            inbox.refused = refused.len(),
            inbox.site_signal = site_signal,
            "the Inbox read a message"
        );
        Ok(InboxOutcome::Read { reply, refused, site_signal })
    }

    /// Reads up to `limit` new messages from the site's email after `cursor` and hands each
    /// one a person wrote to the Inbox (spec 3.5 §7.19 "Email and social webhooks"); returns
    /// the cursor after them. Bounces, complaints and unsubscribes are the outbox's to act
    /// on, and a message software sent on its own gets no turn.
    ///
    /// # Errors
    ///
    /// The email driver's error; a message whose turn fails is logged and skipped.
    pub async fn read_email(
        &self,
        outbox: &adk_drivers::email::EmailOutbox,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<Option<String>> {
        let page = outbox
            .read_inbox(cursor, limit)
            .await
            .map_err(|e| AdkError::config(format!("the site's email inbox: {e}")))?;
        for email in &page.messages {
            if email.kind != adk_drivers::email::InboundKind::Message || email.auto_submitted {
                continue;
            }
            let message = InboxMessage {
                channel: "email".into(),
                account: email.from.address.clone(),
                thread: email.thread.clone(),
                text: format!("Subject: {}\n\n{}", email.subject, email.text),
            };
            if let Err(error) = self.receive(message).await {
                tracing::warn!(error = %error, "the Inbox could not read an email");
            }
        }
        Ok(page.cursor.or_else(|| cursor.map(str::to_string)))
    }

    async fn ensure_session(&self, user: &str, session: &str) -> Result<()> {
        let existing = self
            .sessions
            .get(GetRequest {
                app_name: self.app.clone(),
                user_id: user.to_string(),
                session_id: session.to_string(),
                num_recent_events: None,
                after: None,
            })
            .await;
        match existing {
            Ok(_) => Ok(()),
            Err(error) if error.is_not_found() => {
                let created = self
                    .sessions
                    .create(CreateRequest {
                        app_name: self.app.clone(),
                        user_id: user.to_string(),
                        session_id: Some(session.to_string()),
                        state: std::collections::HashMap::new(),
                    })
                    .await;
                match created {
                    Err(error) if !error.is_conflict() => Err(error),
                    _ => Ok(()),
                }
            }
            Err(error) => Err(error),
        }
    }

    /// Files the site's `inbound_message` signal for a thread: typed fields only (A-I3).
    async fn file_signal(&self, channel: &str, thread_ref: &str) {
        let signal = adk_harness::website::inbound_signal(
            channel,
            thread_ref,
            MessageCategory::Other,
            &self.principal,
            chrono::Utc::now(),
        );
        if let Err(error) = self.signals.emit(signal).await {
            tracing::warn!(error = %error, "the Inbox's signal could not be filed");
        }
    }
}

/// `reply_in_thread` over a chat channel the kernel has (spec 3.5 §7.10, §10 X12): a reply
/// to `<channel>:<thread>` goes to that thread through the channel's [`ChannelSink`], and
/// only as the playbook's reply template, whose `body` field is the text.
///
/// # Example
///
/// ```rust
/// use std::collections::BTreeMap;
/// use std::sync::Arc;
/// use adk_drivers::ThreadReplier;
/// use adk_kernel::channels::InMemoryChannel;
/// use adk_website::inbox::ChannelReplier;
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let chat = Arc::new(InMemoryChannel::new("telegram"));
/// let replier = ChannelReplier::new(chat.clone(), "reply");
/// let body = BTreeMap::from([("body".to_string(), "We open at seven.".to_string())]);
/// replier.reply_in_thread("5550", "reply", &body).await.unwrap();
/// assert_eq!(chat.messages("5550"), ["We open at seven."]);
/// assert!(replier.reply_in_thread("5550", "refund", &body).await.is_err());
/// # }
/// ```
pub struct ChannelReplier {
    sink: Arc<dyn ChannelSink>,
    template: String,
}

impl ChannelReplier {
    /// Replies on `sink`'s channel with the template named `template`.
    pub fn new(sink: Arc<dyn ChannelSink>, template: impl Into<String>) -> Self {
        Self { sink, template: template.into() }
    }
}

#[async_trait]
impl ThreadReplier for ChannelReplier {
    fn prefix(&self) -> &str {
        self.sink.channel()
    }

    async fn reply_in_thread(
        &self,
        thread: &str,
        template: &str,
        fields: &BTreeMap<String, String>,
    ) -> adk_drivers::Result<Value> {
        if template != self.template {
            return Err(adk_drivers::DriverError::refused(format!(
                "`{template}` is not one of the playbook's reply templates ({})",
                self.template
            )));
        }
        let body = fields.get("body").ok_or_else(|| {
            adk_drivers::DriverError::invalid(format!("template `{template}` needs field `body`"))
        })?;
        self.sink
            .send_message(thread, body)
            .await
            .map_err(|e| adk_drivers::DriverError::unavailable(self.sink.channel(), e))?;
        Ok(json!({ "status": "sent", "channel": self.sink.channel() }))
    }
}
