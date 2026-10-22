//! Standing rules over a served harness's authenticated REST API (spec 3.1 §7.5, §8.2).
//!
//! The routes sit behind the server's auth middleware, as `/api/approvals` does, and need
//! the [`OPERATOR_SCOPE`]: no caller is `401`, a caller without the scope `403`. A rule's
//! subject is always the authenticated caller, never a body field (spec 3.1 §9.2 I2, I5),
//! and every grant and revocation is recorded as `harness.rule.created` or
//! `harness.rule.revoked` with `surface: rest` before the store write.
//!
//! | Route | Effect |
//! |---|---|
//! | `GET /api/rules` | The caller's rules |
//! | `GET /api/rules/suggestions/{approvalId}` | The rules a call held in the caller's session offers |
//! | `POST /api/rules` | `{"approvalId": "apr_…", "suggestion": 0}` remembers a suggestion; `{"tool": "bash", "prefix": "cargo test"}` creates an explicit rule. Optional: `"decision": {"kind": "deny", "reason": "…"}`, `"expiresInHours"`; explicit rules also take `"pathWithin"` and `"sessionId"` |
//! | `DELETE /api/rules/{id}` | Revokes one of the caller's rules |

use std::sync::Arc;

use adk_core::principal::{SubjectKind, SubjectRef};
use adk_core::{
    AdkError, ApprovalService, ErrorCategory, ErrorComponent, RequestContext,
    ToolConfirmationRequest, ToolOrigin,
};
use adk_harness::approvals::matcher::plain_shell_words;
use adk_harness::{
    HarnessAgent, OriginPin, RuleOwner, RulePredicate, RuleScope, SHELL_PREFIX_SUGGESTION_DENYLIST,
    StandingDecision, StandingRule, TableRow,
};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get};
use axum::{Extension, Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use super::serve::OPERATOR_SCOPE;

/// The surface REST grants and revocations are audited with (spec 3.1 §9.3).
const SURFACE: &str = "rest";

/// State of the rule routes: the served harness, its app, and its approval inbox.
#[derive(Clone)]
pub(crate) struct RuleEndpoints {
    pub(crate) agent: Arc<HarnessAgent>,
    pub(crate) app_name: String,
    pub(crate) approvals: Option<Arc<dyn ApprovalService>>,
}

/// The rule routes, to mount under `/api` behind the auth middleware.
pub(crate) fn rules_router(endpoints: RuleEndpoints) -> Router {
    Router::new()
        .route("/rules", get(list).post(create))
        .route("/rules/{id}", delete(revoke))
        .route("/rules/suggestions/{approval_id}", get(suggestions))
        .with_state(endpoints)
}

fn failed(error: &AdkError) -> Response {
    let status =
        StatusCode::from_u16(error.http_status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, Json(error.to_problem_json())).into_response()
}

fn problem(category: ErrorCategory, code: &'static str, message: impl Into<String>) -> Response {
    failed(&AdkError::new(ErrorComponent::Server, category, code, message))
}

fn invalid(message: impl Into<String>) -> Response {
    problem(ErrorCategory::InvalidInput, "harness.rule.invalid", message)
}

/// The authenticated operator's user id: `401` without a caller, `403` without the scope.
fn operator(caller: Option<RequestContext>) -> Result<String, Response> {
    let Some(caller) = caller else {
        return Err(problem(
            ErrorCategory::Unauthorized,
            "harness.rule.unauthenticated",
            "rule routes need an authenticated caller; configure a RequestContextExtractor",
        ));
    };
    if !caller.scopes.iter().any(|scope| scope == OPERATOR_SCOPE) {
        return Err(problem(
            ErrorCategory::Forbidden,
            "harness.rule.forbidden",
            format!("rule routes need the `{OPERATOR_SCOPE}` scope"),
        ));
    }
    Ok(caller.user_id)
}

impl RuleEndpoints {
    /// The call held for approval `id` in one of `user`'s sessions, and its session. Any
    /// other request — unknown, decided, another user's — is `404`.
    async fn held_call(
        &self,
        user: &str,
        id: &str,
    ) -> Result<(ToolConfirmationRequest, String), Response> {
        let not_found = || {
            problem(
                ErrorCategory::NotFound,
                "harness.rule.no_held_call",
                format!("no call of yours is held for approval request '{id}'"),
            )
        };
        let Some(service) = &self.approvals else {
            return Err(not_found());
        };
        let request = match service.get(id).await {
            Ok(Some(request)) => request,
            Ok(None) => return Err(not_found()),
            Err(error) => return Err(failed(&error)),
        };
        if request.scope.app_name != self.app_name || request.scope.user_id != user {
            return Err(not_found());
        }
        let pending = self
            .agent
            .runtime()
            .approval_store()
            .pending(&request.scope)
            .await
            .map_err(|error| failed(&error))?;
        pending
            .into_iter()
            .find(|entry| entry.fingerprint == request.fingerprint)
            .map(|entry| (entry.request, request.scope.session_id.clone()))
            .ok_or_else(not_found)
    }
}

/// `GET /api/rules`: the caller's rules, oldest first.
async fn list(
    State(endpoints): State<RuleEndpoints>,
    Extension(caller): Extension<Option<RequestContext>>,
) -> Response {
    let user = match operator(caller) {
        Ok(user) => user,
        Err(response) => return response,
    };
    let standing = endpoints.agent.runtime().standing_rules();
    match standing.list(&endpoints.app_name, &user).await {
        Ok(mut rules) => {
            rules.sort_by_key(|rule| rule.created_at);
            Json(json!({ "rules": rules })).into_response()
        }
        Err(error) => failed(&error),
    }
}

/// `GET /api/rules/suggestions/{approvalId}`: what the held call offers to remember.
async fn suggestions(
    State(endpoints): State<RuleEndpoints>,
    Extension(caller): Extension<Option<RequestContext>>,
    Path(approval_id): Path<String>,
) -> Response {
    let user = match operator(caller) {
        Ok(user) => user,
        Err(response) => return response,
    };
    match endpoints.held_call(&user, &approval_id).await {
        Ok((call, _)) => Json(json!({
            "approvalId": approval_id,
            "toolName": call.tool_name,
            "suggestions": StandingRule::suggestions(&call),
        }))
        .into_response(),
        Err(response) => response,
    }
}

/// `POST /api/rules` body. Unknown fields, such as a `userId`, are refused: the rule's
/// subject is the caller.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateRule {
    /// An approval request whose held call offers the rule.
    approval_id: Option<String>,
    /// The index of the offered rule, from `GET /api/rules/suggestions/{approvalId}`.
    suggestion: Option<usize>,
    /// An explicit rule's tool: one the served harness declares.
    tool: Option<String>,
    /// Leading command words of a `bash` rule.
    prefix: Option<String>,
    /// A directory inside the workspace an edit rule covers.
    path_within: Option<String>,
    /// Limits an explicit rule to one session.
    session_id: Option<String>,
    /// Allow (default) or deny with a reason.
    decision: Option<StandingDecision>,
    /// When the rule stops applying.
    expires_in_hours: Option<u32>,
}

/// `POST /api/rules`.
async fn create(
    State(endpoints): State<RuleEndpoints>,
    Extension(caller): Extension<Option<RequestContext>>,
    Json(body): Json<CreateRule>,
) -> Response {
    let user = match operator(caller) {
        Ok(user) => user,
        Err(response) => return response,
    };
    let decision = body.decision.clone().unwrap_or(StandingDecision::Allow);
    let created = match (&body.approval_id, &body.tool) {
        (Some(approval_id), None) => {
            if body.prefix.is_some() || body.path_within.is_some() || body.session_id.is_some() {
                return invalid(
                    "a rule from a held call takes the suggestion as offered; create an explicit \
                     rule with `tool` instead",
                );
            }
            let Some(index) = body.suggestion else {
                return invalid(format!(
                    "name the offered rule's index as `suggestion`; GET \
                     /api/rules/suggestions/{approval_id} lists them"
                ));
            };
            let (call, session) = match endpoints.held_call(&user, approval_id).await {
                Ok(held) => held,
                Err(response) => return response,
            };
            let Some(suggestion) = StandingRule::suggestions(&call).into_iter().nth(index) else {
                return invalid(format!("the held call offers no rule {index}"));
            };
            let owner = RuleOwner {
                app_name: endpoints.app_name.clone(),
                user_id: user.clone(),
                subject_kind: SubjectKind::Human,
                session_id: session,
                created_by: SubjectRef::human(user.clone()),
            };
            endpoints
                .agent
                .rule_for_held_call(&call, suggestion, decision, &owner)
                .map(|rule| (rule, Some(call)))
        }
        (None, Some(tool)) => {
            if body.suggestion.is_some() {
                return invalid("`suggestion` needs `approvalId`");
            }
            explicit_rule(&endpoints, &user, tool, &body, decision).map(|rule| (rule, None))
        }
        _ => return invalid("give either `approvalId` and `suggestion`, or `tool`"),
    };
    let (mut rule, call) = match created {
        Ok(created) => created,
        Err(error) => return failed(&error),
    };
    if let Some(hours) = body.expires_in_hours {
        rule.expires_at = Some(rule.created_at + chrono::Duration::hours(i64::from(hours)));
    }
    match endpoints.agent.grant_rule(rule, SURFACE, call.as_ref()).await {
        Ok(rule) => (StatusCode::CREATED, Json(json!({ "rule": rule }))).into_response(),
        Err(error) => failed(&error),
    }
}

/// An explicit rule for `tool`, pinned to the served harness's current declaration of it
/// (spec 3.1 §7.5, §9.2 I8–I11).
fn explicit_rule(
    endpoints: &RuleEndpoints,
    user: &str,
    tool: &str,
    body: &CreateRule,
    decision: StandingDecision,
) -> Result<StandingRule, AdkError> {
    let refuse = |message: String| {
        AdkError::new(
            ErrorComponent::Server,
            ErrorCategory::InvalidInput,
            "harness.rule.invalid",
            message,
        )
    };
    let runtime = endpoints.agent.runtime();
    let declared = runtime.toolset().all_tools();
    let found = declared.iter().find(|candidate| candidate.name() == tool).ok_or_else(|| {
        let names: Vec<&str> = declared.iter().map(|tool| tool.name()).collect();
        refuse(format!(
            "{tool} is not a tool this harness declares ({}); a rule for another tool is \
             created from a held call of it",
            names.join(", ")
        ))
    })?;
    let origin = ToolOrigin::of(found.as_ref());
    let kind = runtime.policy().registry().classify(
        &endpoints.agent.info().name,
        tool,
        Some(&origin),
        &Value::Object(Default::default()),
        found.effect(),
        endpoints.agent.info().isolation.isolation(),
        false,
    );
    if !kind.rule_eligible() || TableRow::of(&kind, false).is_non_waivable() {
        return Err(refuse(format!(
            "{tool} is never covered by a standing rule; decide it each time"
        )));
    }
    let mut predicates = Vec::new();
    let mut label = tool.to_string();
    if let Some(prefix) = &body.prefix {
        let tokens =
            plain_shell_words(prefix).filter(|tokens| !tokens.is_empty()).ok_or_else(|| {
                refuse("`prefix` takes plain command words, such as \"cargo test\"".into())
            })?;
        if SHELL_PREFIX_SUGGESTION_DENYLIST.contains(&tokens[0]) {
            return Err(refuse(format!(
                "a rule cannot start with `{}`: it runs any command",
                tokens[0]
            )));
        }
        label = format!("{tool}: {} …", tokens.join(" "));
        predicates.push(RulePredicate::ShellPrefix {
            pointer: "/command".into(),
            tokens: tokens.into_iter().map(str::to_string).collect(),
        });
    }
    if let Some(dir) = &body.path_within {
        if dir.split(['/', '\\']).any(|part| part == "..") || dir.starts_with('/') {
            return Err(refuse(
                "`pathWithin` takes a directory inside the workspace, such as src".into(),
            ));
        }
        label = format!("{tool}: within {dir}/");
        predicates.push(RulePredicate::PathWithin { pointer: "/path".into(), dir: dir.clone() });
    }
    // Bash rules match only command prefixes (spec 3.1 §9.2 I10).
    if kind.is_shell() && body.prefix.is_none() && decision == StandingDecision::Allow {
        return Err(refuse("a bash allow rule needs `prefix`, such as \"cargo test\"".into()));
    }
    let scope = match &body.session_id {
        Some(session_id) => RuleScope::Session {
            app_name: endpoints.app_name.clone(),
            user_id: user.to_string(),
            subject_kind: SubjectKind::Human,
            session_id: session_id.clone(),
        },
        None => RuleScope::User {
            app_name: endpoints.app_name.clone(),
            user_id: user.to_string(),
            subject_kind: SubjectKind::Human,
        },
    };
    Ok(StandingRule {
        id: uuid::Uuid::new_v4().to_string(),
        scope,
        tool: tool.to_string(),
        origin: OriginPin::exact(&origin),
        agent: None,
        predicates,
        decision,
        created_by: SubjectRef::human(user),
        created_at: chrono::Utc::now(),
        expires_at: None,
        label,
        source_fingerprint: None,
    })
}

/// `DELETE /api/rules/{id}`: revokes one of the caller's rules; another user's is `404`.
async fn revoke(
    State(endpoints): State<RuleEndpoints>,
    Extension(caller): Extension<Option<RequestContext>>,
    Path(id): Path<String>,
) -> Response {
    let user = match operator(caller) {
        Ok(user) => user,
        Err(response) => return response,
    };
    match endpoints.agent.revoke_rule(&endpoints.app_name, &user, &id, SURFACE).await {
        Ok(Some(rule)) => Json(json!({ "revoked": rule })).into_response(),
        Ok(None) => problem(
            ErrorCategory::NotFound,
            "harness.rule.not_found",
            format!("you have no rule '{id}'"),
        ),
        Err(error) => failed(&error),
    }
}
