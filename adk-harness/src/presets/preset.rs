//! Harness presets: one choice that sets the prompt, the mode, the members and the shell of
//! a harness for the kind of work it does (spec 3.1 §7.19, spec 3.3 §10 H1).

use crate::builder::HarnessAgentBuilder;
use crate::mode::HarnessMode;
use crate::shell::ShellIsolation;

/// A harness preset (spec 3.1 §7.19, spec 3.3 §10 H1), chosen with
/// [`HarnessAgentBuilder::preset`] or `adk-rust --preset`.
///
/// | Preset | Workspace | Prompt | Mode | Members | Shell |
/// |---|---|---|---|---|---|
/// | `Coding` | A code repository | [`CODING_SYSTEM_PROMPT`](crate::coding::CODING_SYSTEM_PROMPT) | Ask | None | Host |
/// | `Personal` | The caller's; [`PersonalAgent`](crate::personal::PersonalAgent) and `adk-rust --preset personal` use a folder under the harness home ([`PersonalWorkspace`](crate::personal::PersonalWorkspace)) | [`PERSONAL_SYSTEM_PROMPT`](crate::personal::PERSONAL_SYSTEM_PROMPT) | Ask | None | Only in the OS sandbox or a container, with no unsandboxed retry |
/// | `Website` (feature `website`) | The site's content repository | `WEBSITE_SYSTEM_PROMPT` | Plan: every job proposes until the charter says otherwise | None | Off |
///
/// A preset replaces what an earlier one set. Builder calls after `preset` override what it
/// sets, so `adk-rust --mode plan --preset personal` starts in plan mode; the personal
/// preset's shell is the exception, and no later call gives it a host shell. Long-term
/// memory is not part of a preset: [`PersonalAgent`](crate::personal::PersonalAgent) turns
/// it on, and so does the CLI wherever it keeps memory.
///
/// # Example
///
/// ```rust
/// use adk_harness::Preset;
///
/// assert_eq!("personal".parse::<Preset>(), Ok(Preset::Personal));
/// assert_eq!(Preset::Coding.to_string(), "coding");
/// assert!("chef".parse::<Preset>().is_err());
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Preset {
    /// A coding agent over a repository (spec 3.1 §7.19).
    Coding,
    /// A personal agent whose workspace is a folder of notes and files, not a code repository
    /// (spec 3.3 §10 H1).
    Personal,
    /// An agent of an AWP website's team: the website prompt, plan mode until the charter's
    /// authority compiles to more, no shell, and the website tools classed by their
    /// catalogue; `HarnessAgentBuilder::website_back_office` and `website_public` attach the
    /// toolsets (spec 3.5 §7.1, §10 X10).
    #[cfg(feature = "website")]
    Website,
}

impl Preset {
    /// The preset's name, as `--preset` takes it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Coding => "coding",
            Self::Personal => "personal",
            #[cfg(feature = "website")]
            Self::Website => "website",
        }
    }

    /// Lowers the preset onto `builder` (spec 3.1 §7.19, spec 3.3 §10 H1).
    pub(crate) fn apply(self, builder: HarnessAgentBuilder) -> HarnessAgentBuilder {
        let builder = builder.clear_preset();
        match self {
            Self::Coding => builder
                .base_prompt(crate::coding::CODING_SYSTEM_PROMPT)
                .mode(HarnessMode::Ask)
                .shell(ShellIsolation::Host)
                .without_default_members(),
            Self::Personal => builder
                .base_prompt(crate::personal::PERSONAL_SYSTEM_PROMPT)
                .description(crate::personal::PERSONAL_DESCRIPTION)
                .mode(HarnessMode::Ask)
                .without_default_members()
                .isolated_shell_only(true),
            #[cfg(feature = "website")]
            Self::Website => crate::website::apply_preset(builder),
        }
    }
}

impl std::fmt::Display for Preset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Preset {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "coding" => Ok(Self::Coding),
            "personal" => Ok(Self::Personal),
            #[cfg(feature = "website")]
            "website" => Ok(Self::Website),
            #[cfg(not(feature = "website"))]
            "website" => Err("the website preset needs adk-harness's `website` feature".into()),
            other => Err(format!("unknown preset '{other}'; use coding, personal or website")),
        }
    }
}
