//! The owner's chat channel as an authoring surface (spec 3.5 §5 S18, §7.8.2 "Entry points",
//! "Media", A-I24): the photos a paired person sends join the draft their message made, and
//! they confirm the photos' alt text from the same thread.
//!
//! A channel adapter (WhatsApp, Telegram) hands [`OwnerChannel`] what a model never sees:
//! the message's photos. [`OwnerChannel::attach`] runs them through the back office's media
//! intake into the Steward's draft — only for a device the owner paired (A-I24) — and returns
//! the message the thread gets back, which lists each photo's drafted alt text.
//! [`OwnerChannel::reply`] reads the person's answer: `yes` confirms them all; `2: text`
//! replaces the second one's and confirms it. A confirmation applies to the item once it is
//! in the content store — scheduled or published — so a reply sent before the draft is
//! scheduled waits until [`OwnerChannel::settle`] finds it there.

use std::collections::BTreeMap;
use std::sync::Arc;

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result, Tool};
use adk_harness::website::{AuthoringRoute, AuthoringRouter, SiteRuntime, back_office_tools};
use adk_kernel::channels::{ChannelInbound, InboundBody};
use awp_types::ContentId;
use base64::Engine as _;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex;

/// A photo a channel message carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelPhoto {
    /// Its file name, such as `rye-1.jpg`.
    pub name: String,
    /// Its bytes, as the channel delivered them.
    pub bytes: Bytes,
    /// The caption the person wrote under it, which drafts its alt text.
    pub caption: Option<String>,
}

/// One photo whose alt text waits for the person.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingAlt {
    /// The item the photo belongs to.
    pub item: ContentId,
    /// The photo's path in the content repository.
    pub path: String,
    /// The alt text: drafted, or the person's.
    pub alt: String,
    /// Whether the person confirmed it.
    pub confirmed: bool,
}

/// What [`OwnerChannel::attach`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attached {
    /// The photos attached, in order, with their drafted alt text.
    pub photos: Vec<PendingAlt>,
    /// The message the thread gets back.
    pub reply: String,
}

/// The owner's chat channel over a site's runtime (spec 3.5 §5 S18).
pub struct OwnerChannel {
    runtime: Arc<SiteRuntime>,
    router: AuthoringRouter,
    intake: Arc<dyn Tool>,
    pending: Mutex<BTreeMap<String, Vec<PendingAlt>>>,
}

impl std::fmt::Debug for OwnerChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OwnerChannel").finish_non_exhaustive()
    }
}

fn refused(code: &'static str, message: impl Into<String>) -> AdkError {
    AdkError::new(ErrorComponent::Tool, ErrorCategory::Forbidden, code, message)
}

impl OwnerChannel {
    /// The channel for `runtime`, whose paired devices `router` knows.
    ///
    /// # Errors
    ///
    /// When the runtime's back office has no media intake.
    pub fn new(runtime: Arc<SiteRuntime>, router: AuthoringRouter) -> Result<Self> {
        let intake = back_office_tools(&runtime)
            .into_iter()
            .find(|tool| tool.name() == adk_harness::website::MEDIA_INTAKE)
            .ok_or_else(|| AdkError::config("the back office has no media intake"))?;
        Ok(Self { runtime, router, intake, pending: Mutex::new(BTreeMap::new()) })
    }

    /// The person a paired device of `inbound` belongs to; refuses anyone else (A-I24).
    async fn paired(&self, inbound: &ChannelInbound) -> Result<String> {
        match self.router.route(inbound).await? {
            AuthoringRoute::Owner { user_id, .. } => Ok(user_id),
            AuthoringRoute::Public { .. } => Err(refused(
                "website.channel.unpaired",
                "only a device the owner paired adds media to the site (spec 3.5 A-I24)",
            )),
        }
    }

    /// Attaches `photos` from the paired person's `inbound` message to `draft` — its item
    /// `item`, else its first — through the back office's media intake (spec 3.5 §7.8.2
    /// "Media"). Each photo's alt text is drafted from its caption, else from `title`; the
    /// returned reply asks the person to confirm them.
    ///
    /// # Errors
    ///
    /// `Forbidden` for a device no one paired; the media pipeline's refusal.
    pub async fn attach(
        &self,
        inbound: &ChannelInbound,
        draft: &str,
        item: &ContentId,
        title: &str,
        photos: Vec<ChannelPhoto>,
    ) -> Result<Attached> {
        let person = self.paired(inbound).await?;
        let ctx =
            Arc::new(adk_tool::SimpleToolContext::new(format!("{}:{person}", inbound.channel)));
        let mut attached = Vec::new();
        for (n, photo) in photos.into_iter().enumerate() {
            let alt = photo
                .caption
                .as_deref()
                .map(str::trim)
                .filter(|caption| !caption.is_empty())
                .map_or_else(|| format!("{title}, photo {}", n + 1), str::to_string);
            let args = json!({
                "draft": draft,
                "item": item.0,
                "name": photo.name,
                "dataBase64": base64::engine::general_purpose::STANDARD.encode(&photo.bytes),
                "alt": alt,
            });
            let out = self.intake.execute(ctx.clone(), args).await?;
            let path = out["media"]["path"].as_str().unwrap_or_default().to_string();
            attached.push(PendingAlt { item: item.clone(), path, alt, confirmed: false });
        }
        self.pending
            .lock()
            .await
            .entry(inbound.thread.clone())
            .or_default()
            .extend(attached.clone());
        let mut reply = format!(
            "Added {} photo{} to {title}. Their descriptions for people who cannot see them:",
            attached.len(),
            if attached.len() == 1 { "" } else { "s" }
        );
        for (n, photo) in attached.iter().enumerate() {
            reply.push_str(&format!("\n{}: {}", n + 1, photo.alt));
        }
        reply.push_str("\nReply yes to confirm them, or a number and new text to change one.");
        Ok(Attached { photos: attached, reply })
    }

    /// Reads the person's reply on a thread with photos waiting (spec 3.5 §5 S18): `yes`
    /// confirms every one, `n: text` replaces the n-th's alt text and confirms it. Returns the
    /// answer for the thread, or `None` when the message is not about alt text, so it goes to
    /// the Steward as usual.
    ///
    /// # Errors
    ///
    /// `Forbidden` for a device no one paired; the content store's error.
    pub async fn reply(&self, inbound: &ChannelInbound) -> Result<Option<String>> {
        let text = match &inbound.body {
            InboundBody::Text { text } => text.trim().to_string(),
            _ => return Ok(None),
        };
        if !self.pending.lock().await.contains_key(&inbound.thread) {
            return Ok(None);
        }
        self.paired(inbound).await?;
        let lower = text.to_lowercase();
        let answer = {
            let mut pending = self.pending.lock().await;
            let Some(photos) = pending.get_mut(&inbound.thread) else { return Ok(None) };
            if matches!(lower.as_str(), "yes" | "y" | "ok" | "okay" | "confirm" | "confirmed") {
                for photo in photos.iter_mut() {
                    photo.confirmed = true;
                }
                "Thank you: the descriptions are confirmed.".to_string()
            } else if let Some((number, alt)) = text.split_once(':')
                && let Ok(n) = number.trim().parse::<usize>()
                && (1..=photos.len()).contains(&n)
                && !alt.trim().is_empty()
            {
                let photo = &mut photos[n - 1];
                photo.alt = alt.trim().to_string();
                photo.confirmed = true;
                format!("Photo {n} now reads: {}", photo.alt)
            } else {
                return Ok(None);
            }
        };
        self.settle().await?;
        Ok(Some(answer))
    }

    /// Applies every confirmed alt text to its item in the content store; returns how many
    /// photos it confirmed. A photo whose item is not in the store yet stays waiting.
    ///
    /// # Errors
    ///
    /// The content store's error.
    pub async fn settle(&self) -> Result<usize> {
        let mut pending = self.pending.lock().await;
        let store = self.runtime.store();
        let site = |e: adk_site::SiteError| AdkError::config(e.to_string());
        let mut applied = 0;
        for photos in pending.values_mut() {
            let mut kept = Vec::new();
            for photo in photos.drain(..) {
                if !photo.confirmed {
                    kept.push(photo);
                    continue;
                }
                let Some(mut item) = store.get(&photo.item).await.map_err(site)? else {
                    kept.push(photo);
                    continue;
                };
                let Some(media) = item.media.iter_mut().find(|m| m.path == photo.path) else {
                    kept.push(photo);
                    continue;
                };
                media.alt = Some(photo.alt.clone());
                media.alt_confirmed = true;
                store.put(&item).await.map_err(site)?;
                applied += 1;
            }
            *photos = kept;
        }
        pending.retain(|_, photos| !photos.is_empty());
        Ok(applied)
    }

    /// The photos still waiting on `thread`.
    pub async fn waiting(&self, thread: &str) -> Vec<PendingAlt> {
        self.pending.lock().await.get(thread).cloned().unwrap_or_default()
    }
}
