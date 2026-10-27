//! Screen actions people and agents share (spec 3.3 §2 item 7, §5.6, §6.1 `verb-ui`;
//! Experimental): one way to change something a surface shows — first, snoozing something in
//! Coming up — from the home, the terminal, voice and an agent, always through the governed
//! path.
//!
//! An action is declared once, as a [`VerbSpec`], with a [`VerbHandler`] that runs it, in a
//! [`VerbRegistry`] over the kernel's agenda. Every caller reaches the same handler:
//!
//! | Caller | How | Governed as | Waits for a person |
//! |---|---|---|---|
//! | A person on the home, the terminal or by voice | [`VerbGate::call`] with the server-authenticated [`Caller`](crate::Caller) | `urn:adk:agent:<app>/surface.verbs`, acting for the person, through `authorize_tool_call` | No: the person is the one asking |
//! | An agent | Its `verb_<name>` tool from [`VerbToolset`], published as `verbs:<app>` | The agent itself, with [`VerbPolicy`] in the operator layer | Yes, every call that changes something ([`VerbPolicy`]) |
//!
//! What no action can do (spec 3.3 §7.4, S-I4, S-I10, S-I11): a handler gets only a
//! [`VerbScope`] — the caller's own agenda items and the one change of moving a wake later —
//! so it can never answer a decision, change an agent, a standing rule, a mode or Stop all, or
//! resume work. [`VerbRegistry`] refuses the names of those actions ([`RESERVED`]) and every
//! object kind but [`ObjectKind::ComingUp`].
//!
//! | Piece | What it does |
//! |---|---|
//! | [`VerbRegistry`] | The catalogue — `coming_up` and `snooze` — and any action a host declares |
//! | [`VerbGate`] | A person's call: the §7.2 row check, ownership (S-I8), the governed path, the `surface.verb` record |
//! | [`VerbToolset`], [`VerbClasses`] | An agent's tools, `verb_<name>`, classed by their declared effect |
//! | [`VerbPolicy`] | Holds every agent call that changes something, whatever the mode or standing rules (spec 3.2 K-I9 pattern); at most [`MAX_WAITING`] waiting per agent |
//! | [`VerbRouter`] | The card of an agent's call: a kernel-text summary, its risk, Allow once · Edit · Deny |
//! | [`VerbHints`] | The card's chat heading, "You can change it back", no **Always allow**, what the agent read first |
//! | [`spoken`] | "Snooze news digest" by voice, after the engine's cancel window |
//!
//! Chat channels and the tray cannot take a screen action (spec 3.3 §7.2): "/snooze" typed in
//! Telegram reaches the agent as text, and text an agent writes is never a command.
//!
//! # Example
//!
//! ```rust,no_run
//! use std::sync::Arc;
//! use adk_surface::verbs::{KernelAgenda, VerbRegistry};
//!
//! # fn example(kernel: &adk_kernel::Kernel) -> Result<(), adk_surface::verbs::VerbError> {
//! let registry = VerbRegistry::builder("adk-rust", Arc::new(KernelAgenda::of(kernel))).build()?;
//! assert!(registry.spec("snooze").is_some());
//! assert_eq!(registry.origin(), "verbs:adk-rust");
//! # Ok(())
//! # }
//! ```

mod error;
mod gate;
mod policy;
mod registry;
mod snooze;
pub mod spoken;
mod terminal;
mod tools;
pub mod words;

use adk_core::AgentPrincipal;
use adk_kernel::drivers::EffectClass;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub use error::VerbError;
pub use gate::VerbGate;
pub use policy::{VERB_POLICY_REF, VerbHints, VerbPolicy, VerbRouter};
pub use registry::{
    AgendaAccess, KernelAgenda, VerbHandler, VerbRegistry, VerbRegistryBuilder, VerbScope,
};
pub use snooze::{ComingUpItem, SnoozeChoice, find_by_title};
#[cfg(feature = "tui")]
pub use terminal::SnoozeCommand;
pub use terminal::{SnoozeText, parse_snooze, problem_line, snooze_text};
pub use tools::{VerbClasses, VerbToolset};

/// The agent name a person's own screen actions run under: `urn:adk:agent:<app>/surface.verbs`
/// acting for the person (spec 3.3 §2 item 7, owner question 23).
pub const VERBS_AGENT: &str = "surface.verbs";

/// The audit action of a screen action taken (spec 3.3 §11).
pub const SURFACE_AUDIT_VERB: &str = "surface.verb";

/// The tool-name prefix of an action an agent calls: `verb_snooze`.
pub const VERB_TOOL_PREFIX: &str = "verb_";

/// The most actions one agent may have waiting for a person at once (owner question 22).
pub const MAX_WAITING: usize = 3;

/// The id a host's grant of screen actions carries on an agent's detail (spec 3.3 §5.7).
pub const GRANT_ID: &str = "verb-ui";

/// Names no screen action may take: answering a decision, changing an agent, governance and
/// resuming work stay with the surfaces' own controls (spec 3.3 §7.4, S-I4, S-I10, S-I11).
pub const RESERVED: &[&str] = &[
    "approve",
    "reject",
    "allow",
    "deny",
    "decide",
    "answer",
    "always_allow",
    "add_agent",
    "change_agent",
    "remove_agent",
    "resume",
    "resume_all",
    "unfreeze",
    "mount",
];

/// The publisher of every action tool of `app`: `verbs:<app>`, the [`ToolOrigin`](adk_core::ToolOrigin)
/// source a card, a policy and a router tell them by (spec 3.3 §7.1).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_surface::verbs::verbs_origin("adk-rust"), "verbs:adk-rust");
/// ```
pub fn verbs_origin(app: &str) -> String {
    format!("verbs:{app}")
}

/// The principal a person's screen actions run under: `urn:adk:agent:<app>/surface.verbs`.
///
/// # Example
///
/// ```rust
/// let principal = adk_surface::verbs::verbs_principal("adk-rust");
/// assert_eq!(principal.id.as_str(), "urn:adk:agent:adk-rust/surface.verbs");
/// ```
pub fn verbs_principal(app: &str) -> AgentPrincipal {
    AgentPrincipal::derived(app, VERBS_AGENT)
}

/// What a screen action acts on (spec 3.3 §2 item 7). This wave allows only Coming up.
///
/// # Example
///
/// ```rust
/// use adk_surface::verbs::ObjectKind;
///
/// assert_eq!(serde_json::to_value(ObjectKind::ComingUp).unwrap(), "comingUp");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ObjectKind {
    /// Something in Coming up: an agenda item scheduled to run (spec 3.3 §5.1).
    ComingUp,
}

/// The object kinds a [`VerbSpec`] may name this wave.
pub const ALLOWED_OBJECTS: &[ObjectKind] = &[ObjectKind::ComingUp];

/// One thing an action acts on (spec 3.3 §2 item 7).
///
/// # Example
///
/// ```rust
/// use adk_surface::verbs::{ObjectKind, ObjectRef};
///
/// let item = ObjectRef::coming_up("agenda-7f3");
/// assert_eq!(serde_json::to_value(&item).unwrap(), serde_json::json!({ "kind": "comingUp", "id": "agenda-7f3" }));
/// assert_eq!(item.kind, ObjectKind::ComingUp);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ObjectRef {
    /// What kind of thing.
    pub kind: ObjectKind,
    /// Its id: the agenda item's id for Coming up.
    pub id: String,
}

impl ObjectRef {
    /// The Coming up item `id`.
    pub fn coming_up(id: impl Into<String>) -> Self {
        Self { kind: ObjectKind::ComingUp, id: id.into() }
    }
}

/// Who may take an action (spec 3.3 §2 item 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Callers {
    /// Only a person, through a surface: an agent's call is refused.
    PersonOnly,
    /// A person, and an agent through its tool, every changing call waiting for a person.
    PersonAndAgents,
}

/// One screen action, declared once for every caller (spec 3.3 §2 item 7, §6.1).
///
/// # Example
///
/// ```rust
/// use adk_kernel::drivers::EffectClass;
/// use adk_surface::verbs::{Callers, ObjectKind, VerbSpec};
///
/// let snooze = VerbSpec::new("snooze", "Snooze", "Snooze something in Coming up", EffectClass::WriteLocal)
///     .with_objects([ObjectKind::ComingUp])
///     .with_callers(Callers::PersonAndAgents)
///     .only_delays()
///     .reversible()
///     .with_voice_phrase("snooze");
/// assert_eq!(snooze.tool_name(), "verb_snooze");
/// assert!(snooze.only_delays);
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct VerbSpec {
    /// The name: `^[a-z][a-z0-9_]{0,58}$`, never one of [`RESERVED`].
    pub name: String,
    /// The button's words: "Snooze".
    pub label: String,
    /// What it does, in plain words, for a person and for the model: "Snooze something in
    /// Coming up". Also the chat heading of an agent's card.
    pub description: String,
    /// What it acts on; an allowlist, of [`ALLOWED_OBJECTS`] only.
    pub objects: Vec<ObjectKind>,
    /// The JSON Schema of its arguments, the tool's parameters.
    pub args: Value,
    /// What it changes, as a driver tool's manifest says it.
    pub effect: EffectClass,
    /// Who may take it.
    pub callers: Callers,
    /// It only delays work: a person may take it while everything is stopped (owner question
    /// 23).
    pub only_delays: bool,
    /// It can be changed back: its card says "You can change it back".
    pub reversible: bool,
    /// The words that start it by voice, such as "snooze"; `None`: not by voice.
    pub voice_phrase: Option<String>,
}

impl VerbSpec {
    /// An action `name` with no objects and no arguments, for a person only.
    pub fn new(
        name: impl Into<String>,
        label: impl Into<String>,
        description: impl Into<String>,
        effect: EffectClass,
    ) -> Self {
        Self {
            name: name.into(),
            label: label.into(),
            description: description.into(),
            objects: Vec::new(),
            args: json!({ "type": "object", "properties": {} }),
            effect,
            callers: Callers::PersonOnly,
            only_delays: false,
            reversible: false,
            voice_phrase: None,
        }
    }

    /// Acts on `objects`.
    #[must_use]
    pub fn with_objects(mut self, objects: impl IntoIterator<Item = ObjectKind>) -> Self {
        self.objects = objects.into_iter().collect();
        self
    }

    /// Takes arguments of the JSON Schema `schema`.
    #[must_use]
    pub fn with_args(mut self, schema: Value) -> Self {
        self.args = schema;
        self
    }

    /// May be taken by `callers`.
    #[must_use]
    pub fn with_callers(mut self, callers: Callers) -> Self {
        self.callers = callers;
        self
    }

    /// Only delays work, so it still runs while everything is stopped.
    #[must_use]
    pub fn only_delays(mut self) -> Self {
        self.only_delays = true;
        self
    }

    /// Can be changed back.
    #[must_use]
    pub fn reversible(mut self) -> Self {
        self.reversible = true;
        self
    }

    /// Starts by voice with `phrase`.
    #[must_use]
    pub fn with_voice_phrase(mut self, phrase: impl Into<String>) -> Self {
        self.voice_phrase = Some(phrase.into());
        self
    }

    /// The tool an agent calls it by: `verb_<name>`.
    pub fn tool_name(&self) -> String {
        format!("{VERB_TOOL_PREFIX}{}", self.name)
    }

    /// Whether it only reads.
    pub fn reads(&self) -> bool {
        self.effect == EffectClass::Read
    }
}

/// A request to take an action (spec 3.3 §2 item 7): the shape of agentrix's `VerbIntent`,
/// re-declared. It carries no origin and no surface: those come from the server-authenticated
/// [`Caller`](crate::Caller).
///
/// The arguments are the action's own; an object the action acts on fills the argument
/// `item` when the arguments leave it out.
///
/// # Example
///
/// ```rust
/// use adk_surface::verbs::{ObjectRef, VerbIntent};
///
/// let intent: VerbIntent = serde_json::from_value(serde_json::json!({
///     "verb": "snooze",
///     "objects": [{ "kind": "comingUp", "id": "agenda-7f3" }],
///     "args": { "to": "2026-10-26T19:00:00+03:00" },
/// }))
/// .unwrap();
/// assert_eq!(intent.objects, vec![ObjectRef::coming_up("agenda-7f3")]);
/// assert_eq!(intent.call_args()["item"], "agenda-7f3");
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VerbIntent {
    /// The action's name.
    pub verb: String,
    /// What it acts on.
    #[serde(default)]
    pub objects: Vec<ObjectRef>,
    /// The record's target, `agenda:<id>`; filled from `objects` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Its arguments.
    #[serde(default)]
    pub args: Value,
}

impl VerbIntent {
    /// An intent to take `verb` with `args`.
    pub fn new(verb: impl Into<String>, args: Value) -> Self {
        Self { verb: verb.into(), objects: Vec::new(), target: None, args }
    }

    /// Acts on `object` too.
    #[must_use]
    pub fn on(mut self, object: ObjectRef) -> Self {
        self.objects.push(object);
        self
    }

    /// The arguments the handler runs with: `args`, with `item` from the first object when
    /// `args` names none.
    pub fn call_args(&self) -> Value {
        let mut args = match &self.args {
            Value::Object(map) => map.clone(),
            _ => serde_json::Map::new(),
        };
        if !args.contains_key("item")
            && let Some(object) = self.objects.first()
        {
            args.insert("item".to_string(), json!(object.id));
        }
        Value::Object(args)
    }
}

/// One action of the catalogue, as `GET /surface/verbs` lists it (spec 3.3 §6.1).
///
/// # Example
///
/// ```rust
/// use adk_surface::verbs::VerbView;
///
/// let view: VerbView = serde_json::from_value(serde_json::json!({
///     "name": "snooze", "label": "Snooze", "description": "Snooze something in Coming up",
///     "objects": ["comingUp"], "effect": "write_local", "agentsAsk": true,
///     "onlyDelays": true, "reversible": true,
/// }))
/// .unwrap();
/// assert!(view.agents_ask);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VerbView {
    /// The name.
    pub name: String,
    /// The button's words.
    pub label: String,
    /// What it does.
    pub description: String,
    /// What it acts on.
    pub objects: Vec<ObjectKind>,
    /// What it changes: `read`, `write_local`, …
    pub effect: String,
    /// Agents may ask for it; it always waits for a person when it changes something.
    pub agents_ask: bool,
    /// It only delays work, so it still runs while everything is stopped.
    pub only_delays: bool,
    /// It can be changed back.
    pub reversible: bool,
}

impl From<&VerbSpec> for VerbView {
    fn from(spec: &VerbSpec) -> Self {
        Self {
            name: spec.name.clone(),
            label: spec.label.clone(),
            description: spec.description.clone(),
            objects: spec.objects.clone(),
            effect: spec.effect.to_string(),
            agents_ask: spec.callers == Callers::PersonAndAgents,
            only_delays: spec.only_delays,
            reversible: spec.reversible,
        }
    }
}

/// What an action did (spec 3.3 §2 item 7): shown once it ran, never before.
///
/// # Example
///
/// ```rust
/// use adk_surface::verbs::VerbDone;
///
/// let done = VerbDone::new("snooze", "Moved News digest to 19:00")
///     .with_target("agenda:agenda-7f3", "News digest");
/// assert_eq!(done.target.as_deref(), Some("agenda:agenda-7f3"));
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VerbDone {
    /// The action.
    pub verb: String,
    /// What it acted on: `agenda:<id>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// What it acted on, in plain words: the item's title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// What happened, in plain words: "Moved News digest to 19:00".
    pub text: String,
    /// What a read returned, or the changed thing; `null` when nothing more.
    #[serde(default)]
    pub result: Value,
}

impl VerbDone {
    /// `verb` did `text`.
    pub fn new(verb: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            verb: verb.into(),
            target: None,
            title: None,
            text: text.into(),
            result: Value::Null,
        }
    }

    /// It acted on `target`, titled `title`.
    #[must_use]
    pub fn with_target(mut self, target: impl Into<String>, title: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self.title = Some(title.into());
        self
    }

    /// It returned `result`.
    #[must_use]
    pub fn with_result(mut self, result: Value) -> Self {
        self.result = result;
        self
    }
}

/// Whether `name` may name a screen action: `^[a-z][a-z0-9_]{0,58}$` and none of
/// [`RESERVED`].
///
/// # Errors
///
/// [`VerbError::Invalid`] for a name outside the pattern, [`VerbError::Reserved`] for a
/// reserved one.
///
/// # Example
///
/// ```rust
/// use adk_surface::verbs::check_name;
///
/// assert!(check_name("snooze").is_ok());
/// assert_eq!(check_name("approve").unwrap_err().code(), "surface.verb.reserved");
/// assert_eq!(check_name("Snooze").unwrap_err().code(), "surface.verb.invalid");
/// ```
pub fn check_name(name: &str) -> Result<(), VerbError> {
    let mut chars = name.chars();
    let shaped = name.len() <= 59
        && chars.next().is_some_and(|first| first.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !shaped {
        return Err(VerbError::invalid(format!(
            "`{name}` is not a screen action's name: use 1 to 59 lower-case letters, digits and \
             underscores, starting with a letter"
        )));
    }
    if RESERVED.contains(&name) {
        return Err(VerbError::Reserved { name: name.to_string() });
    }
    Ok(())
}
