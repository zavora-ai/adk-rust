//! The `[ultracode]` limits (spec 3.1 §7.10, §8.1): defaults, the user config, the project
//! file (tighten-only) and the flags, which win.

use std::collections::BTreeMap;
use std::path::Path;

use adk_core::{AdkError, Result};
use serde::{Deserialize, Serialize};

use super::route::{ArchitectureChoice, RouterMode};
use crate::harness::PROJECT_CONFIG_FILE;
use crate::harness::args::loosening_note;

/// One numeric `[ultracode]` key: its default, ceiling and floor.
struct Bound {
    key: &'static str,
    default: u64,
    ceiling: u64,
    floor: u64,
}

const MAX_ROUNDS: Bound = Bound { key: "max_rounds", default: 2, ceiling: 5, floor: 1 };
const MAX_REVIEWERS: Bound = Bound { key: "max_reviewers", default: 4, ceiling: 6, floor: 3 };
const MAX_BOARD_ROUNDS: Bound = Bound { key: "max_board_rounds", default: 2, ceiling: 3, floor: 1 };
const IMPLEMENT_MODEL_CALLS: Bound =
    Bound { key: "implement_model_calls", default: 100, ceiling: 500, floor: 1 };
const REVIEWER_MODEL_CALLS: Bound =
    Bound { key: "reviewer_model_calls", default: 20, ceiling: 50, floor: 1 };
const BOARD_MODEL_CALLS: Bound =
    Bound { key: "board_model_calls", default: 48, ceiling: 150, floor: 1 };
const MAX_MODEL_CALLS: Bound =
    Bound { key: "max_model_calls", default: 400, ceiling: 2_000, floor: 1 };

/// Where the review architecture choice came from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SettingSource {
    /// No file or flag set it.
    #[default]
    Default,
    /// `[ultracode]` in the user config.
    UserConfig,
    /// `[ultracode]` in `.adk/harness.toml`.
    ProjectConfig,
    /// A command-line flag.
    Flag,
}

/// The `ultracode` flags that override the files (spec 3.1 §8.1).
///
/// # Example
///
/// ```rust
/// use adk_cli::harness::ultracode::{ArchitectureChoice, UltracodeFlags};
///
/// let flags = UltracodeFlags { architecture: Some(ArchitectureChoice::Graph), ..UltracodeFlags::default() };
/// assert!(flags.max_rounds.is_none());
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UltracodeFlags {
    /// `--architecture`.
    pub architecture: Option<ArchitectureChoice>,
    /// `--router`.
    pub router: Option<RouterMode>,
    /// `--max-rounds`.
    pub max_rounds: Option<i64>,
    /// `--max-model-calls`.
    pub max_model_calls: Option<u64>,
}

/// The resolved `[ultracode]` limits (spec 3.1 §7.10): the review architecture and router
/// mode, the round, reviewer and board caps, the per-step and run-wide model-call budgets,
/// and the operator's sensitive paths.
///
/// | Key | Default | Ceiling | In `.adk/harness.toml` |
/// |---|---|---|---|
/// | `architecture` | `auto` | — | `graph` only |
/// | `router` | `model` | — | `heuristic` only |
/// | `max_rounds`, `max_reviewers`, `max_board_rounds` | 2, 4, 2 | 5, 6, 3 | lower only; floors 1, 3, 1 |
/// | `implement_model_calls`, `reviewer_model_calls`, `board_model_calls` | 100, 20, 48 | 500, 50, 150 | lower only |
/// | `max_model_calls` | 400 | 2,000 | lower only |
/// | `sensitive_paths` | `[]` | — | append only |
///
/// # Example
///
/// ```rust
/// use adk_cli::harness::ultracode::{UltracodeFlags, UltracodeLimits};
///
/// let dir = tempfile::tempdir().unwrap();
/// let limits = UltracodeLimits::resolve(None, dir.path(), &UltracodeFlags::default()).unwrap();
/// assert_eq!(limits, UltracodeLimits::default());
/// assert_eq!(limits.worst_case_model_calls(), 100 + 1 + 2 * 128 + 100);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UltracodeLimits {
    /// The requested architecture; `auto` lets the router choose.
    pub architecture: ArchitectureChoice,
    /// How the router chooses under `auto`.
    pub router: RouterMode,
    /// Review rounds (review, then revise while changes are requested).
    pub max_rounds: u32,
    /// Most reviewers in one stage, the three floor seats included.
    pub max_reviewers: u32,
    /// Most rounds of one Blackboard board.
    pub max_board_rounds: u32,
    /// Most model calls of one implement or revise turn.
    pub implement_model_calls: u64,
    /// Most model calls of one independent reviewer.
    pub reviewer_model_calls: u64,
    /// Most model calls of one Blackboard board.
    pub board_model_calls: u64,
    /// Most model calls of the whole run.
    pub max_model_calls: u64,
    /// Globs of paths whose change makes the review `sensitive`.
    pub sensitive_paths: Vec<String>,
    /// Where `architecture` came from.
    #[serde(skip)]
    pub architecture_source: SettingSource,
    /// Settings that were clamped or ignored, and why.
    #[serde(skip)]
    pub notes: Vec<String>,
}

impl Default for UltracodeLimits {
    fn default() -> Self {
        Self {
            architecture: ArchitectureChoice::Auto,
            router: RouterMode::Model,
            max_rounds: 2,
            max_reviewers: 4,
            max_board_rounds: 2,
            implement_model_calls: IMPLEMENT_MODEL_CALLS.default,
            reviewer_model_calls: REVIEWER_MODEL_CALLS.default,
            board_model_calls: BOARD_MODEL_CALLS.default,
            max_model_calls: MAX_MODEL_CALLS.default,
            sensitive_paths: Vec::new(),
            architecture_source: SettingSource::Default,
            notes: Vec::new(),
        }
    }
}

/// The `[ultracode]` table as written.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct UltracodeTable {
    architecture: Option<String>,
    router: Option<String>,
    max_rounds: Option<i64>,
    max_reviewers: Option<i64>,
    max_board_rounds: Option<i64>,
    implement_model_calls: Option<i64>,
    reviewer_model_calls: Option<i64>,
    board_model_calls: Option<i64>,
    max_model_calls: Option<i64>,
    sensitive_paths: Option<Vec<String>>,
    #[serde(flatten)]
    unknown: BTreeMap<String, toml::Value>,
}

/// The `[ultracode]` table of `path`, if the file and the table exist.
fn load(path: &Path) -> Result<Option<UltracodeTable>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(AdkError::config(format!("failed to read {}: {error}", path.display())));
        }
    };
    let mut table: toml::Table = toml::from_str(&text).map_err(|error| {
        AdkError::config(format!("{} is not valid harness configuration: {error}", path.display()))
    })?;
    let Some(value) = table.remove("ultracode") else {
        return Ok(None);
    };
    value.try_into().map(Some).map_err(|error| {
        AdkError::config(format!("{}: [ultracode] is not valid: {error}", path.display()))
    })
}

fn parse_architecture(origin: &str, value: &str) -> Result<ArchitectureChoice> {
    match value.trim() {
        "auto" => Ok(ArchitectureChoice::Auto),
        "graph" => Ok(ArchitectureChoice::Graph),
        "blackboard" => Ok(ArchitectureChoice::Blackboard),
        other => Err(AdkError::config(format!(
            "{origin}: [ultracode] architecture '{other}' is unknown; use auto, graph or blackboard"
        ))),
    }
}

fn parse_router(origin: &str, value: &str) -> Result<RouterMode> {
    match value.trim() {
        "model" => Ok(RouterMode::Model),
        "heuristic" => Ok(RouterMode::Heuristic),
        other => Err(AdkError::config(format!(
            "{origin}: [ultracode] router '{other}' is unknown; use model or heuristic"
        ))),
    }
}

impl UltracodeLimits {
    /// Resolves the limits: the defaults, then `[ultracode]` in `user_config` (values beyond
    /// a ceiling or floor are clamped, with a note), then `[ultracode]` in
    /// `.adk/harness.toml` under `workspace` (tighten-only: `architecture = "graph"`,
    /// `router = "heuristic"`, lower numbers down to the floors, appended `sensitive_paths`;
    /// anything else is ignored with a note), then `flags`, which win (spec 3.1 §7.10, §8.1).
    ///
    /// # Errors
    ///
    /// Returns a configuration error when a file does not parse, a value is unknown or
    /// negative, a flag is outside its floor or ceiling, or the run could not afford one
    /// implement turn, the router and three reviewers within `max_model_calls`.
    pub fn resolve(
        user_config: Option<&Path>,
        workspace: &Path,
        flags: &UltracodeFlags,
    ) -> Result<Self> {
        let mut limits = Self::default();
        if let Some(path) = user_config
            && let Some(table) = load(path)?
        {
            limits.apply_user(&path.display().to_string(), &table)?;
        }
        if let Some(table) = load(&workspace.join(PROJECT_CONFIG_FILE))? {
            limits.apply_project(&table)?;
        }
        limits.apply_flags(flags)?;
        let floor = limits.minimum_model_calls();
        if floor > limits.max_model_calls {
            return Err(AdkError::config(format!(
                "ultracode needs at least {floor} model calls for one implement turn, the router \
                 and three reviewers (implement_model_calls {} + 1 + 3 × reviewer_model_calls {}), \
                 but max_model_calls is {}. Raise max_model_calls (--max-model-calls {floor}) or \
                 lower implement_model_calls or reviewer_model_calls in [ultracode]",
                limits.implement_model_calls, limits.reviewer_model_calls, limits.max_model_calls
            )));
        }
        Ok(limits)
    }

    /// The model calls of the worst case without the run-wide cap: the first turn, the
    /// router, every review stage at its largest (a board whose seats then fall back to
    /// independent reviewers, or a full independent stage) and every revision.
    pub fn worst_case_model_calls(&self) -> u64 {
        let reviewers = u64::from(self.max_reviewers) * self.reviewer_model_calls;
        let stage = reviewers.max(self.board_model_calls + reviewers);
        let rounds = u64::from(self.max_rounds);
        self.implement_model_calls
            + 1
            + rounds * stage
            + rounds.saturating_sub(1) * self.implement_model_calls
    }

    /// The bound the run is held to: the worst case, capped by `max_model_calls`.
    pub fn model_call_bound(&self) -> u64 {
        self.worst_case_model_calls().min(self.max_model_calls)
    }

    /// One implement turn, the router and three independent reviewers.
    fn minimum_model_calls(&self) -> u64 {
        self.implement_model_calls + 1 + 3 * self.reviewer_model_calls
    }

    fn numbers(&mut self) -> [(&'static Bound, NumberSlot<'_>); 7] {
        [
            (&MAX_ROUNDS, NumberSlot::U32(&mut self.max_rounds)),
            (&MAX_REVIEWERS, NumberSlot::U32(&mut self.max_reviewers)),
            (&MAX_BOARD_ROUNDS, NumberSlot::U32(&mut self.max_board_rounds)),
            (&IMPLEMENT_MODEL_CALLS, NumberSlot::U64(&mut self.implement_model_calls)),
            (&REVIEWER_MODEL_CALLS, NumberSlot::U64(&mut self.reviewer_model_calls)),
            (&BOARD_MODEL_CALLS, NumberSlot::U64(&mut self.board_model_calls)),
            (&MAX_MODEL_CALLS, NumberSlot::U64(&mut self.max_model_calls)),
        ]
    }

    fn apply_user(&mut self, origin: &str, table: &UltracodeTable) -> Result<()> {
        if let Some(value) = &table.architecture {
            self.architecture = parse_architecture(origin, value)?;
            self.architecture_source = SettingSource::UserConfig;
        }
        if let Some(value) = &table.router {
            self.router = parse_router(origin, value)?;
        }
        let mut notes = Vec::new();
        for (bound, mut slot) in self.numbers() {
            let Some(value) = table.value(bound.key) else {
                continue;
            };
            let value = non_negative(origin, bound.key, value)?;
            let clamped = value.clamp(bound.floor, bound.ceiling);
            if clamped != value {
                notes.push(format!(
                    "{origin}: [ultracode] {key} = {value} is outside {floor}..={ceiling}; using {clamped}",
                    key = bound.key,
                    floor = bound.floor,
                    ceiling = bound.ceiling
                ));
            }
            slot.set(clamped);
        }
        self.notes.extend(notes);
        self.sensitive_paths.extend(table.sensitive_paths.iter().flatten().cloned());
        for key in table.unknown.keys() {
            self.notes.push(format!("{origin}: unknown key `ultracode.{key}` ignored"));
        }
        Ok(())
    }

    fn apply_project(&mut self, table: &UltracodeTable) -> Result<()> {
        let origin = PROJECT_CONFIG_FILE;
        if let Some(value) = &table.architecture {
            match parse_architecture(origin, value)? {
                ArchitectureChoice::Graph => {
                    self.architecture = ArchitectureChoice::Graph;
                    self.architecture_source = SettingSource::ProjectConfig;
                }
                requested if requested == self.architecture => {}
                requested => self.notes.push(loosening_note(
                    "ultracode.architecture",
                    &format!(
                        "\"{requested}\" would choose the review for you; only \"graph\" is accepted here"
                    ),
                )),
            }
        }
        if let Some(value) = &table.router {
            match parse_router(origin, value)? {
                RouterMode::Heuristic => self.router = RouterMode::Heuristic,
                requested if requested == self.router => {}
                requested => self.notes.push(loosening_note(
                    "ultracode.router",
                    &format!(
                        "\"{requested}\" adds a model call; only \"heuristic\" is accepted here"
                    ),
                )),
            }
        }
        let mut notes = Vec::new();
        for (bound, mut slot) in self.numbers() {
            let Some(value) = table.value(bound.key) else {
                continue;
            };
            let value = non_negative(origin, bound.key, value)?;
            let current = slot.get();
            if value > current {
                notes.push(loosening_note(
                    &format!("ultracode.{}", bound.key),
                    &format!("{value} is above the {current} in effect"),
                ));
            } else if value < bound.floor {
                notes.push(format!(
                    "{origin}: [ultracode] {key} = {value} is below its floor; using {floor}",
                    key = bound.key,
                    floor = bound.floor
                ));
                slot.set(bound.floor.min(current));
            } else {
                slot.set(value);
            }
        }
        self.notes.extend(notes);
        self.sensitive_paths.extend(table.sensitive_paths.iter().flatten().cloned());
        for key in table.unknown.keys() {
            self.notes.push(format!("{origin}: unknown key `ultracode.{key}` ignored"));
        }
        Ok(())
    }

    fn apply_flags(&mut self, flags: &UltracodeFlags) -> Result<()> {
        if let Some(architecture) = flags.architecture {
            self.architecture = architecture;
            self.architecture_source = SettingSource::Flag;
        }
        if let Some(router) = flags.router {
            self.router = router;
        }
        let flag = |name: &str, bound: &Bound, value: u64| -> Result<u64> {
            if value < bound.floor || value > bound.ceiling {
                return Err(AdkError::config(format!(
                    "--{name} {value} is outside {}..={}: the ceiling of `{}` is {}",
                    bound.floor, bound.ceiling, bound.key, bound.ceiling
                )));
            }
            Ok(value)
        };
        if let Some(rounds) = flags.max_rounds {
            let rounds = u64::try_from(rounds).unwrap_or(0);
            self.max_rounds =
                u32::try_from(flag("max-rounds", &MAX_ROUNDS, rounds)?).unwrap_or(self.max_rounds);
        }
        if let Some(calls) = flags.max_model_calls {
            self.max_model_calls = flag("max-model-calls", &MAX_MODEL_CALLS, calls)?;
        }
        Ok(())
    }
}

impl UltracodeTable {
    fn value(&self, key: &str) -> Option<i64> {
        match key {
            "max_rounds" => self.max_rounds,
            "max_reviewers" => self.max_reviewers,
            "max_board_rounds" => self.max_board_rounds,
            "implement_model_calls" => self.implement_model_calls,
            "reviewer_model_calls" => self.reviewer_model_calls,
            "board_model_calls" => self.board_model_calls,
            "max_model_calls" => self.max_model_calls,
            _ => None,
        }
    }
}

fn non_negative(origin: &str, key: &str, value: i64) -> Result<u64> {
    u64::try_from(value).map_err(|_| {
        AdkError::config(format!("{origin}: [ultracode] {key} = {value} must not be negative"))
    })
}

/// A numeric limit, `u32` or `u64`, behind one interface.
enum NumberSlot<'a> {
    U32(&'a mut u32),
    U64(&'a mut u64),
}

impl NumberSlot<'_> {
    fn get(&self) -> u64 {
        match self {
            Self::U32(value) => u64::from(**value),
            Self::U64(value) => **value,
        }
    }

    fn set(&mut self, value: u64) {
        match self {
            Self::U32(slot) => **slot = u32::try_from(value).unwrap_or(u32::MAX),
            Self::U64(slot) => **slot = value,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn project_config_only_tightens() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("user.toml");
        write(&user, "[ultracode]\nmax_model_calls = 9000\nreviewer_model_calls = 30\n");
        write(
            &dir.path().join(PROJECT_CONFIG_FILE),
            "mode = \"ask\"\n[ultracode]\narchitecture = \"blackboard\"\nrouter = \"heuristic\"\n\
             max_reviewers = 2\nmax_rounds = 1\nboard_model_calls = 400\nimplement_model_calls = 60\n\
             sensitive_paths = [\"migrations/*\"]\nsurprise = true\n",
        );
        let limits =
            UltracodeLimits::resolve(Some(&user), dir.path(), &UltracodeFlags::default()).unwrap();
        assert_eq!(
            limits,
            UltracodeLimits {
                architecture: ArchitectureChoice::Auto,
                router: RouterMode::Heuristic,
                max_rounds: 1,
                max_reviewers: 3,
                implement_model_calls: 60,
                reviewer_model_calls: 30,
                max_model_calls: 2_000,
                sensitive_paths: vec!["migrations/*".into()],
                notes: limits.notes.clone(),
                ..UltracodeLimits::default()
            }
        );
        let notes = limits.notes.join("\n");
        for needle in [
            "max_model_calls = 9000 is outside 1..=2000; using 2000",
            "`ultracode.architecture` ignored",
            "max_reviewers = 2 is below its floor; using 3",
            "`ultracode.board_model_calls` ignored — 400 is above the 48 in effect",
            "unknown key `ultracode.surprise` ignored",
        ] {
            assert!(notes.contains(needle), "{needle}: {notes}");
        }

        write(&dir.path().join(PROJECT_CONFIG_FILE), "[ultracode]\narchitecture = \"graph\"\n");
        let graph = UltracodeLimits::resolve(None, dir.path(), &UltracodeFlags::default()).unwrap();
        assert_eq!(graph.architecture, ArchitectureChoice::Graph);
        assert_eq!(graph.architecture_source, SettingSource::ProjectConfig);

        // Flags win, within their ceilings.
        let flags = UltracodeFlags {
            architecture: Some(ArchitectureChoice::Blackboard),
            router: Some(RouterMode::Model),
            max_rounds: Some(5),
            max_model_calls: Some(1_000),
        };
        let flagged = UltracodeLimits::resolve(None, dir.path(), &flags).unwrap();
        assert_eq!(
            (flagged.architecture, flagged.architecture_source, flagged.router),
            (ArchitectureChoice::Blackboard, SettingSource::Flag, RouterMode::Model)
        );
        assert_eq!((flagged.max_rounds, flagged.max_model_calls), (5, 1_000));
        let over = UltracodeFlags { max_rounds: Some(6), ..UltracodeFlags::default() };
        let error = UltracodeLimits::resolve(None, dir.path(), &over).unwrap_err();
        assert!(error.to_string().contains("the ceiling of `max_rounds` is 5"), "{error}");
        let over = UltracodeFlags { max_model_calls: Some(2_001), ..UltracodeFlags::default() };
        let error = UltracodeLimits::resolve(None, dir.path(), &over).unwrap_err();
        assert!(error.to_string().contains("2000"), "{error}");
    }

    #[test]
    fn a_budget_that_cannot_afford_one_review_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let flags = UltracodeFlags { max_model_calls: Some(160), ..UltracodeFlags::default() };
        let error = UltracodeLimits::resolve(None, dir.path(), &flags).unwrap_err();
        assert!(error.to_string().contains("at least 161 model calls"), "{error}");
        let flags = UltracodeFlags { max_model_calls: Some(161), ..UltracodeFlags::default() };
        assert!(UltracodeLimits::resolve(None, dir.path(), &flags).is_ok());
    }

    #[test]
    fn invalid_values_name_the_file() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join(PROJECT_CONFIG_FILE), "[ultracode]\nmax_rounds = -1\n");
        let error =
            UltracodeLimits::resolve(None, dir.path(), &UltracodeFlags::default()).unwrap_err();
        assert!(error.to_string().contains("must not be negative"), "{error}");
        write(&dir.path().join(PROJECT_CONFIG_FILE), "[ultracode]\nrouter = \"oracle\"\n");
        let error =
            UltracodeLimits::resolve(None, dir.path(), &UltracodeFlags::default()).unwrap_err();
        assert!(error.to_string().contains("router 'oracle' is unknown"), "{error}");
    }
}
