//! What `adk-rust` tells the harness about itself: its slash commands, subcommands and
//! flags, generated from the command tables and the clap tree (spec 3.1 §7.25).

use adk_harness::{CommandEntry, KernelEntry, SurfaceManifest};
use clap::CommandFactory;

/// The subcommands whose flags the manifest lists, as typed.
const FLAG_COMMANDS: &[&[&str]] =
    &[&["rules", "add"], &["memory"], &["sessions"], &["up"], &["wake"]];

/// The packs `--pack` turns on.
const PACKS: &[&str] = &["browser", "computer-use", "payments", "voice"];

/// The `adk-rust` surface: every slash command of the REPL, every subcommand with its
/// `about` and long help, and the flags of the REPL and of `rules add`, `memory`,
/// `sessions`, `up` and `wake` with their help lines. `kernel_running` says whether this
/// harness runs under `adk-rust up`.
pub(crate) fn cli_surface(kernel_running: bool) -> SurfaceManifest {
    let mut surface = SurfaceManifest::new("adk-rust", env!("CARGO_PKG_VERSION"))
        .commands(adk_cli::harness::slash_command_entries())
        .with_kernel(KernelEntry::new(true, kernel_running));
    for pack in PACKS {
        surface = surface.pack_switch(*pack, format!("--pack {pack}"));
    }
    let cli = crate::cli::Cli::command();
    for sub in cli.get_subcommands().filter(|sub| !sub.is_hide_set()) {
        let about = sub.get_about().map(ToString::to_string).unwrap_or_default();
        let long = sub.clone().render_long_help().to_string();
        surface = surface.command(CommandEntry::subcommand(sub.get_name(), about, Some(long)));
    }
    surface = surface.commands(flags(&cli, &[]));
    for path in FLAG_COMMANDS {
        if let Some(command) = find(&cli, path) {
            surface = surface.commands(flags(command, path));
        }
    }
    surface
}

/// The subcommand at `path` below `cli`.
fn find<'a>(cli: &'a clap::Command, path: &[&str]) -> Option<&'a clap::Command> {
    path.iter().try_fold(cli, |command, name| command.find_subcommand(name))
}

/// The visible long flags of `command`, typed after `path`.
fn flags(command: &clap::Command, path: &[&str]) -> Vec<CommandEntry> {
    command
        .get_arguments()
        .filter(|arg| !arg.is_hide_set())
        .filter_map(|arg| {
            let long = arg.get_long()?;
            let help = arg.get_help().map(ToString::to_string).unwrap_or_default();
            let typed = if path.is_empty() {
                format!("--{long}")
            } else {
                format!("{} --{long}", path.join(" "))
            };
            Some(CommandEntry::flag(typed, help.lines().next().unwrap_or_default().to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_harness::CommandKind;

    fn names(surface: &SurfaceManifest, kind: CommandKind) -> Vec<String> {
        surface
            .commands
            .iter()
            .filter(|command| command.kind == kind)
            .map(|command| command.name.clone())
            .collect()
    }

    #[test]
    fn every_slash_command_and_built_in_is_listed() {
        let surface = cli_surface(false);
        let slash = names(&surface, CommandKind::Slash);
        for entry in adk_cli::harness::slash_command_entries() {
            assert!(slash.contains(&entry.name), "/{} is missing", entry.name);
        }
        for built_in in ["help", "clear", "exit"] {
            assert!(slash.iter().any(|name| name == built_in), "/{built_in} is missing");
        }
    }

    #[test]
    fn every_visible_subcommand_is_listed_with_its_long_help() {
        let surface = cli_surface(false);
        let cli = crate::cli::Cli::command();
        for sub in cli.get_subcommands().filter(|sub| !sub.is_hide_set()) {
            let entry = surface
                .commands
                .iter()
                .find(|command| {
                    command.kind == CommandKind::Subcommand && command.name == sub.get_name()
                })
                .unwrap_or_else(|| panic!("subcommand {} is missing", sub.get_name()));
            assert!(entry.usage.as_deref().is_some_and(|usage| usage.contains("Usage:")));
        }
    }

    #[test]
    fn the_flags_of_the_repl_and_the_named_subcommands_are_listed() {
        let surface = cli_surface(true);
        let flags = names(&surface, CommandKind::Flag);
        assert!(flags.iter().any(|flag| flag == "--continue"), "{flags:?}");
        assert!(flags.iter().any(|flag| flag.starts_with("rules add --")), "{flags:?}");
        assert!(flags.iter().any(|flag| flag.starts_with("up --")), "{flags:?}");
        assert_eq!(surface.kernel, Some(KernelEntry::new(true, true)));
        assert_eq!(
            surface.pack_switches.get("browser").map(String::as_str),
            Some("--pack browser")
        );
    }

    #[test]
    fn the_adk_rust_block_fits_its_cap_and_keeps_its_commands() {
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let harness = adk_harness::HarnessAgent::builder("harness")
            .model(Arc::new(adk_model::ScriptedLlm::new(
                "scripted",
                vec![adk_model::ScriptedResponse::text("ok")],
            )))
            .workspace(adk_devtools::Workspace::new(dir.path()).allow_bash(true))
            .project_instructions(adk_harness::InstructionsConfig {
                user_file: None,
                ..adk_harness::InstructionsConfig::default()
            })
            .surface(cli_surface(false))
            .build()
            .unwrap();
        let block = harness.manifest().render();
        assert!(block.len() <= adk_harness::HARNESS_BLOCK_MAX_BYTES, "{} bytes", block.len());
        assert!(
            block.contains("/approvals  Calls in this session waiting for a decision"),
            "{block}"
        );
        assert!(block.contains("adk-rust up  "), "the subcommands stay in the block: {block}");
        assert!(block.contains("Safety"), "{block}");
    }
}
