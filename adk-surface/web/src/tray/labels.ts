/**
 * The tray panel's own words (spec 3.3 §5.5, §8). Every word the engine also uses comes from
 * `generated/words.ts`, so the tray says what every other surface says.
 */
import * as words from "../generated/words";

export const trayLabels = {
  brand: "ADK-Rust",
  running: "Running",
  stopped: words.ALL_AGENTS_STOPPED,
  decisions: words.DECISIONS,
  waiting: (count: number) => (count === 1 ? "1 decision waits" : `${count} decisions wait`),
  nothingWaits: "Nothing is waiting for you.",
  allowOnce: words.ALLOW_ONCE,
  deny: words.DENY,
  openToDecide: "open to decide",
  openInHome: words.OPEN_IN_HOME,
  exactCall: "The exact call",
  changed: "This decision changed since you saw it. Check it again.",
  nowAndNext: "Now and next",
  working: words.WORKING,
  nothingComingUp: "Nothing is planned.",
  ask: (name: string) => `Ask ${name}`,
  messageYourAgents: "Message your agents",
  queued: words.SENT_AFTER_THIS_CARD_IS_ANSWERED,
  openHome: "Open home",
  stopAll: words.STOP_ALL,
  startWithToken: "Start the tray with --token",
  cantReach: "Can't reach your agents right now. Trying again.",
  spendBar: "Spent of today's limit",
};
