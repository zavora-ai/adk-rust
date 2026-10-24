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
//! | A card carries the agent, a short summary with every argument value masked, and the risk (S-I5) | The gateway's `DecisionCard`; the adapter adds only the time it was asked and a link to the home |
//! | Chat answers **Allow once** or **Deny**, at low and medium risk, never money or high risk (§7.2, §7.4) | The gateway sets a card's answers and refuses any other; the adapter offers a card's answers as buttons and no others |
//! | An answer carries the version the card showed; a second answer is shown, never retried (S-I2) | The gateway, through `ApprovalService::decide` |
//! | "stop all" stops everything; resuming happens only in the home (S-I10) | The gateway; the adapter lists `/stop` among the bot's commands |
//! | Typed text never answers a decision (S-I4) | The adapter: only a pressed button becomes an answer |
//! | Text from a model, a tool or a person is escaped before display (S-I9) | The adapter |
//!
//! | Platform | Adapter | Transport |
//! |---|---|---|
//! | Telegram | [`TelegramAdapter`] | Bot API long polling (`getUpdates`): no inbound port, so the home stays on loopback (§13 decision 10) |

mod telegram;

pub use telegram::{
    TELEGRAM, TELEGRAM_CHANNEL, TelegramAdapter, TelegramAdapterBuilder, TelegramError,
    TelegramPolling,
};
