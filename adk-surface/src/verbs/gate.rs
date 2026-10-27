//! A person's own screen action (spec 3.3 §2 item 7, §7.2, §11, owner question 23).

use std::sync::Arc;
use std::time::Duration;

use adk_core::principal::SubjectKind;
use adk_core::{
    ActingContext, ApprovalScope, GOVERNANCE_FROZEN_CODE, GovernedCall, RunConfig,
    ToolAuthorization, ToolGate, authorize_tool_call,
};
use serde_json::{Map, Value, json};

use super::policy::VerbPolicy;
use super::registry::VerbRegistry;
use super::tools::VerbTool;
use super::{
    ComingUpItem, SURFACE_AUDIT_VERB, VERBS_AGENT, VerbDone, VerbError, VerbIntent, VerbSpec, words,
};
use crate::engine::SurfaceEngine;
use crate::origin::{Caller, Origin};
use crate::rules::{self, Row};

/// A person's screen actions, from any surface that may take them (spec 3.3 §2 item 7, §7.2).
///
/// [`call`](Self::call) runs one for an authenticated [`Caller`]:
///
/// 1. **Where** (§7.2): the home, the phone, a terminal and voice take screen actions; a chat
///    channel and the tray do not, and an agent asks through its own tool.
/// 2. **Whose** (S-I8): only the caller's own items, in any area.
/// 3. **The governed path**: `authorize_tool_call` under `urn:adk:agent:<app>/surface.verbs`,
///    acting for the person, so the call has a `tool_call` record like any other. Stop all
///    applies, except to an action that only delays work, such as snooze (owner question 23).
/// 4. **The record**: `surface.verb`, through the engine's record, stamped with the surface,
///    how it was given, the device and presence; for voice, the transcript's digest, never the
///    words.
///
/// What it did is answered only after it ran.
///
/// # Example
///
/// ```rust,no_run
/// use adk_surface::verbs::{ObjectRef, VerbGate, VerbIntent};
/// use adk_surface::{Caller, Origin, SurfaceKind};
///
/// # async fn example(gate: VerbGate, to: &str) -> Result<(), adk_surface::verbs::VerbError> {
/// let ana = Caller::human("ana", SurfaceKind::Web, Origin::Pointer);
/// let item = gate.coming_up(&ana).await?.remove(0);
/// let snooze = VerbIntent::new("snooze", serde_json::json!({ "to": item.choices[0].to }))
///     .on(ObjectRef::coming_up(&item.id));
/// let done = gate.call(&ana, snooze).await?;
/// println!("{}", done.text);
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct VerbGate {
    registry: VerbRegistry,
    engine: SurfaceEngine,
}

impl std::fmt::Debug for VerbGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerbGate").field("registry", &self.registry).finish_non_exhaustive()
    }
}

impl VerbGate {
    /// A person's actions of `registry`, recorded through `engine`.
    pub fn new(registry: VerbRegistry, engine: SurfaceEngine) -> Self {
        Self { registry, engine }
    }

    /// The registry.
    pub fn registry(&self) -> &VerbRegistry {
        &self.registry
    }

    /// The engine the actions are recorded through.
    pub fn engine(&self) -> &SurfaceEngine {
        &self.engine
    }

    /// How long a spoken action waits before it runs, and can be cancelled: the engine's
    /// window for a spoken "allow once" (spec 3.3 §5.6, owner question 26).
    pub fn voice_cancel_window(&self) -> Duration {
        self.engine.voice_cancel_window()
    }

    /// Refuses a caller who may not take a screen action from where they are.
    fn check_row(caller: &Caller) -> Result<(), VerbError> {
        match rules::row(caller) {
            Row::Web | Row::Terminal | Row::Phone | Row::Voice => Ok(()),
            Row::Chat | Row::Tray => Err(VerbError::NotHere(words::NOT_FROM_HERE.to_string())),
            Row::Agent => {
                Err(VerbError::NotHere(words::AGENTS_ASK_THROUGH_THEIR_TOOLS.to_string()))
            }
        }
    }

    /// The caller's Coming up, with the times each item may move to.
    ///
    /// # Errors
    ///
    /// [`VerbError::NotHere`] from a chat channel, the tray or an agent; the kernel's error.
    pub async fn coming_up(&self, caller: &Caller) -> Result<Vec<ComingUpItem>, VerbError> {
        Self::check_row(caller)?;
        self.registry.scope(caller.user_id(), None).coming_up().await
    }

    /// Takes the action `intent` names, for `caller` (spec 3.3 §2 item 7).
    ///
    /// # Errors
    ///
    /// | Error | When |
    /// |---|---|
    /// | [`VerbError::NotHere`] | From a chat channel, the tray, or an agent |
    /// | [`VerbError::Unknown`] | No such action |
    /// | [`VerbError::Invalid`] | An object kind the action does not take, or its arguments are wrong |
    /// | [`VerbError::NotFound`] | No such item of the caller's |
    /// | [`VerbError::Refused`] | It cannot be done now, or the governed path refused it — Stop all for an action that does not only delay |
    pub async fn call(&self, caller: &Caller, intent: VerbIntent) -> Result<VerbDone, VerbError> {
        self.call_with(caller, intent, None).await
    }

    /// [`call`](Self::call) for words the person said, whose SHA-256 the record carries as
    /// `transcriptDigest` (spec 3.3 §5.6).
    #[cfg(feature = "voice")]
    pub(crate) async fn call_spoken(
        &self,
        caller: &Caller,
        intent: VerbIntent,
        transcript: &str,
    ) -> Result<VerbDone, VerbError> {
        use sha2::Digest as _;
        let digest = format!("{:x}", sha2::Sha256::digest(transcript.as_bytes()));
        self.call_with(caller, intent, Some(digest)).await
    }

    async fn call_with(
        &self,
        caller: &Caller,
        intent: VerbIntent,
        transcript_digest: Option<String>,
    ) -> Result<VerbDone, VerbError> {
        Self::check_row(caller)?;
        let entry = self
            .registry
            .entry(&intent.verb)
            .ok_or_else(|| VerbError::Unknown { verb: intent.verb.clone() })?;
        if let Some(object) =
            intent.objects.iter().find(|object| !entry.spec.objects.contains(&object.kind))
        {
            return Err(VerbError::invalid(format!(
                "`{}` does not act on {:?}",
                entry.spec.name, object.kind
            )));
        }
        let args = intent.call_args();
        let call_id = format!("verb-{}", uuid::Uuid::new_v4());
        let governed = self.authorize(caller, &entry.spec, &call_id, args).await;
        let args = match governed {
            Ok(args) => args,
            Err(error) => {
                self.record(
                    caller,
                    &entry.spec,
                    &intent.call_args(),
                    &call_id,
                    Err(&error),
                    transcript_digest,
                )
                .await;
                return Err(error);
            }
        };
        let scope = self.registry.scope(caller.user_id(), None);
        let result = entry.handler.run(&scope, &args).await;
        if !entry.spec.reads() {
            self.record(caller, &entry.spec, &args, &call_id, result.as_ref(), transcript_digest)
                .await;
        }
        result
    }

    /// The governed path for `caller`'s call (owner question 23): the arguments to run with, or
    /// why not.
    async fn authorize(
        &self,
        caller: &Caller,
        spec: &VerbSpec,
        call_id: &str,
        args: Value,
    ) -> Result<Value, VerbError> {
        let registry = self.registry.inner();
        let app = self.registry.app_name();
        let user = caller.user_id();
        let mut config = RunConfig::builder()
            .tool_policy(Arc::new(VerbPolicy::new(self.registry.clone(), None)));
        // Snoozing only delays work, so it still runs while everything is stopped.
        if !spec.only_delays
            && let Some(control) = &registry.governance
        {
            config = config.governance(control.clone());
        }
        if let Some(trail) = &registry.audit {
            config = config.audit(trail.clone());
        }
        let config = config.build();
        let invocation = format!("surface-verbs-{}", uuid::Uuid::new_v4());
        let scope = ApprovalScope::new(app, user, VERBS_AGENT);
        let acting = ActingContext::derived(app, VERBS_AGENT, user, SubjectKind::Human);
        let gate =
            ToolGate::new(&config, VERBS_AGENT, &invocation, scope).with_acting_context(acting);
        let tool = VerbTool::new(&self.registry, spec, None);
        let call = GovernedCall::for_tool(&tool, call_id, args);
        match authorize_tool_call(&gate, call).await {
            Ok(ToolAuthorization::Execute { args, .. }) => Ok(args),
            Ok(ToolAuthorization::Refuse { reason, .. }) => Err(VerbError::refused(reason)),
            Ok(ToolAuthorization::Pending { reason, .. }) => Err(VerbError::refused(reason)),
            Err(error) if error.code == GOVERNANCE_FROZEN_CODE => Err(VerbError::refused(
                "Everything is stopped: this changes what your agents do, so it waits for Resume all",
            )),
            Err(error) => Err(VerbError::Source(error)),
        }
    }

    /// The `surface.verb` record of `caller`'s action (spec 3.3 §11).
    async fn record(
        &self,
        caller: &Caller,
        spec: &VerbSpec,
        args: &Value,
        call_id: &str,
        outcome: Result<&VerbDone, &VerbError>,
        transcript_digest: Option<String>,
    ) {
        let mut fields = Map::new();
        fields.insert("verb".into(), json!(spec.name));
        if let Some(trail) = &self.registry.inner().audit {
            fields.insert("argsDigest".into(), json!(trail.digest(args)));
        }
        fields.insert("effect".into(), json!(spec.effect.to_string()));
        fields.insert("functionCallId".into(), json!(call_id));
        match outcome {
            Ok(done) => {
                fields.insert("outcome".into(), json!("done"));
                if let Some(target) = &done.target {
                    fields.insert("target".into(), json!(target));
                }
                if let Some(title) = &done.title {
                    fields.insert("title".into(), json!(title));
                }
            }
            Err(error) => {
                fields.insert("outcome".into(), json!("refused"));
                fields.insert("code".into(), json!(error.code()));
                if let Some(item) = args.get("item").and_then(Value::as_str) {
                    fields.insert("target".into(), json!(format!("agenda:{item}")));
                }
            }
        }
        if let Some(digest) = transcript_digest {
            fields.insert("transcriptDigest".into(), json!(digest));
        }
        // An origin the caller declared is the caller's: a person's action never reads as an
        // agent's.
        debug_assert!(!matches!(caller.origin, Origin::Agent { .. }));
        if let Err(error) =
            self.engine.record(caller, SURFACE_AUDIT_VERB, Value::Object(fields)).await
        {
            tracing::warn!(error = %error, verb = %spec.name, "the screen action's record failed");
        }
    }
}
