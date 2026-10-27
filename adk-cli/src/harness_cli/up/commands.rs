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
    /// `/snooze`, as the person at the attached terminal (spec 3.3 §5.2, §2 item 7).
    #[cfg(feature = "verb-ui")]
    verbs: Option<adk_surface::verbs::VerbGate>,
}

/// The screen action the attached terminal takes as a command (spec 3.3 §5.2).
#[cfg(feature = "verb-ui")]
const SNOOZE: &str = "snooze";

impl HarnessCommandHost {
    /// The commands of `commands`; "Always allow" through `rules` when the web home's engine
    /// is built in (feature `surface`).
    pub(super) fn new(commands: HarnessCommands) -> Self {
        Self {
            commands,
            #[cfg(feature = "surface")]
            rules: None,
            #[cfg(feature = "verb-ui")]
            verbs: None,
        }
    }

    /// `/snooze <title> [tomorrow]` through `gate` (spec 3.3 §5.2, owner question 27): the
    /// terminal attached to this kernel snoozes as the person it admitted, typing, in up's
    /// process.
    #[cfg(feature = "verb-ui")]
    pub(super) fn with_verbs(mut self, gate: adk_surface::verbs::VerbGate) -> Self {
        self.verbs = Some(gate);
        self
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
        let listed = HarnessCommands::list()
            .into_iter()
            .map(|(name, description)| CommandInfo::new(name, description));
        #[cfg(feature = "verb-ui")]
        let listed = listed.chain(
            self.verbs
                .is_some()
                .then(|| CommandInfo::new(SNOOZE, adk_surface::verbs::words::SNOOZE_SOMETHING)),
        );
        listed.collect()
    }

    fn changes(&self, name: &str, args: &str) -> bool {
        #[cfg(feature = "verb-ui")]
        if name == SNOOZE && self.verbs.is_some() {
            return true;
        }
        HarnessCommands::changes(name, args)
    }

    async fn tightens(&self, call: &CommandCall) -> adk_kernel::Result<bool> {
        // Snoozing only delays work, so it runs while everything is stopped (owner question
        // 23).
        #[cfg(feature = "verb-ui")]
        if call.name == SNOOZE && self.verbs.is_some() {
            return Ok(true);
        }
        self.commands
            .tightens(&call.name, &call.args, &call.user, &call.session)
            .await
            .map_err(kernel_error)
    }

    async fn session_mode(&self, user: &str, session: &str) -> adk_kernel::Result<Option<String>> {
        self.commands.session_mode(user, session).await.map(Some).map_err(kernel_error)
    }

    async fn run(&self, call: &CommandCall) -> adk_kernel::Result<String> {
        #[cfg(feature = "verb-ui")]
        if call.name == SNOOZE
            && let Some(gate) = &self.verbs
        {
            return snooze(gate, call).await;
        }
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
        // A screen action or a workflow's governed node asks every time: no standing rule is
        // offered for it (spec 3.3 §7.2, owner question 22).
        if request.origin.as_ref().is_some_and(|origin| {
            origin.source.starts_with("verbs:") || origin.source.starts_with("graph:")
        }) {
            return Ok(None);
        }
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

/// `/snooze` for the person the kernel admitted, on the terminal: typed, and on the kernel's
/// own machine when it came over its socket or pipe (spec 3.3 §5.2). A refusal is the
/// command's error, in the terminal's words.
#[cfg(feature = "verb-ui")]
async fn snooze(
    gate: &adk_surface::verbs::VerbGate,
    call: &CommandCall,
) -> adk_kernel::Result<String> {
    use adk_surface::{Caller, Origin, SurfaceKind};
    let mut caller = Caller::human(call.user.clone(), SurfaceKind::Terminal, Origin::Keyboard)
        .with_groups([adk_cli::harness::OPERATOR_SCOPE]);
    if call.reach.transport != adk_kernel::control::ControlTransport::Http {
        caller = caller.with_paired_local();
    }
    if let Some(device) = &call.reach.device {
        caller = caller.with_device(device.clone());
    }
    adk_surface::verbs::snooze_text(gate, &caller, &call.args).await.map_err(|error| {
        let line = adk_surface::verbs::problem_line(&error);
        let mut error = AdkError::from(error);
        error.message = line;
        kernel_error(error)
    })
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
