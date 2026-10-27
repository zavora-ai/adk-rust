//! Calls a person asked for through a drawn form, in the thread (spec 3.3 §7.4 rule 5): the
//! [`DrawnSent`] item an agent's events make, and each sent form's [`FormState`], from its
//! decision and its call's result, never from the click.

use std::collections::HashMap;

use adk_core::{Event, RequestedCall, RequestedVia, RequestedViaKind};
use serde_json::Value;

use super::drawn::{DrawnSent, FormState, FormStatus, SentVia};
use crate::model::{DecidedAnswer, FeedEntry, ThreadItem};
use crate::words;

/// The drawn surface and action a call a person asked for came from, when `event` is one of
/// the events the agent wrote for it.
pub(crate) fn requested_via(event: &Event) -> Option<RequestedVia> {
    RequestedCall::via_of(event)
}

/// The item of a requested call: held with `decision_id`, or refused before it was held.
pub(crate) fn sent(
    via: &RequestedVia,
    tool: &str,
    call_id: &str,
    event: &Event,
    hh_mm: &str,
    refused_by: Option<&str>,
    decision_id: Option<String>,
) -> ThreadItem {
    let sent_via = match via.kind {
        RequestedViaKind::Form => SentVia::Form,
        _ => SentVia::App,
    };
    ThreadItem::DrawnSent {
        sent: DrawnSent {
            surface_id: via.surface_id.clone(),
            version: via.version,
            action_id: via.action_id.clone(),
            tool: tool.to_string(),
            call_id: call_id.to_string(),
            decision_id,
            held: refused_by.is_none(),
            at: event.timestamp,
            text: match (refused_by, sent_via) {
                (Some(agent), _) => words::cant_use_here(agent, tool),
                (None, SentVia::Form) => words::sent_at(hh_mm),
                (None, SentVia::App) => words::app_asked_at(hh_mm),
            },
            via: sent_via,
        },
    }
}

/// Whether `call_id` is a call a person asked for, whose result says how their form ended.
pub(crate) fn is_requested(call_id: Option<&String>) -> bool {
    call_id.is_some_and(|id| RequestedCall::is_requested_id(id))
}

/// Whether `response`, the answer to a requested call that was allowed, says the call was
/// refused when it resumed because the agent's rules changed (spec 3.3 §7.4 rule 5).
pub(crate) fn refused_on_resume(tool: &str, response: &Value) -> bool {
    response
        .get("error")
        .and_then(Value::as_str)
        .is_some_and(|error| error.starts_with(&format!("Tool '{tool}' denied by policy")))
}

/// The state of every drawn form sent in `entries`, its latest send per version, from how its
/// decision ended — `decided` answers for a decision id, `None` while it is open or unknown —
/// and the call's result (spec 3.3 §7.4 rule 5).
///
/// | Sent call | State |
/// |---|---|
/// | Refused by the drawing agent's rules | "Finance can't use send_invoice here"; the form can be sent again |
/// | Held, its decision open | "Sent at 10:03 · waiting for you" |
/// | Answered yes, still running | "Sent at 10:03" |
/// | Ran | "Sent · done at 10:04" |
/// | Ran and failed | "Sent · failed" |
/// | Refused when it resumed | "Not run: Finance can't use send_invoice any more" |
/// | Answered no | "Sent · you said no" |
/// | Ended without an answer | "Ended without an answer" |
///
/// # Example
///
/// ```rust
/// use adk_surface::genui::form_states;
///
/// assert!(form_states(&[], |_| None).is_empty());
/// ```
pub fn form_states(
    entries: &[FeedEntry],
    decided: impl Fn(&str) -> Option<DecidedAnswer>,
) -> Vec<FormState> {
    let mut sends: Vec<(&DrawnSent, String)> = Vec::new();
    let mut results: HashMap<&str, (bool, &str, &str)> = HashMap::new();
    for entry in entries {
        for item in &entry.items {
            match item {
                // A call an app asked for is not a form's send.
                ThreadItem::DrawnSent { sent } if sent.via == SentVia::App => {}
                ThreadItem::DrawnSent { sent } => {
                    sends.retain(|(known, _)| {
                        (known.surface_id.as_str(), known.version)
                            != (sent.surface_id.as_str(), sent.version)
                    });
                    sends.push((sent, entry.at_text.clone()));
                }
                ThreadItem::Result { call_id: Some(id), ok, text, .. } => {
                    results.insert(id.as_str(), (*ok, text.as_str(), entry.at_text.as_str()));
                }
                _ => {}
            }
        }
    }
    sends
        .into_iter()
        .map(|(sent, at_text)| {
            let answer = sent.decision_id.as_deref().and_then(&decided);
            let said_no = answer == Some(DecidedAnswer::No);
            let (status, text) = match (sent.held, results.get(sent.call_id.as_str())) {
                (false, _) => (FormStatus::Refused, sent.text.clone()),
                (true, _) if answer == Some(DecidedAnswer::Ended) => {
                    (FormStatus::Ended, words::ENDED_WITHOUT_AN_ANSWER.to_string())
                }
                (true, Some((true, _, done_at))) => (FormStatus::Done, words::sent_done(done_at)),
                (true, Some(_)) if said_no => (FormStatus::SaidNo, words::SENT_YOU_SAID_NO.into()),
                (true, Some((false, result, _))) if *result != words::FAILED => {
                    (FormStatus::NotRun, (*result).to_string())
                }
                (true, Some(_)) => (FormStatus::Failed, words::SENT_FAILED.to_string()),
                (true, None) => match answer {
                    None => (FormStatus::Waiting, words::sent_waiting(&at_text)),
                    Some(DecidedAnswer::No) => {
                        (FormStatus::SaidNo, words::SENT_YOU_SAID_NO.to_string())
                    }
                    Some(DecidedAnswer::Ended) => {
                        (FormStatus::Ended, words::ENDED_WITHOUT_AN_ANSWER.to_string())
                    }
                    Some(
                        DecidedAnswer::Yes
                        | DecidedAnswer::Edited
                        | DecidedAnswer::Replied
                        | DecidedAnswer::Unknown,
                    ) => (FormStatus::Sent, words::sent_at(&at_text)),
                },
            };
            FormState {
                surface_id: sent.surface_id.clone(),
                version: sent.version,
                locked: status != FormStatus::Refused,
                status,
                text,
                decision_id: sent.decision_id.clone(),
            }
        })
        .collect()
}
