//! The agent factory of `adk-rust up` (spec 3.3 §5.7, §10 L1): the kernel builds every agent
//! a person added through it, with the composition the primary agent uses.
//!
//! | Item | What it does |
//! |---|---|
//! | [`HarnessAgentFactory`] | `adk_kernel::agents::AgentFactory` over up's harness composition: assesses a definition, reports the primary agent's limits as a suggester's ceiling, lists the models whose key resolves, builds and hosts a harness, revokes an agent's pinned rules when it is removed |
//! | [`AgentModeStore`] | Caps a session's mode at the agent's own: an added agent never asks less than its definition says |
//! | [`model_choices`] | The providers whose key resolves, each with its default model |

use std::sync::Arc;

use adk_core::{AgentId, AgentPrincipal};
use adk_harness::{Harness, HarnessMode, ModeRecord, ModeStore, StandingRuleStore};
use adk_kernel::agents::{
    AgentAssessment, AgentCeiling, AgentChoices, AgentFactory, AgentRecord, HostedAgent,
    ModelChoice,
};
use adk_kernel::{KernelError, PartitionMode};
use async_trait::async_trait;
use futures::future::BoxFuture;

/// Builds the harness of one added agent: up's composition, lowered from the record.
pub(crate) type BuildAgent =
    Arc<dyn Fn(AgentRecord) -> BoxFuture<'static, anyhow::Result<Arc<Harness>>> + Send + Sync>;

fn kernel_error(error: impl std::fmt::Display) -> KernelError {
    KernelError::Config { reason: error.to_string() }
}

/// The providers whose key resolves in this process's environment, each with the model an
/// added agent runs on by default there, the primary agent's model first (spec 3.3 §5.7,
/// decision 21). Keys are read to see that they exist, never kept.
pub(crate) fn model_choices(
    primary: Option<ModelChoice>,
    has_key: impl Fn(&str) -> bool,
) -> Vec<ModelChoice> {
    let mut models: Vec<ModelChoice> = primary.into_iter().collect();
    for (provider, keys, model) in [
        ("anthropic", &["ANTHROPIC_API_KEY"][..], "claude-sonnet-5-5"),
        ("openai", &["OPENAI_API_KEY"][..], "gpt-5.6-terra"),
        ("gemini", &["GOOGLE_API_KEY", "GEMINI_API_KEY"][..], "gemini-3.7-flash"),
    ] {
        if keys.iter().any(|key| has_key(key))
            && !models.iter().any(|known| known.provider == provider && known.model == model)
        {
            models.push(ModelChoice::new(provider, model));
        }
    }
    models
}

/// `adk-rust up`'s agent factory (spec 3.3 §10 L1).
pub(crate) struct HarnessAgentFactory {
    app: String,
    user: String,
    primary: Arc<Harness>,
    primary_name: String,
    choices: AgentChoices,
    rules: Arc<dyn StandingRuleStore>,
    build: BuildAgent,
    reserved: Vec<String>,
}

impl HarnessAgentFactory {
    /// The factory of `app`'s kernel for `user`, whose primary agent is `primary`, shown as
    /// `primary_name`; `build` composes an added agent, `rules` is the standing-rule store a
    /// removed agent's pinned rules are revoked from.
    pub(crate) fn new(
        app: impl Into<String>,
        user: impl Into<String>,
        primary: Arc<Harness>,
        primary_name: impl Into<String>,
        choices: AgentChoices,
        rules: Arc<dyn StandingRuleStore>,
        build: BuildAgent,
    ) -> Self {
        let reserved = primary.agent().info().members.clone();
        Self {
            app: app.into(),
            user: user.into(),
            primary,
            primary_name: primary_name.into(),
            choices,
            rules,
            build,
            reserved,
        }
    }
}

#[async_trait]
impl AgentFactory for HarnessAgentFactory {
    fn assess(&self, record: &AgentRecord) -> AgentAssessment {
        adk_harness::assess_agent(record)
    }

    async fn ceiling(
        &self,
        agent: &AgentId,
        session: &str,
    ) -> adk_kernel::Result<Option<AgentCeiling>> {
        // Only the primary agent suggests agents; an added agent has no propose_agent.
        let primary =
            AgentPrincipal::derived(&self.app, self.primary.agent().info().name.as_str()).id;
        let app_target = AgentPrincipal::derived(&self.app, &self.app).id;
        if *agent != primary && *agent != app_target {
            return Ok(None);
        }
        let ceiling =
            self.primary.agent_ceiling(&self.user, session).await.map_err(kernel_error)?;
        Ok(Some(ceiling))
    }

    async fn choices(&self) -> adk_kernel::Result<AgentChoices> {
        Ok(self.choices.clone())
    }

    async fn build(&self, record: &AgentRecord) -> adk_kernel::Result<HostedAgent> {
        let harness = (self.build)(record.clone()).await.map_err(kernel_error)?;
        Ok(harness.hosted_agent())
    }

    async fn retired(&self, record: &AgentRecord) -> adk_kernel::Result<u64> {
        let pinned = AgentPrincipal::derived(&self.app, &record.id).id.as_str().to_string();
        let rules = self.rules.list(&self.app, &self.user).await.map_err(kernel_error)?;
        let mut revoked = 0;
        for rule in rules.iter().filter(|rule| rule.agent.as_deref() == Some(pinned.as_str())) {
            if self.rules.revoke(&self.app, &rule.id).await.map_err(kernel_error)? {
                revoked += 1;
            }
        }
        Ok(revoked)
    }

    fn reserved_ids(&self) -> Vec<String> {
        self.reserved.clone()
    }

    fn reserved_names(&self) -> Vec<String> {
        vec![self.primary_name.clone()]
    }
}

/// An added agent's sessions never ask less than its definition says (spec 3.3 §5.7 "How much
/// it asks first"): every mode read is the stricter of the session's and the agent's.
#[derive(Clone)]
pub(crate) struct AgentModeStore {
    inner: Arc<dyn ModeStore>,
    cap: HarnessMode,
}

impl AgentModeStore {
    /// Modes of `inner`, capped at `cap`.
    pub(crate) fn new(inner: Arc<dyn ModeStore>, cap: PartitionMode) -> Self {
        Self { inner, cap: cap.into() }
    }
}

#[async_trait]
impl ModeStore for AgentModeStore {
    async fn get(
        &self,
        app: &str,
        user: &str,
        session: &str,
    ) -> adk_core::Result<Option<ModeRecord>> {
        Ok(self.inner.get(app, user, session).await?.map(|mut record| {
            if self.cap.is_stricter_than(record.mode) {
                record.mode = self.cap;
            }
            record
        }))
    }

    async fn set(
        &self,
        app: &str,
        user: &str,
        session: &str,
        record: ModeRecord,
    ) -> adk_core::Result<()> {
        self.inner.set(app, user, session, record).await
    }
}

/// The agent a person added that a harness is built for, beside the kernel's primary agent
/// (spec 3.3 §5.7, §10 L1): `Setup::agent_with` lowers it last, after the area.
pub(crate) struct Added {
    /// The record.
    pub(crate) record: AgentRecord,
    /// The primary agent, whose approval service and agenda the added agent shares.
    pub(crate) beside: Arc<Harness>,
    /// The model it runs on, when its definition names one; else the primary agent's.
    pub(crate) model: Option<crate::BuiltModel>,
    /// The session modes it reads, capped at its own.
    pub(crate) modes: Arc<dyn ModeStore>,
}

impl Added {
    /// `builder` as the added agent's harness.
    pub(crate) fn lower(
        &self,
        builder: adk_harness::HarnessAgentBuilder,
    ) -> adk_harness::HarnessAgentBuilder {
        builder
            .hosted_beside(&self.beside)
            .mode_store(Arc::new(AgentModeStore::new(self.modes.clone(), self.record.mode)))
            .added_agent(&self.record)
    }
}

/// The packs a record names, as the harness knows them; a name it does not know is left out
/// (the kernel refused it when the record was written).
pub(crate) fn packs_of(record: &AgentRecord) -> Vec<adk_harness::Pack> {
    record
        .uses
        .packs
        .iter()
        .filter_map(|pack| match pack.as_str() {
            "browser" => Some(adk_harness::Pack::Browser),
            "computer-use" | "computer_use" => Some(adk_harness::Pack::ComputerUse),
            "payments" => Some(adk_harness::Pack::Payments),
            "voice" => Some(adk_harness::Pack::Voice),
            _ => None,
        })
        .collect()
}

/// A pack's name, as definitions name it.
pub(crate) fn pack_name(pack: adk_harness::Pack) -> Option<&'static str> {
    match pack {
        adk_harness::Pack::Browser => Some("browser"),
        adk_harness::Pack::ComputerUse => Some("computer-use"),
        adk_harness::Pack::Payments => Some("payments"),
        adk_harness::Pack::Voice => Some("voice"),
        _ => None,
    }
}

/// The environment variables a provider's key is read from.
fn key_names(provider: &str) -> &'static [&'static str] {
    match provider {
        "anthropic" => &["ANTHROPIC_API_KEY"],
        "openai" => &["OPENAI_API_KEY"],
        "gemini" | "google" => &["GOOGLE_API_KEY", "GEMINI_API_KEY"],
        _ => &[],
    }
}

/// Whether the key of `provider` resolves in this process's environment.
pub(crate) fn has_key(provider: &str) -> bool {
    key_names(provider)
        .iter()
        .any(|name| std::env::var(name).is_ok_and(|value| !value.trim().is_empty()))
}

/// The model `record` runs on: `None` for the primary agent's, and always under
/// `--model-script`, whose scripted model drives every agent (spec 3.3 §10 L1).
///
/// # Errors
///
/// "No key for Anthropic" when the provider's key does not resolve; the kernel then shows the
/// agent as Can't start.
pub(crate) async fn model_of(
    record: &AgentRecord,
    scripted: bool,
) -> anyhow::Result<Option<crate::BuiltModel>> {
    let Some(choice) = record.model.as_ref().filter(|_| !scripted) else {
        return Ok(None);
    };
    let (provider, label) = match choice.provider.as_str() {
        "anthropic" => (adk_model::ModelProvider::Anthropic, "Anthropic"),
        "openai" => (adk_model::ModelProvider::Openai, "OpenAI"),
        "gemini" | "google" => (adk_model::ModelProvider::Gemini, "Gemini"),
        other => anyhow::bail!("No provider called {other}"),
    };
    let key = key_names(&choice.provider)
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.trim().is_empty()))
        .ok_or_else(|| anyhow::anyhow!("No key for {label}"))?;
    let built =
        crate::create_model(crate::cli::Provider::Model(provider), &choice.model, Some(&key), None)
            .await?;
    Ok(Some(built))
}

/// The session modes `setup` keeps: `harness.db`'s, the shared store's, or in memory.
pub(super) fn modes_of(setup: &super::Setup) -> Arc<dyn ModeStore> {
    if let Some(shared) = &setup.shared {
        return shared.modes.clone();
    }
    #[cfg(feature = "persist")]
    if let Some(db) = &setup.db {
        return db.modes.clone();
    }
    Arc::new(adk_harness::InMemoryModeStore::new())
}

/// The session `adk-rust agents` runs `/agent` in: the command keeps nothing of it.
const CLI_SESSION: &str = "adk-rust-agents";

/// `adk-rust agents` (spec 3.3 §5.7, §10 L1): `list` and `show` over the kernel's read routes;
/// every other verb runs `/agent` on the kernel through its command routes, prints what the
/// change does, and asks on the terminal before it applies. `--yes` applies without asking,
/// except at high risk; with no terminal to ask on, a change is refused.
///
/// # Errors
///
/// The kernel cannot be reached, the change was refused, or nobody confirmed it.
pub(crate) async fn command(
    kernel: &crate::cli::KernelTarget,
    args: &[String],
    json: bool,
    yes: bool,
) -> anyhow::Result<()> {
    use std::io::{IsTerminal as _, Write as _};

    let client = crate::kernel_cli::client(kernel).await?;
    let verb = args.first().map_or("list", String::as_str);
    match verb {
        "list" | "ls" => {
            let views = client.agents().await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&views)?);
                return Ok(());
            }
            if views.is_empty() {
                println!(
                    "No agents of your own yet. Add one with `adk-rust agents new --name NAME`, or ask ADK-Rust to suggest one."
                );
            }
            for view in &views {
                let state = if view.cant_start.is_some() {
                    "Can't start"
                } else {
                    view.agent.state.as_str()
                };
                println!(
                    "  {:<24} {:<12} {:<10} {}",
                    adk_surface_line(&view.agent.display_name),
                    state,
                    view.agent.area,
                    view.agent.mode
                );
            }
            Ok(())
        }
        "show" => {
            let id = args
                .get(1)
                .ok_or_else(|| anyhow::anyhow!("name the agent: adk-rust agents show finance"))?;
            let line = format!("show {}", quote(id));
            let text = client.run_command("agent", &line, CLI_SESSION).await?;
            if json {
                let view = client.agent(id).await?;
                println!("{}", serde_json::to_string_pretty(&view)?);
            } else {
                println!("{text}");
            }
            Ok(())
        }
        _ => {
            let line = args.iter().map(|arg| quote(arg)).collect::<Vec<_>>().join(" ");
            let answer =
                client.run_command("agent", &format!("{line} --json"), CLI_SESSION).await?;
            let preview: serde_json::Value =
                serde_json::from_str(&answer).map_err(|_| anyhow::anyhow!("{answer}"))?;
            let code = preview["code"].as_str().unwrap_or_default().to_string();
            let high = matches!(preview["preview"]["risk"].as_str(), Some("high" | "critical"))
                || preview["preview"]["money"].as_bool() == Some(true);
            if json {
                println!("{}", serde_json::to_string_pretty(&preview)?);
            } else {
                print_preview(&preview);
            }
            if yes && high {
                anyhow::bail!(
                    "--yes is refused at high risk: confirm this change on the terminal, or in the \
                     web home after confirming it's you"
                );
            }
            if !yes {
                if !std::io::stdin().is_terminal() {
                    anyhow::bail!(
                        "no terminal to ask on: run it on a terminal to confirm, or pass --yes for a \
                         change that is not high risk"
                    );
                }
                print!("Confirm? [y/N] ");
                std::io::stdout().flush()?;
                let mut reply = String::new();
                std::io::stdin().read_line(&mut reply)?;
                if !matches!(reply.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                    println!("Nothing changed.");
                    return Ok(());
                }
            }
            let done = client
                .run_command(
                    "agent",
                    &format!("{line} --confirm {code}{}", if json { " --json" } else { "" }),
                    CLI_SESSION,
                )
                .await?;
            println!("{done}");
            Ok(())
        }
    }
}

/// `value` as one `/agent` word.
fn quote(value: &str) -> String {
    if !value.is_empty() && !value.contains(|c: char| c.is_whitespace() || c == '"' || c == '\'') {
        return value.to_string();
    }
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Text from the kernel on one line, every control character escaped (S-I9).
fn adk_surface_line(text: &str) -> String {
    text.chars()
        .flat_map(|c| {
            if c.is_control() {
                format!("\\u{{{:x}}}", c as u32).chars().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

fn print_preview(answer: &serde_json::Value) {
    let preview = &answer["preview"];
    let name = adk_surface_line(answer["name"].as_str().unwrap_or_default());
    if let Some(ended) = preview.get("ended").filter(|ended| !ended.is_null()) {
        let count = |key: &str| ended[key].as_array().map_or(0, Vec::len);
        println!(
            "Removing {name} stops {} piece(s) of work, ends {} decision(s) waiting for you and {} routine(s). Its threads stay.",
            count("work"),
            count("decisions"),
            count("routines")
        );
        return;
    }
    println!("Here is what {name} can do:");
    for ability in preview["abilities"].as_array().into_iter().flatten() {
        println!("  - {}", adk_surface_line(ability.as_str().unwrap_or_default()));
    }
    let risk = match preview["risk"].as_str() {
        Some("low") => "Low risk",
        Some("medium") => "Medium risk",
        _ => "High risk",
    };
    println!("{risk} · You can remove it later");
}

#[cfg(test)]
mod tests {
    use adk_harness::{InMemoryModeStore, ModeSource};

    use super::*;

    #[test]
    fn choices_list_only_the_providers_with_a_key() {
        let primary = Some(ModelChoice::new("anthropic", "claude-sonnet-5-5"));
        let choices = model_choices(primary.clone(), |key| {
            key == "ANTHROPIC_API_KEY" || key == "GEMINI_API_KEY"
        });
        assert_eq!(
            choices,
            [
                ModelChoice::new("anthropic", "claude-sonnet-5-5"),
                ModelChoice::new("gemini", "gemini-3.7-flash")
            ]
        );
        assert!(model_choices(None, |_| false).is_empty());
    }

    #[tokio::test]
    async fn the_mode_store_caps_the_agents_sessions() {
        let inner: Arc<dyn ModeStore> = Arc::new(InMemoryModeStore::new());
        let capped = AgentModeStore::new(inner.clone(), PartitionMode::Plan);
        capped
            .set("app", "ana", "s1", ModeRecord::new(HarnessMode::Auto, ModeSource::User))
            .await
            .unwrap();
        assert_eq!(capped.get("app", "ana", "s1").await.unwrap().unwrap().mode, HarnessMode::Plan);
        capped
            .set("app", "ana", "s2", ModeRecord::new(HarnessMode::ReadOnly, ModeSource::User))
            .await
            .unwrap();
        assert_eq!(
            capped.get("app", "ana", "s2").await.unwrap().unwrap().mode,
            HarnessMode::ReadOnly
        );
        assert!(capped.get("app", "ana", "s3").await.unwrap().is_none());
    }
}
