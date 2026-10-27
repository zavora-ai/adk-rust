//! The card of a call a person asked for through a drawn form (spec 3.3 §7.1, §7.4 rule 5,
//! wave 4 decision 12): it starts "You pressed Send on a form Finance drew · 10:03", says
//! "From a form Finance drew", and never offers **Always allow**, because a standing rule
//! would let the model call the tool unasked, which is not what the person decided.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use adk_core::{ApprovalRequest, RequestedViaKind};
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

/// A form this engine sent, or a call an app asked for: its session, call and when.
struct Pressed {
    user: String,
    session: String,
    tool: String,
    args: Value,
    at: DateTime<Utc>,
    /// The app that asked, as a person reads it; `None` for a form.
    app: Option<String>,
}

/// The hints of drawn-form calls: the sends this engine made, and the session's held call,
/// which the agent marks with `adk.requested` — runtime data, never text an agent wrote.
pub(crate) struct DrawnHints {
    source: Arc<dyn Source>,
    projector: Projector,
    known: Mutex<HashMap<String, Option<CardHint>>>,
    /// The request is written before the agent's events reach the session, so a send this
    /// engine made is known from the moment its card can be drawn.
    pressed: Mutex<VecDeque<Pressed>>,
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

    /// Notes that `user` sent a drawn form in `session` asking for `tool` with `args`.
    pub(crate) fn pressed(&self, user: &str, session: &str, tool: &str, args: &Value) {
        self.note(user, session, tool, args, None);
    }

    /// Notes that the app from `app` in `user`'s `session` asked for `tool` with `args`
    /// (spec 3.3 §6.1 `genui-apps`).
    #[cfg(feature = "genui-apps")]
    pub(crate) fn app_asked(&self, user: &str, session: &str, tool: &str, args: &Value, app: &str) {
        self.note(user, session, tool, args, Some(app.to_string()));
    }

    fn note(&self, user: &str, session: &str, tool: &str, args: &Value, app: Option<String>) {
        let mut pressed = self.pressed.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if pressed.len() >= PRESSED {
            pressed.pop_front();
        }
        pressed.push_back(Pressed {
            user: user.to_string(),
            session: session.to_string(),
            tool: tool.to_string(),
            args: args.clone(),
            at: Utc::now(),
            app,
        });
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
        let sent_here = self
            .pressed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .rev()
            .find(|pressed| {
                pressed.user == request.scope.user_id
                    && pressed.session == request.scope.session_id
                    && pressed.tool == request.tool_name
                    && pressed.args == request.args_redacted
                    && pressed.at <= request.created_at + chrono::Duration::seconds(1)
            })
            .map(|pressed| (pressed.at, pressed.app.clone()));
        if let Some((at, app)) = sent_here {
            let hint = match app {
                Some(app) => self.app_hint(&app, at),
                None => self.form_hint(request, at),
            };
            self.remember(&request.id, Some(hint.clone()));
            return Some(hint);
        }
        let events = match self
            .source
            .transcript(&request.scope.user_id, &request.scope.session_id)
            .await
        {
            Ok(Some(events)) => events,
            Ok(None) => return None,
            Err(error) => {
                tracing::warn!(approval.id = %request.id, error = %error, "could not read the session of a decision; its card says less");
                return None;
            }
        };
        let links = project::links(&events, std::slice::from_ref(request));
        let held = links
            .iter()
            .find(|(_, link)| link.id == request.id)
            .and_then(|(seq, _)| events.get(usize::try_from(*seq).ok()?.checked_sub(1)?));
        let Some(held) = held else {
            // The held call reaches the session just after the request; until then nothing is
            // known for certain.
            if Utc::now().signed_duration_since(request.created_at).num_seconds()
                > HOLD_WRITTEN_WITHIN_SECS
            {
                self.remember(&request.id, None);
            }
            return None;
        };
        let hint = super::requested_via(held).map(|via| match via.kind {
            RequestedViaKind::Form => self.form_hint(request, held.timestamp),
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
