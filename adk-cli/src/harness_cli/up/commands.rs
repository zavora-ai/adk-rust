//! The REPL's slash commands and "Always allow" for a terminal attached to `adk-rust up`
//! (spec 3.3 §5.2): the kernel's control routes run them over up's harness, by the REPL's own
//! code, and make "Always allow" the standing rule the harness makes in process.

use adk_cli::harness::HarnessCommands;
#[cfg(feature = "surface")]
use adk_core::ApprovalRequest;
#[cfg(feature = "surface")]
use adk_core::principal::SubjectRef;
use adk_core::{AdkError, ErrorCategory, ErrorComponent};
use adk_kernel::KernelError;
use adk_kernel::control::{CommandCall, CommandHost, CommandInfo};

/// Up's command host: its primary harness's commands, and its standing rules.
pub(super) struct HarnessCommandHost {
    commands: HarnessCommands,
    #[cfg(feature = "surface")]
    rules: Option<rules::Rules>,
}

impl HarnessCommandHost {
    /// The commands of `commands`; "Always allow" through `rules` when the web home's engine
    /// is built in (feature `surface`).
    pub(super) fn new(commands: HarnessCommands) -> Self {
        Self {
            commands,
            #[cfg(feature = "surface")]
            rules: None,
        }
    }

    /// "Always allow" as the harness makes it in process: the rule `source` prepares for a
    /// waiting decision.
    #[cfg(feature = "surface")]
    pub(super) fn with_rules(mut self, source: adk_surface::LocalSource) -> Self {
        self.rules = Some(rules::Rules::new(source));
        self
    }
}

fn kernel_error(error: AdkError) -> KernelError {
    KernelError::Core(error)
}

#[async_trait::async_trait]
impl CommandHost for HarnessCommandHost {
    fn commands(&self) -> Vec<CommandInfo> {
        HarnessCommands::list()
            .into_iter()
            .map(|(name, description)| CommandInfo::new(name, description))
            .collect()
    }

    fn changes(&self, name: &str, args: &str) -> bool {
        HarnessCommands::changes(name, args)
    }

    async fn run(&self, call: &CommandCall) -> adk_kernel::Result<String> {
        let reach = Some(call.reach.clone());
        match self.commands.run(&call.name, &call.args, &call.user, &call.session, reach).await {
            Ok(Some(text)) => Ok(text),
            Ok(None) => Err(kernel_error(AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::NotFound,
                "harness.command.not_found",
                format!("no command /{}; /help lists them", call.name),
            ))),
            Err(error) => Err(kernel_error(error)),
        }
    }

    #[cfg(feature = "surface")]
    async fn always_allow_offer(
        &self,
        request: &ApprovalRequest,
    ) -> adk_kernel::Result<Option<String>> {
        match &self.rules {
            Some(rules) => Ok(rules.offer(request).await),
            None => Ok(None),
        }
    }

    #[cfg(feature = "surface")]
    async fn prepare_always_allow(
        &self,
        request: &ApprovalRequest,
        by: &SubjectRef,
    ) -> adk_kernel::Result<String> {
        match &self.rules {
            Some(rules) => rules.prepare(request, by).await.map_err(kernel_error),
            None => Err(no_rules()),
        }
    }

    #[cfg(feature = "surface")]
    async fn grant_always_allow(&self, request: &ApprovalRequest) -> adk_kernel::Result<()> {
        match &self.rules {
            Some(rules) => rules.grant(request).await.map_err(kernel_error),
            None => Err(no_rules()),
        }
    }
}

#[cfg(feature = "surface")]
fn no_rules() -> KernelError {
    kernel_error(AdkError::new(
        ErrorComponent::Server,
        ErrorCategory::InvalidInput,
        "harness.rule.unsupported",
        "this kernel makes no standing rules from a decision: allow it once",
    ))
}

#[cfg(feature = "surface")]
mod rules {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use adk_core::principal::SubjectRef;
    use adk_core::{AdkError, ApprovalRequest, ErrorCategory, ErrorComponent};
    use adk_surface::{Caller, LocalSource, Origin, RuleCommit, Source, SurfaceKind};

    /// How long a prepared rule waits for its decision's yes.
    const PREPARED_FOR: Duration = Duration::from_secs(600);

    /// How many prepared rules wait at once.
    const PREPARED: usize = 64;

    /// The rules "Always allow" makes, by the web home's engine source over up's harness:
    /// prepared while a decision waits, granted once its yes is recorded.
    pub(super) struct Rules {
        source: LocalSource,
        prepared: Mutex<Prepared>,
    }

    /// The rules prepared, by decision, with when.
    type Prepared = HashMap<String, (Instant, Box<dyn RuleCommit>)>;

    impl Rules {
        pub(super) fn new(source: LocalSource) -> Self {
            Self { source, prepared: Mutex::new(HashMap::new()) }
        }

        pub(super) async fn offer(&self, request: &ApprovalRequest) -> Option<String> {
            self.source.always_allow_offer(request).await.map(|offer| offer.label)
        }

        pub(super) async fn prepare(
            &self,
            request: &ApprovalRequest,
            by: &SubjectRef,
        ) -> adk_core::Result<String> {
            let label = self.offer(request).await.ok_or_else(|| {
                AdkError::new(
                    ErrorComponent::Server,
                    ErrorCategory::InvalidInput,
                    "harness.rule.none",
                    "no standing rule applies to this call; allow it once",
                )
            })?;
            // The rule's author is the person the kernel admitted.
            let caller = Caller::human(by.id.clone(), SurfaceKind::Terminal, Origin::Keyboard);
            let commit = self.source.prepare_always_allow(&caller, request).await?;
            let mut prepared = self.prepared.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            prepared.retain(|_, (at, _)| now.saturating_duration_since(*at) < PREPARED_FOR);
            if prepared.len() >= PREPARED {
                prepared.clear();
            }
            prepared.insert(request.id.clone(), (now, commit));
            Ok(label)
        }

        pub(super) async fn grant(&self, request: &ApprovalRequest) -> adk_core::Result<()> {
            let commit = self
                .prepared
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&request.id)
                .filter(|(at, _)| at.elapsed() < PREPARED_FOR)
                .map(|(_, commit)| commit)
                .ok_or_else(|| {
                    AdkError::new(
                        ErrorComponent::Server,
                        ErrorCategory::NotFound,
                        "harness.rule.not_prepared",
                        "no standing rule was prepared for this decision; the call was allowed \
                         once",
                    )
                })?;
            Ok(commit.commit().await?)
        }
    }
}
