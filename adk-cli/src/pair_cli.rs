//! `adk-rust pair` (spec 3.3 §10 K1, K2): pair a phone, a tray or a terminal with a code,
//! list what may reach your agents, remove one, and accept the code a chat account was sent.

use adk_kernel::control::ControlClient;
use adk_kernel::control::api::{
    ChannelAccountState, ChannelAccountView, ChannelAccountsResponse, ChannelPairingView,
    IssueRequest,
};
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
    /// Remove a paired device or chat account: its access ends at once, and a chat account's
    /// unfinished turns stop. A chat account waiting to be paired is dismissed: its code stops
    /// working and it gets no new one for a day
    Revoke {
        /// The device or account id, a unique prefix of it, or its name
        device: String,
    },
    /// Pair the chat account that was sent this code, so its messages reach your agents
    Accept {
        /// The code the chat account was sent, such as ABCD-EFGH-2345
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
    /// How many days the device stays paired, 1 to 365 (default: 90)
    #[arg(long, value_name = "DAYS", value_parser = clap::value_parser!(u32).range(1..=365))]
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
            println!("{}", paired(&account));
            Ok(())
        }
    }
}

/// What `pair accept` says of the account it paired, its name escaped (spec 3.3 S-I9).
fn paired(account: &ChannelAccountView) -> String {
    format!(
        "Paired {} on {}: its messages reach your agents, and it can say yes once or no to \
         everyday decisions. Remove it with `adk-rust pair revoke {}`.",
        terminal_safe(account_name(account)),
        terminal_safe(&account.channel),
        short(&account.id)
    )
}

async fn issue_code(client: &ControlClient, args: IssueArgs) -> Result<()> {
    let request = IssueRequest { name: args.name, scopes: args.scope, expires_in_days: args.days };
    let issued = client.issue_pairing_code(&request).await?;
    let scopes: Vec<&str> = issued.scopes.iter().map(|scope| scope.as_str()).collect();
    println!("Pairing code  {}", issued.code);
    println!("Works until   {}", local(issued.expires_at));
    println!("It may        {}", scopes.join(", "));
    println!("Paired for    {} days once it pairs", issued.device_ttl_secs.div_ceil(86_400));
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
    // A kernel without chat channels lists none; any other failure is reported.
    let channels = client.channel_accounts().await?;
    print!("{}", listing(&devices, &channels));
    Ok(())
}

/// What `pair list` prints for `devices` and `channels`.
fn listing(devices: &[DeviceView], channels: &ChannelAccountsResponse) -> String {
    use std::fmt::Write as _;

    let (accounts, waiting) = (&channels.accounts, &channels.waiting);
    if devices.is_empty() && accounts.is_empty() && waiting.is_empty() {
        return "Nothing is paired. Pair a phone or a terminal with `adk-rust pair`.\n".into();
    }
    let mut out = format!("{:<18} {:<24} {:<20} {:<10} SINCE\n", "ID", "NAME", "MAY", "STATE");
    for device in devices {
        let _ = writeln!(out, "{}", device_line(device));
    }
    for account in accounts {
        let state = match account.state {
            ChannelAccountState::Active => "paired",
            _ => "removed",
        };
        let name = format!("{} ({})", terminal_safe(account_name(account)), account.channel);
        let _ = writeln!(
            out,
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
        let _ = writeln!(
            out,
            "{} on {} asked to pair; accept its code with `adk-rust pair accept <code>` before \
             {}, or dismiss it with `adk-rust pair revoke {}`",
            terminal_safe(name),
            request.channel,
            local(request.expires_at),
            short(&request.id)
        );
    }
    out
}

fn account_name(account: &ChannelAccountView) -> &str {
    account.display_name.as_deref().unwrap_or(&account.account)
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

/// What `pair revoke` acts on.
#[derive(Debug, PartialEq)]
enum Target<'a> {
    Device(&'a DeviceView),
    Account(&'a ChannelAccountView),
    Waiting(&'a ChannelPairingView),
}

/// The one paired device, paired chat account or waiting chat account `wanted` names: by a
/// prefix of its id, by its exact name, or for an account by its account id. Removed and
/// expired ones are not candidates.
fn resolve<'a>(
    wanted: &str,
    devices: &'a [DeviceView],
    channels: &'a ChannelAccountsResponse,
) -> Result<Target<'a>> {
    let mut matches: Vec<Target<'a>> = devices
        .iter()
        .filter(|device| device.state == DeviceState::Active)
        .filter(|device| device.id.starts_with(wanted) || device.name == wanted)
        .map(Target::Device)
        .collect();
    matches.extend(
        channels
            .accounts
            .iter()
            .filter(|account| account.state == ChannelAccountState::Active)
            .filter(|account| {
                account.id.starts_with(wanted)
                    || account.display_name.as_deref() == Some(wanted)
                    || account.account == wanted
            })
            .map(Target::Account),
    );
    matches.extend(
        channels
            .waiting
            .iter()
            .filter(|request| {
                request.id.starts_with(wanted)
                    || request.display_name.as_deref() == Some(wanted)
                    || request.account == wanted
            })
            .map(Target::Waiting),
    );
    match matches.len() {
        1 => Ok(matches.remove(0)),
        0 => bail!("nothing paired matches {wanted}: see `adk-rust pair list`"),
        _ => bail!("{wanted} matches more than one paired device or account: use more of its id"),
    }
}

async fn revoke(client: &ControlClient, wanted: &str) -> Result<()> {
    let devices = client.devices().await?;
    let channels = client.channel_accounts().await?;
    match resolve(wanted, &devices, &channels)? {
        Target::Device(device) => {
            let removed = client.revoke_device(&device.id).await?;
            println!("Removed {}: its token no longer works.", terminal_safe(&removed.name));
        }
        Target::Account(account) => {
            let removed = client.revoke_channel_account(&account.id).await?;
            println!(
                "Removed {} on {}: its messages no longer reach your agents, and its turns \
                 that had not finished were stopped.",
                terminal_safe(account_name(&removed)),
                terminal_safe(&removed.channel)
            );
        }
        Target::Waiting(request) => {
            let dismissed = client.dismiss_channel_request(&request.id).await?;
            println!(
                "Dismissed {} on {}: its pairing code no longer works, and it gets no new one \
                 for a day.",
                terminal_safe(dismissed.display_name.as_deref().unwrap_or(&dismissed.account)),
                terminal_safe(&dismissed.channel)
            );
        }
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

    /// A chat account's display name is the platform's, untrusted: `pair accept` escapes it
    /// as `pair list` does, bidi controls included (spec 3.3 S-I6, S-I9).
    #[test]
    fn pair_accept_escapes_the_accounts_name() {
        let account: ChannelAccountView = serde_json::from_value(serde_json::json!({
            "id": "ch-77f0aa",
            "channel": "telegram",
            "account": "4217",
            "displayName": "Ana\u{202e}gnp.exe",
            "userId": "ana",
            "pairedAt": "2026-10-25T07:00:00Z",
            "state": "active"
        }))
        .unwrap();
        let line = paired(&account);
        assert!(!line.contains('\u{202e}'), "{line}");
        assert!(line.starts_with("Paired Ana"), "{line}");
    }

    /// A pairing of no days, or of more than a year, is refused.
    #[test]
    fn days_are_one_to_365() {
        use clap::Parser;

        #[derive(clap::Parser)]
        struct Cli {
            #[command(flatten)]
            issue: IssueArgs,
        }

        for days in ["0", "366", "-1"] {
            assert!(Cli::try_parse_from(["pair", "--days", days]).is_err(), "--days {days}");
        }
        for days in [1, 90, 365] {
            let cli = Cli::parse_from(["pair", "--days", &days.to_string()]);
            assert_eq!(cli.issue.days, Some(days));
        }
    }

    fn device(id: &str, name: &str, state: &str) -> DeviceView {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": name, "owner": "ana", "scopes": ["read"],
            "createdAt": "2026-10-01T09:00:00Z", "expiresAt": "2027-01-01T09:00:00Z",
            "state": state,
        }))
        .unwrap()
    }

    fn channels(
        accounts: serde_json::Value,
        waiting: serde_json::Value,
    ) -> ChannelAccountsResponse {
        serde_json::from_value(serde_json::json!({ "accounts": accounts, "waiting": waiting }))
            .unwrap()
    }

    fn account(id: &str, account: &str, name: &str, state: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id, "channel": "telegram", "account": account, "displayName": name,
            "userId": "ana", "pairedAt": "2026-10-02T09:00:00Z", "state": state,
        })
    }

    fn waiting(id: &str, account: &str, name: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id, "channel": "telegram", "account": account, "displayName": name,
            "requestedAt": "2026-10-03T09:00:00Z", "expiresAt": "2026-10-03T10:00:00Z",
        })
    }

    /// `pair revoke` acts on one active device or account, matched by
    /// id prefix, name or account id, and refuses an ambiguous match.
    #[test]
    fn revoke_resolves_one_active_match() {
        let devices = [
            device("e2a1000000000001", "Ana's phone", "active"),
            device("e2b2000000000002", "Old tablet", "revoked"),
            device("f3c3000000000003", "Tray", "expired"),
        ];
        let channels = channels(
            serde_json::json!([
                account("ch-aa11", "4217", "Ana T", "active"),
                account("ch-bb22", "9001", "Gone", "revoked"),
            ]),
            serde_json::json!([waiting("ch-cc33", "5150", "Stranger")]),
        );
        let name = |target: Target<'_>| match target {
            Target::Device(device) => device.id.clone(),
            Target::Account(account) => account.id.clone(),
            Target::Waiting(request) => request.id.clone(),
        };
        assert_eq!(name(resolve("e2a", &devices, &channels).unwrap()), "e2a1000000000001");
        assert_eq!(name(resolve("Ana's phone", &devices, &channels).unwrap()), "e2a1000000000001");
        assert_eq!(name(resolve("4217", &devices, &channels).unwrap()), "ch-aa11");
        assert_eq!(name(resolve("Ana T", &devices, &channels).unwrap()), "ch-aa11");
        assert_eq!(name(resolve("Stranger", &devices, &channels).unwrap()), "ch-cc33");
        assert_eq!(name(resolve("ch-cc", &devices, &channels).unwrap()), "ch-cc33");
        // Removed and expired ones are not candidates.
        assert!(resolve("Old tablet", &devices, &channels).is_err());
        assert!(resolve("Tray", &devices, &channels).is_err());
        assert!(resolve("Gone", &devices, &channels).is_err());
        // "e2" names the active device only: the removed one does not count.
        assert_eq!(name(resolve("e2", &devices, &channels).unwrap()), "e2a1000000000001");
        let error = resolve("ch-", &devices, &channels).unwrap_err();
        assert!(error.to_string().contains("more than one"), "{error}");
        let error = resolve("nobody", &devices, &channels).unwrap_err();
        assert!(error.to_string().contains("nothing paired"), "{error}");
    }

    /// `pair list` shows devices, chat accounts and waiting accounts,
    /// escaping names a device or a platform supplied.
    #[test]
    fn list_shows_everything_paired_and_waiting() {
        let empty = channels(serde_json::json!([]), serde_json::json!([]));
        assert!(listing(&[], &empty).starts_with("Nothing is paired."));
        let devices = [device("e2a1000000000001", "Ana's\u{1b}[2J phone", "active")];
        let channels = channels(
            serde_json::json!([account("ch-aa11", "4217", "Ana T", "revoked")]),
            serde_json::json!([waiting("ch-cc33", "5150", "Stranger")]),
        );
        let text = listing(&devices, &channels);
        assert!(text.starts_with("ID "), "{text}");
        assert!(!text.contains('\u{1b}'), "{text}");
        assert!(text.contains("e2a1000000000001") && text.contains("paired"), "{text}");
        assert!(text.contains("Ana T (telegram)") && text.contains("removed"), "{text}");
        assert!(text.contains("Stranger on telegram asked to pair"), "{text}");
        assert!(text.contains("adk-rust pair revoke ch-cc33"), "{text}");
    }

    /// A kernel that cannot list its chat accounts is reported as failing, not as having
    /// none: `pair list` and `pair revoke` both answer its error.
    #[tokio::test]
    async fn a_failed_chat_account_listing_is_reported() {
        use axum::http::StatusCode;
        use axum::routing::get;

        let app = axum::Router::new()
            .route(
                "/api/kernel/devices",
                get(|| async { axum::Json(serde_json::json!({ "devices": [] })) }),
            )
            .route(
                "/api/kernel/channels",
                get(|| async { (StatusCode::INTERNAL_SERVER_ERROR, "the binding store is down") }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = ControlClient::http(base);

        let error = list(&client).await.expect_err("pair list reports the failure");
        assert!(error.to_string().contains("500"), "{error}");
        let error = revoke(&client, "Ana").await.expect_err("pair revoke reports the failure");
        assert!(!error.to_string().contains("nothing paired"), "{error}");
    }
}
