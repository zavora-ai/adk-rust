//! `adk-rust pair` (spec 3.3 §10 K1, K2): pair a phone, a tray or a terminal with a code,
//! list what may reach your agents, remove one, and accept the code a chat account was sent.

use adk_kernel::control::ControlClient;
use adk_kernel::control::api::{ChannelAccountState, IssueRequest};
use adk_kernel::pairing::{DeviceScope, DeviceState, DeviceView};
use anyhow::{Result, bail};
use chrono::{DateTime, Local, Utc};

use crate::cli::KernelTarget;
use crate::kernel_cli::client;

/// `adk-rust pair` subcommands.
#[derive(clap::Subcommand, Debug, Clone, PartialEq, Eq)]
pub enum PairCommand {
    /// The paired devices and chat accounts, and the chat accounts waiting for their code to
    /// be accepted
    List,
    /// Remove a paired device or chat account: its access ends at once
    Revoke {
        /// The device or account id, a unique prefix of it, or its name
        device: String,
    },
    /// Pair the chat account that was sent this code, so its messages reach your agents
    Accept {
        /// The code the chat account was sent, such as ABCD-2345
        code: String,
    },
}

/// What `adk-rust pair` with no subcommand issues a code for.
#[derive(clap::Args, Debug, Clone, Default, PartialEq, Eq)]
pub struct IssueArgs {
    /// What to call the device, such as "Ana's phone"
    #[arg(long)]
    pub name: Option<String>,
    /// What the device may do, comma-separated: decide, read, stop (default: all three)
    #[arg(long, value_delimiter = ',', value_parser = parse_scope)]
    pub scope: Vec<DeviceScope>,
    /// How many days the device stays paired (default: 90, at most 365)
    #[arg(long, value_name = "DAYS")]
    pub days: Option<u32>,
}

fn parse_scope(value: &str) -> Result<DeviceScope, String> {
    value.parse().map_err(|error: adk_kernel::KernelError| error.to_string())
}

/// `adk-rust pair [list|revoke|accept]`.
pub async fn run(
    target: &KernelTarget,
    command: Option<PairCommand>,
    issue: IssueArgs,
) -> Result<()> {
    let client = client(target).await?;
    match command {
        None => issue_code(&client, issue).await,
        Some(PairCommand::List) => list(&client).await,
        Some(PairCommand::Revoke { device }) => revoke(&client, &device).await,
        Some(PairCommand::Accept { code }) => {
            let account = client.accept_channel(&code).await?;
            println!(
                "Paired {} on {}: its messages reach your agents, and it can say yes once or no \
                 to everyday decisions. Remove it with `adk-rust pair revoke {}`.",
                account.display_name.as_deref().unwrap_or(&account.account),
                account.channel,
                short(&account.id)
            );
            Ok(())
        }
    }
}

async fn issue_code(client: &ControlClient, args: IssueArgs) -> Result<()> {
    let request = IssueRequest { name: args.name, scopes: args.scope, expires_in_days: args.days };
    let issued = client.issue_pairing_code(&request).await?;
    let scopes: Vec<&str> = issued.scopes.iter().map(|scope| scope.as_str()).collect();
    println!("Pairing code  {}", issued.code);
    println!("Works until   {}", local(issued.expires_at));
    println!("It may        {}", scopes.join(", "));
    println!();
    match &issued.url {
        Some(url) => println!("On the phone, open {url}"),
        None => println!(
            "Enter the code on the device. It pairs one device; ask for a new code for each."
        ),
    }
    println!("See what is paired with `adk-rust pair list`.");
    Ok(())
}

async fn list(client: &ControlClient) -> Result<()> {
    let devices = client.devices().await?;
    // A kernel without chat channels has no accounts to list.
    let channels = client.channel_accounts().await.ok();
    let accounts = channels.as_ref().map(|channels| channels.accounts.as_slice()).unwrap_or(&[]);
    let waiting = channels.as_ref().map(|channels| channels.waiting.as_slice()).unwrap_or(&[]);
    if devices.is_empty() && accounts.is_empty() && waiting.is_empty() {
        println!("Nothing is paired. Pair a phone or a terminal with `adk-rust pair`.");
        return Ok(());
    }
    println!("{:<18} {:<24} {:<20} {:<10} SINCE", "ID", "NAME", "MAY", "STATE");
    for device in &devices {
        println!("{}", device_line(device));
    }
    for account in accounts {
        let state = match account.state {
            ChannelAccountState::Active => "paired",
            _ => "removed",
        };
        let name = format!(
            "{} ({})",
            terminal_safe(account.display_name.as_deref().unwrap_or(&account.account)),
            account.channel
        );
        println!(
            "{:<18} {:<24} {:<20} {:<10} {}",
            short(&account.id),
            clip(&name, 24),
            "chat, decide, stop",
            state,
            local(account.paired_at)
        );
    }
    for request in waiting {
        let name = request.display_name.as_deref().unwrap_or(&request.account);
        println!(
            "{} on {} asked to pair; accept its code with `adk-rust pair accept <code>` before {}",
            terminal_safe(name),
            request.channel,
            local(request.expires_at)
        );
    }
    Ok(())
}

fn device_line(device: &DeviceView) -> String {
    let scopes: Vec<&str> = device.scopes.iter().map(|scope| scope.as_str()).collect();
    let state = match device.state {
        DeviceState::Active => "paired",
        DeviceState::Expired => "expired",
        _ => "removed",
    };
    format!(
        "{:<18} {:<24} {:<20} {:<10} {}",
        device.id,
        clip(&terminal_safe(&device.name), 24),
        scopes.join(", "),
        state,
        local(device.created_at)
    )
}

async fn revoke(client: &ControlClient, wanted: &str) -> Result<()> {
    let devices = client.devices().await?;
    let device_matches: Vec<&DeviceView> = devices
        .iter()
        .filter(|device| device.state == DeviceState::Active)
        .filter(|device| device.id.starts_with(wanted) || device.name == wanted)
        .collect();
    let accounts =
        client.channel_accounts().await.map(|channels| channels.accounts).unwrap_or_default();
    let account_matches: Vec<_> = accounts
        .iter()
        .filter(|account| account.state == ChannelAccountState::Active)
        .filter(|account| {
            account.id.starts_with(wanted)
                || account.display_name.as_deref() == Some(wanted)
                || account.account == wanted
        })
        .collect();
    match (device_matches.as_slice(), account_matches.as_slice()) {
        ([device], []) => {
            let removed = client.revoke_device(&device.id).await?;
            println!("Removed {}: its token no longer works.", terminal_safe(&removed.name));
        }
        ([], [account]) => {
            let removed = client.revoke_channel_account(&account.id).await?;
            println!(
                "Removed {} on {}: its messages no longer reach your agents.",
                terminal_safe(removed.display_name.as_deref().unwrap_or(&removed.account)),
                removed.channel
            );
        }
        ([], []) => bail!("nothing paired matches {wanted}: see `adk-rust pair list`"),
        _ => bail!("{wanted} matches more than one paired device or account: use more of its id"),
    }
    Ok(())
}

/// `at` on this machine's clock, to the minute.
fn local(at: DateTime<Utc>) -> String {
    at.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string()
}

fn short(id: &str) -> &str {
    &id[..id.len().min(18)]
}

/// `text` cut to `width` characters.
fn clip(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(width.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// A name a device or a chat platform supplied, with its control characters escaped, so it
/// cannot drive the terminal (spec 3.3 S-I9).
fn terminal_safe(text: &str) -> String {
    adk_harness::escape_control(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_parse_from_a_comma_list() {
        use clap::Parser;

        #[derive(clap::Parser)]
        struct Cli {
            #[command(flatten)]
            issue: IssueArgs,
        }

        let cli = Cli::parse_from(["pair", "--scope", "read,decide", "--name", "Phone"]);
        assert_eq!(cli.issue.scope, [DeviceScope::Read, DeviceScope::Decide]);
        assert!(Cli::try_parse_from(["pair", "--scope", "always"]).is_err());
    }

    #[test]
    fn names_are_clipped_and_cannot_drive_the_terminal() {
        assert_eq!(clip("abcdef", 4), "abc…");
        assert!(!terminal_safe("phone\u{1b}[2J").contains('\u{1b}'));
    }
}
