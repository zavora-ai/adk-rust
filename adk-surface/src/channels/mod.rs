//! Chat channels (spec 3.3 §5.3, §6.1; feature `channels`): the doorbell and the
//! conversation, through someone else's servers.
//!
//! A chat adapter moves messages; the kernel's channel contract
//! ([`adk_kernel::channels`], §10 K1) owns every rule. The adapter turns the platform's
//! updates into [`ChannelInbound`](adk_kernel::channels::ChannelInbound) values for
//! [`ChannelGateway::receive`](adk_kernel::channels::ChannelGateway::receive), and renders
//! what the gateway sends through [`ChannelSink`](adk_kernel::channels::ChannelSink).
//!
//! | Rule | Where it is kept |
//! |---|---|
//! | Only paired accounts reach an agent; a stranger gets a pairing code and starts nothing (S-I6) | The gateway |
//! | A card carries the agent, a short summary with every argument value masked, and the risk, never the call's arguments (S-I5) | The gateway's `DecisionCard`. The adapter adds the time it was asked and a link to the home; with the approval service at hand it heads the card with the tool's name in plain words ("Send email"), and keeps the kernel's summary under it only when an approval route wrote one |
//! | Chat answers **Allow once** or **Deny**, at low and medium risk, never money or high risk (§7.2, §7.4) | The gateway sets a card's answers and refuses any other; the adapter offers a card's answers as buttons and no others |
//! | An answer carries the version the card showed; a second answer is shown, never retried (S-I2) | The gateway, through `ApprovalService::decide` |
//! | "stop all" stops everything; resuming happens only in the home (S-I10) | The gateway; the adapter lists `/stop` among the bot's commands |
//! | A stranger's messages never hold up a paired person's, "stop all" and answers included (S-I6, S-I10) | The adapter: a paired account's updates are handed over first, a failed update is read again only when it is a paired account's and the failure may clear, and nothing sent to a stranger waits out a rate limit |
//! | Typed text never answers a decision (S-I4) | The adapter: only a pressed button becomes an answer |
//! | Text from a model, a tool or a person is escaped before display (S-I9) | The adapter |
//! | A person's words are only what they saw (S-I6) | The adapter drops the characters Telegram shows as nothing from typed text: controls, zero-width characters, variation selectors and the tag block |
//!
//! | Platform | Adapter | Transport |
//! |---|---|---|
//! | Telegram | [`TelegramAdapter`] | Bot API long polling (`getUpdates`): no inbound port, so the home stays on loopback (§13 decision 10) |

#[cfg(feature = "discord")]
mod discord;
#[cfg(feature = "matrix")]
mod matrix;
#[cfg(any(feature = "whatsapp", feature = "slack", feature = "discord", feature = "matrix"))]
mod shared;
#[cfg(feature = "slack")]
mod slack;
mod telegram;
#[cfg(feature = "whatsapp")]
mod whatsapp;

#[cfg(feature = "discord")]
pub use discord::{
    DISCORD, DISCORD_CHANNEL, DiscordAdapter, DiscordAdapterBuilder, DiscordConnection,
    DiscordError,
};
#[cfg(feature = "matrix")]
pub use matrix::{
    MATRIX, MATRIX_CHANNEL, MatrixAdapter, MatrixAdapterBuilder, MatrixError, MatrixSync,
};
#[cfg(any(feature = "whatsapp", feature = "slack", feature = "discord", feature = "matrix"))]
pub use shared::ChannelConnection;
#[cfg(feature = "slack")]
pub use slack::{
    SLACK, SLACK_CHANNEL, SlackAdapter, SlackAdapterBuilder, SlackConnection, SlackError,
};
pub use telegram::{
    TELEGRAM, TELEGRAM_CHANNEL, TelegramAdapter, TelegramAdapterBuilder, TelegramError,
    TelegramPolling,
};
#[cfg(feature = "whatsapp")]
pub use whatsapp::{
    WHATSAPP, WHATSAPP_CHANNEL, WhatsAppAdapter, WhatsAppAdapterBuilder, WhatsAppError,
    WhatsAppWebhook,
};
