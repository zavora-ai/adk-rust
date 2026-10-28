//! The card of a call a person asked for through a drawn form (spec 3.3 §7.1, §7.4 rule 5,
//! wave 4 decision 12): it starts "You pressed Send on a form Finance drew · 10:03", says
//! "From a form Finance drew", and never offers **Always allow**, because a standing rule
//! would let the model call the tool unasked, which is not what the person decided.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use adk_core::{ApprovalRequest, Event, Part, RequestedCall, RequestedViaKind};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::hints::{CardHint, CardHints};
use crate::model::CardDetail;
use crate::project::{self, Projector};
use crate::source::Source;
use crate::words;

/// How many requests' hints are remembered before the memory starts again.
const REMEMBERED: usize = 4_096;

/// How long after a request is created its held call may still be on its way to the session.
const HOLD_WRITTEN_WITHIN_SECS: i64 = 30;

/// How many sends this engine made are remembered for the cards they cause.
const PRESSED: usize = 256;

/// How long after a send its request may still be made: the run that holds the call may
/// wait in its queue first. A later request is never the send's.
const REQUEST_MADE_WITHIN_SECS: i64 = 120;

/// A form this engine sent, or a call an app asked for: its session, call and when.
struct Pressed {
    user: String,
    session: String,
    tool: String,
    args: Value,
    at: DateTime<Utc>,
    /// The requested call's ID.
    call_id: String,
    /// The form and version sent; `None` for an app's call.
    form: Option<(String, u64)>,
    /// The app that asked, as a person reads it; `None` for a form.
    app: Option<String>,
    /// The request this send's call made, once a card matched it: a send has one.
    request: Option<String>,
}

/// The hints of drawn-form calls: the session's held call, which the agent marks with
/// `adk.requested` — runtime data, never text an agent wrote — and, until that reaches the
/// session, the sends this engine made.
pub(crate) struct DrawnHints {
    source: Arc<dyn Source>,
    projector: Projector,
    known: Mutex<HashMap<String, Option<CardHint>>>,
    /// The request is written before the agent's events reach the session, so a send this
    /// engine made is known from the moment its card can be drawn.
    pressed: Mutex<VecDeque<Pressed>>,
}

/// Whether `events` carry the call `call_id`: its run has written it.
fn written(events: &[Event], call_id: &str) -> bool {
    events
        .iter()
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| &content.parts)
        .any(|part| match part {
            Part::FunctionCall { id, .. } | Part::FunctionResponse { id, .. } => {
                id.as_deref() == Some(call_id)
            }
            _ => false,
        })
}

impl DrawnHints {
    pub(crate) fn new(source: Arc<dyn Source>, projector: Projector) -> Self {
        Self {
            source,
            projector,
            known: Mutex::new(HashMap::new()),
            pressed: Mutex::new(VecDeque::new()),
        }
    }

    /// Notes that `user` is sending version `version` of the form `surface` in `session` as
    /// `call`, unless a send of that version is already on its way: one whose call the
    /// session in `events` does not carry yet (spec 3.3 §7.4 rule 5). `false` when one is.
    pub(crate) fn begin_send(
        &self,
        user: &str,
        session: &str,
        (surface, version): (&str, u64),
        call: &RequestedCall,
        events: &[Event],
    ) -> bool {
        let mut pressed = self.pressed.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Utc::now();
        let on_its_way = pressed.iter().any(|earlier| {
            earlier.user == user
                && earlier.session == session
                && earlier.form.as_ref().is_some_and(|(id, at)| id == surface && *at == version)
                && now.signed_duration_since(earlier.at).num_seconds() <= REQUEST_MADE_WITHIN_SECS
                && !written(events, &earlier.call_id)
        });
        if on_its_way {
            return false;
        }
        Self::note(
            &mut pressed,
            Pressed {
                user: user.to_string(),
                session: session.to_string(),
                tool: call.tool_name.clone(),
                args: call.args.clone(),
                at: now,
                call_id: call.function_call_id.clone(),
                form: Some((surface.to_string(), version)),
                app: None,
                request: None,
            },
        );
        true
    }

    /// Notes that the app from `app` in `user`'s `session` asked for `call` (spec 3.3 §6.1
    /// `genui-apps`).
    #[cfg(feature = "genui-apps")]
    pub(crate) fn app_asked(&self, user: &str, session: &str, call: &RequestedCall, app: &str) {
        let mut pressed = self.pressed.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::note(
            &mut pressed,
            Pressed {
                user: user.to_string(),
                session: session.to_string(),
                tool: call.tool_name.clone(),
                args: call.args.clone(),
                at: Utc::now(),
                call_id: call.function_call_id.clone(),
                form: None,
                app: Some(app.to_string()),
                request: None,
            },
        );
    }

    /// Forgets the send of `call_id`, whose call was never asked for.
    pub(crate) fn withdraw(&self, call_id: &str) {
        self.pressed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|pressed| pressed.call_id != call_id);
    }

    fn note(pressed: &mut VecDeque<Pressed>, send: Pressed) {
        if pressed.len() >= PRESSED {
            pressed.pop_front();
        }
        pressed.push_back(send);
    }

    /// The send `request` came from, before its held call reaches the session: the latest
    /// send of the identical call in its session, made at most
    /// [`REQUEST_MADE_WITHIN_SECS`] before the request and not yet matched to another one.
    /// The match is kept, so no later request is ever taken for the same send.
    fn sent_here(&self, request: &ApprovalRequest) -> Option<(DateTime<Utc>, Option<String>)> {
        let mut pressed = self.pressed.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let send = pressed.iter_mut().rev().find(|pressed| {
            pressed.user == request.scope.user_id
                && pressed.session == request.scope.session_id
                && pressed.tool == request.tool_name
                && pressed.args == request.args_redacted
                && pressed.request.as_ref().is_none_or(|id| *id == request.id)
                && pressed.at <= request.created_at + chrono::Duration::seconds(1)
                && request.created_at.signed_duration_since(pressed.at).num_seconds()
                    <= REQUEST_MADE_WITHIN_SECS
        })?;
        send.request = Some(request.id.clone());
        Some((send.at, send.app.clone()))
    }

    /// The hint of a call an app asked for: its first step, its detail line, and no Always
    /// allow (spec 3.3 §6.1 `genui-apps`).
    fn app_hint(&self, app: &str, at: DateTime<Utc>) -> CardHint {
        CardHint::new()
            .without_always_allow()
            .with_first_step(words::app_asked(app, &self.projector.hh_mm(at)))
            .with_detail(CardDetail::new(words::from_the_app(app)))
    }

    /// The hint of a form's call: its first step, its detail line, and no Always allow.
    fn form_hint(&self, request: &ApprovalRequest, at: DateTime<Utc>) -> CardHint {
        let agent = self.projector.agent(&project::agent_name(request)).display_name;
        CardHint::new()
            .without_always_allow()
            .with_first_step(words::pressed_send_on_form(&agent, &self.projector.hh_mm(at)))
            .with_detail(CardDetail::new(words::from_a_form(&agent)))
    }

    fn remember(&self, id: &str, hint: Option<CardHint>) {
        let mut known = self.known.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if known.len() >= REMEMBERED {
            known.clear();
        }
        known.insert(id.to_string(), hint);
    }
}

#[async_trait]
impl CardHints for DrawnHints {
    async fn hint(&self, request: &ApprovalRequest) -> Option<CardHint> {
        if let Some(known) =
            self.known.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&request.id)
        {
            return known.clone();
        }
        // The held call says where it came from; a send is read only until it is written,
        // and only for its own request.
        let events = match self
            .source
            .transcript(&request.scope.user_id, &request.scope.session_id)
            .await
        {
            Ok(Some(events)) => events,
            Ok(None) => Vec::new(),
            Err(error) => {
                tracing::warn!(approval.id = %request.id, error = %error, "could not read the session of a decision; its card says less");
                Vec::new()
            }
        };
        let links = project::links(&events, std::slice::from_ref(request));
        let held = links
            .iter()
            .find(|(_, link)| link.id == request.id)
            .and_then(|(seq, _)| events.get(usize::try_from(*seq).ok()?.checked_sub(1)?));
        let Some(held) = held else {
            if let Some((at, app)) = self.sent_here(request) {
                return Some(match app {
                    Some(app) => self.app_hint(&app, at),
                    None => self.form_hint(request, at),
                });
            }
            // The held call reaches the session just after the request; until then nothing is
            // known for certain.
            if Utc::now().signed_duration_since(request.created_at).num_seconds()
                > HOLD_WRITTEN_WITHIN_SECS
            {
                self.remember(&request.id, None);
            }
            return None;
        };
        // The card says when the person pressed Send, as it did before the hold was written.
        let pressed_at = self
            .pressed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .find(|pressed| pressed.request.as_deref() == Some(request.id.as_str()))
            .map(|pressed| pressed.at);
        let hint = super::requested_via(held).map(|via| match via.kind {
            RequestedViaKind::Form => self.form_hint(request, pressed_at.unwrap_or(held.timestamp)),
            // An app may call only its own server's tools, so the call's origin names the app.
            RequestedViaKind::App => {
                match request.origin.as_ref().and_then(|origin| origin.source.strip_prefix("mcp:"))
                {
                    Some(server) => self.app_hint(&super::app_name(server), held.timestamp),
                    None => CardHint::new().without_always_allow(),
                }
            }
            // Whatever asked for it, a person's request is never made a standing rule.
            _ => CardHint::new().without_always_allow(),
        });
        self.remember(&request.id, hint.clone());
        hint
    }
}
