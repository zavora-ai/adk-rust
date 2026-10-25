//! The phone's doorbell (spec 3.3 §5.4, §7.3, §11 S-I5): a wake-up when a decision waits,
//! none during quiet time, one per phone when quiet time ends, each sent once across replicas.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::{AuditActor, AuditRecordDraft, AuditRecordKind, AuditTrail};
use adk_core::principal::SubjectRef;
use adk_core::{
    ApprovalEventKind, ApprovalNotification, ApprovalNotifier, ApprovalRequest, ApprovalService,
    ApprovalState, Clock, InboxQuery,
};
use adk_kernel::pairing::{DeviceRecord, Pairing};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::SURFACE_AUDIT_NOTIFY;
use super::push::{Deliveries, PushSender, Sent, Subscription, new_wake_id, wake_up};
use super::quiet::QuietTime;
use super::store::{Attachment, Attachments, Holder, PUSH, PUSH_KEY, WAKE};

/// Tells the phone's push task about every decision submitted (spec 3.3 §5.4). Add it to the
/// approval service with `StoreApprovalService::with_notifier`, beside Telegram's doorbell;
/// the decisions it hears about wait for [`Phone::start_push`](super::Phone::start_push).
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::{InMemoryApprovalStore, StoreApprovalService};
/// use adk_surface::mobile::PushDoorbell;
///
/// let doorbell = Arc::new(PushDoorbell::new());
/// let service = StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
///     .with_notifier(doorbell.clone());
/// # let _ = service;
/// ```
pub struct PushDoorbell {
    sender: mpsc::UnboundedSender<ApprovalRequest>,
    receiver: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<ApprovalRequest>>>,
}

impl fmt::Debug for PushDoorbell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PushDoorbell").finish_non_exhaustive()
    }
}

impl PushDoorbell {
    /// A doorbell whose push task has not started yet.
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        Self { sender, receiver: Arc::new(tokio::sync::Mutex::new(receiver)) }
    }

    pub(crate) fn receiver(
        &self,
    ) -> Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<ApprovalRequest>>> {
        Arc::clone(&self.receiver)
    }
}

impl Default for PushDoorbell {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ApprovalNotifier for PushDoorbell {
    /// Passes each submitted decision on; every other change wakes nothing.
    async fn notify(&self, notification: &ApprovalNotification) -> adk_core::Result<()> {
        if notification.kind == ApprovalEventKind::Submitted
            && self.sender.send(notification.request.clone()).is_err()
        {
            tracing::warn!(approval.id = %notification.request.id, "the phone's doorbell is gone; the decision waits in the inbox");
        }
        Ok(())
    }
}

/// A phone that can be woken: an active device of the person with a push subscription.
#[derive(Debug, Clone)]
struct Phone {
    device: DeviceRecord,
    subscription: Subscription,
}

/// What a wake-up's claim covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Claim {
    /// One decision at one version.
    Decision(String),
    /// The end of a quiet time.
    QuietEnded(DateTime<Utc>),
}

impl Claim {
    fn key(&self) -> String {
        match self {
            Self::Decision(key) => key.clone(),
            Self::QuietEnded(at) => format!("quiet:{}", at.to_rfc3339()),
        }
    }
}

fn decision_key(request: &ApprovalRequest) -> String {
    format!("{}:{}", request.id, request.version)
}

/// The push task's parts.
#[derive(Clone)]
pub(crate) struct Ringer {
    pub(crate) pairing: Pairing,
    pub(crate) store: Attachments,
    pub(crate) sender: PushSender,
    pub(crate) quiet: Arc<dyn QuietTime>,
    pub(crate) approvals: Arc<dyn ApprovalService>,
    pub(crate) audit: Option<AuditTrail>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) deliveries: Arc<Deliveries>,
    pub(crate) poll: Duration,
}

impl Ringer {
    /// Runs until the doorbell is dropped: wakes for each decision it hears about, sweeps at
    /// the start and at each end of quiet time.
    pub(crate) async fn run(
        self,
        receiver: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<ApprovalRequest>>>,
    ) {
        let mut receiver = receiver.lock().await;
        let mut quiet_end = self.quiet.until(None, self.clock.now()).await;
        if quiet_end.is_none() {
            self.sweep(None).await;
        }
        loop {
            let wait = match quiet_end {
                Some(end) => (end - self.clock.now()).to_std().unwrap_or_default().min(self.poll),
                None => self.poll,
            };
            tokio::select! {
                request = receiver.recv() => match request {
                    Some(request) => {
                        let ringer = self.clone();
                        tokio::spawn(async move { ringer.submitted(request).await });
                    }
                    None => return,
                },
                () = tokio::time::sleep(wait) => {
                    let now = self.clock.now();
                    match self.quiet.until(None, now).await {
                        Some(end) => quiet_end = Some(end),
                        None => {
                            if let Some(end) = quiet_end.take() {
                                self.sweep(Some(end)).await;
                            }
                        }
                    }
                }
            }
        }
    }

    /// A decision was submitted: wake each of its person's phones now, or when its quiet time
    /// ends.
    pub(crate) async fn submitted(&self, request: ApprovalRequest) {
        if request.state != ApprovalState::Pending {
            return;
        }
        let phones = match self.phones_for(&request).await {
            Ok(phones) => phones,
            Err(error) => {
                tracing::warn!(approval.id = %request.id, error = %error, "the phones to wake could not be read");
                return;
            }
        };
        if phones.is_empty() {
            return;
        }
        let now = self.clock.now();
        if let Some(until) = self.quiet.until(request.run_id.as_deref(), now).await {
            for phone in &phones {
                self.record(&phone.device, std::slice::from_ref(&request.id), "held", None).await;
            }
            tracing::debug!(approval.id = %request.id, quiet.until = %until, "wake-up held for quiet time");
            // Its area's quiet time may end apart from the default area's, which the sweep
            // follows; ending together, they share one claim.
            let ringer = self.clone();
            tokio::spawn(async move {
                // Reads the clock again each poll, so a clock set forward ends the wait.
                loop {
                    let left = (until - ringer.clock.now()).to_std().unwrap_or_default();
                    if left.is_zero() {
                        break;
                    }
                    tokio::time::sleep(left.min(ringer.poll)).await;
                }
                let Ok(Some(current)) = ringer.approvals.get(&request.id).await else { return };
                if current.state != ApprovalState::Pending {
                    return;
                }
                for phone in ringer.phones_for(&current).await.unwrap_or_default() {
                    ringer
                        .wake(&phone, &Claim::QuietEnded(until), std::slice::from_ref(&current))
                        .await;
                }
            });
            return;
        }
        for phone in &phones {
            self.wake(
                phone,
                &Claim::Decision(decision_key(&request)),
                std::slice::from_ref(&request),
            )
            .await;
        }
    }

    /// Each phone whose person has a waiting decision gets one wake-up: at the end of quiet
    /// time `ended`, claimed by that end; at the start, claimed by the first decision no
    /// wake-up covered yet, so a restart wakes no phone twice for one decision.
    pub(crate) async fn sweep(&self, ended: Option<DateTime<Utc>>) {
        let phones = match self.phones().await {
            Ok(phones) => phones,
            Err(error) => {
                tracing::warn!(error = %error, "the phones to wake could not be read");
                return;
            }
        };
        for phone in phones {
            let waiting = match self.waiting(&phone.device).await {
                Ok(waiting) => waiting,
                Err(error) => {
                    tracing::warn!(device.id = %phone.device.id, error = %error, "the waiting decisions could not be read");
                    continue;
                }
            };
            if waiting.is_empty() {
                continue;
            }
            let claim = match ended {
                Some(end) => Claim::QuietEnded(end),
                None => {
                    let holder = Holder::Device(phone.device.id.clone());
                    let covered: Vec<String> = match self.store.list(&holder, WAKE).await {
                        Ok(rows) => rows.into_iter().map(|row| row.key).collect(),
                        Err(error) => {
                            tracing::warn!(device.id = %phone.device.id, error = %error, "the wake-ups sent could not be read");
                            continue;
                        }
                    };
                    match waiting.iter().map(decision_key).find(|key| !covered.contains(key)) {
                        Some(first) => Claim::Decision(first),
                        None => continue,
                    }
                }
            };
            self.wake(&phone, &claim, &waiting).await;
        }
    }

    /// Sends `phone` one wake-up for `covers`, unless `claim` was taken here or on another
    /// replica. Every decision it covers is marked woken.
    async fn wake(&self, phone: &Phone, claim: &Claim, covers: &[ApprovalRequest]) {
        let holder = Holder::Device(phone.device.id.clone());
        let now = self.clock.now();
        let ids: Vec<String> = covers.iter().map(|request| request.id.clone()).collect();
        let row =
            Attachment::new(holder.clone(), WAKE, claim.key(), json!({ "approvalIds": ids }), now);
        match self.store.put_if_absent(row).await {
            Ok(true) => {}
            Ok(false) => return,
            Err(error) => {
                tracing::warn!(device.id = %phone.device.id, error = %error, "a wake-up could not be claimed; it is not sent");
                return;
            }
        }
        for request in covers {
            let row = Attachment::new(holder.clone(), WAKE, decision_key(request), json!({}), now);
            if let Err(error) = self.store.put_if_absent(row).await {
                tracing::warn!(device.id = %phone.device.id, error = %error, "a woken decision could not be marked");
            }
        }
        let id = new_wake_id();
        match self.sender.send(&phone.subscription, &wake_up(&id)).await {
            Sent::Delivered => {
                self.deliveries.sent(&id, &phone.device.id, self.clock.now());
                self.record(&phone.device, &ids, "sent", None).await;
            }
            Sent::Gone => {
                if let Err(error) = self.store.remove(&holder, PUSH, PUSH_KEY).await {
                    tracing::warn!(device.id = %phone.device.id, error = %error, "a gone subscription could not be removed");
                }
                self.record(&phone.device, &ids, "dropped", Some("the subscription is gone")).await;
            }
            Sent::Failed(reason) => {
                tracing::warn!(device.id = %phone.device.id, reason = %reason, "a wake-up could not be sent");
                self.record(&phone.device, &ids, "dropped", Some(&reason)).await;
            }
        }
    }

    /// Every phone that can be woken.
    async fn phones(&self) -> adk_core::Result<Vec<Phone>> {
        let now = self.clock.now();
        let mut phones = Vec::new();
        for device in self.pairing.devices().await.map_err(adk_core::AdkError::from)? {
            if !device.is_active(now) {
                continue;
            }
            let holder = Holder::Device(device.id.clone());
            let Some(row) = self.store.list(&holder, PUSH).await?.into_iter().next() else {
                continue;
            };
            match serde_json::from_value::<Subscription>(row.record) {
                Ok(subscription) => phones.push(Phone { device, subscription }),
                Err(error) => {
                    tracing::warn!(holder = %holder.key(), error = %error, "a push subscription could not be read");
                }
            }
        }
        Ok(phones)
    }

    /// The phones of `request`'s person that may answer it.
    async fn phones_for(&self, request: &ApprovalRequest) -> adk_core::Result<Vec<Phone>> {
        let mut phones = self.phones().await?;
        phones.retain(|phone| {
            phone.device.owner == request.scope.user_id && may_answer(request, &phone.device)
        });
        Ok(phones)
    }

    /// The decisions waiting for `device`'s person that it may answer.
    async fn waiting(&self, device: &DeviceRecord) -> adk_core::Result<Vec<ApprovalRequest>> {
        let mut waiting = Vec::new();
        let mut cursor = None;
        loop {
            let query = InboxQuery::for_approver(
                SubjectRef::human(device.owner.clone()),
                device.groups.clone(),
            )
            .with_cursor(cursor)
            .with_limit(200);
            let page = self.approvals.inbox(query).await?;
            waiting.extend(page.items.into_iter().filter(|request| {
                request.state == ApprovalState::Pending
                    && request.scope.user_id == device.owner
                    && may_answer(request, device)
            }));
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        waiting.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id)));
        Ok(waiting)
    }

    /// Writes `surface.notify` (spec 3.3 §11): the device, the decisions and what happened.
    async fn record(
        &self,
        device: &DeviceRecord,
        ids: &[String],
        outcome: &str,
        reason: Option<&str>,
    ) {
        let Some(trail) = &self.audit else { return };
        let mut draft = AuditRecordDraft::new(
            AuditRecordKind::Other("surface".to_string()),
            SURFACE_AUDIT_NOTIFY,
        )
        .actor(AuditActor::subject(SubjectRef::human(device.owner.clone())))
        .ext("surface", json!("phone"))
        .ext("device", json!(device.id))
        .ext("approvalIds", json!(ids))
        .ext("outcome", json!(outcome));
        if let Some(reason) = reason {
            draft = draft.ext("reason", Value::String(reason.to_string()));
        }
        if let Err(error) = trail.record(draft).await {
            tracing::warn!(device.id = %device.id, error = %error, "a wake-up's record failed");
        }
    }
}

/// Whether `device`'s person may answer `request`: an approver the policy names, and not its
/// requester when the policy forbids self-approval.
fn may_answer(request: &ApprovalRequest, device: &DeviceRecord) -> bool {
    let person = SubjectRef::human(device.owner.clone());
    request.is_eligible(&person, &device.groups)
        && !(request.policy.forbid_self_approval && request.is_self(&person))
}
