//! Resolves who a run's agents act as, and for whom, at the start of the run.

use std::collections::BTreeSet;
use std::sync::Arc;

use adk_core::audit_log::{
    AuditActor, AuditDecision, AuditRecordDraft, AuditRecordKind, AuditRecordOutcome,
};
use adk_core::{
    ActingContext, Agent, IdentitySource, Result, RunConfig, SUBJECT_MISMATCH_CODE,
    SubjectDelegationPolicy,
};

/// What a runner resolves each run's identity from.
#[derive(Clone)]
pub(crate) struct ActingResolver {
    pub(crate) app_name: String,
    pub(crate) root: Arc<dyn Agent>,
    pub(crate) source: Option<Arc<dyn IdentitySource>>,
    pub(crate) delegation: Option<Arc<dyn SubjectDelegationPolicy>>,
}

impl ActingResolver {
    /// Adds the identity source's principals for the agent tree to `config.acting` and
    /// pins the run's subject to the session's user.
    ///
    /// A principal `config` already names for an agent is kept. A refused subject is
    /// recorded on the run's audit trail before the error is returned.
    pub(crate) async fn resolve(
        &self,
        config: &mut RunConfig,
        user_id: &str,
        session_id: &str,
        invocation_id: &str,
    ) -> Result<()> {
        let root_name = self.root.name();
        if let Some(source) = &self.source {
            if !config.acting.principals.contains_key(root_name) {
                let principal = match source.principal_for(root_name).await? {
                    Some(principal) => principal,
                    None => source.current().await?,
                };
                config.acting.principals.insert(root_name.to_string(), principal);
            }
            let mut names = BTreeSet::new();
            collect_descendants(&self.root, &mut names);
            for name in names {
                if config.acting.principals.contains_key(&name) {
                    continue;
                }
                if let Some(principal) = source.principal_for(&name).await? {
                    config.acting.principals.insert(name, principal);
                }
            }
        }

        let root_principal = config.acting.principal_for(&self.app_name, root_name);
        let requested = config.acting.on_behalf_of.clone();
        let admitted = config
            .acting
            .admit_subject(&self.app_name, user_id, &root_principal, self.delegation.as_deref())
            .await;
        if let Err(error) = admitted {
            if error.code == SUBJECT_MISMATCH_CODE
                && let Some(trail) = &config.audit
            {
                let acting = ActingContext {
                    agent: root_principal,
                    on_behalf_of: requested,
                    delegation_chain: config.acting.delegation_chain.clone(),
                };
                let draft = AuditRecordDraft::new(AuditRecordKind::Access, SUBJECT_MISMATCH_CODE)
                    .actor(AuditActor::new(&acting, invocation_id))
                    .decision(AuditDecision::Denied)
                    .outcome(AuditRecordOutcome::NotRun)
                    .ext(
                        "sessionSubject",
                        serde_json::to_value(config.acting.session_subject(user_id))
                            .unwrap_or_default(),
                    )
                    .ext("sessionId", serde_json::Value::String(session_id.to_string()));
                if let Err(audit_error) = trail.record(draft).await {
                    tracing::warn!(error = %audit_error, "could not record the refused run");
                }
            }
            tracing::warn!(error = %error, "run refused: its subject is not the session's user");
            return Err(error);
        }
        Ok(())
    }
}

/// The names of every agent below `agent`.
fn collect_descendants(agent: &Arc<dyn Agent>, names: &mut BTreeSet<String>) {
    for child in agent.sub_agents() {
        if names.insert(child.name().to_string()) {
            collect_descendants(child, names);
        }
    }
}
