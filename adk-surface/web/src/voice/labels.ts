/**
 * The voice view's words (spec 3.3 §5.6, §8). Every word the engine also says comes from
 * `generated/words.ts`; the sentences the engine builds from a card are mirrored here, word
 * for word, from `adk_surface::words`.
 */
import * as words from "../generated/words";

export const voiceLabels = {
  brand: "ADK-Rust",
  talkingWith: "Talking with",
  voiceModel: "Voice model",
  end: words.END,
  listening: words.LISTENING,
  speaking: words.SPEAKING,
  connecting: "Connecting",
  ended: "The conversation ended.",
  stopped: words.ALL_AGENTS_STOPPED,
  cancel: words.CANCEL,
  decideOnScreen: words.DECIDE_ON_SCREEN,
  whatItCanSee: words.WHAT_IT_CAN_SEE,
  nothingShared: "Nothing is shared. Turn on the camera or the screen to show it something.",
  privacyNote: words.VOICE_PRIVACY_NOTE,
  sayAllowOnceOrDeny: words.SAY_ALLOW_ONCE_OR_DENY,
  sayDenyOrDecideOnScreen: words.SAY_DENY_OR_DECIDE_ON_SCREEN,
  exactCall: "The exact call",
  mic: (on: boolean) => (on ? "Mic on" : "Mic off"),
  camera: (on: boolean) => (on ? "Camera on" : "Camera off"),
  screen: (on: boolean) => (on ? "Screen on" : "Screen off"),
  inputs: "What it hears and sees",
  /** The one label voice adds to the shared message box. */
  talk: "Talk by voice",
  noMicrophone: "Voice needs your microphone. Allow it in the browser, then start again.",
  ticketRefused: "The voice conversation could not start. Start again.",
  startAgain: "Start again",
} as const;

/** `words::talking_with`: "Talking with Chief of staff". */
export function talkingWith(agent: string): string {
  return `Talking with ${agent}`;
}

/** `words::heard`: `Heard "allow once"`. */
export function heard(text: string): string {
  return `Heard "${text}"`;
}

/** `words::wants_to`: "Finance wants to send the October invoice to [CLIENT]." */
export function wantsTo(agent: string, heading: string): string {
  const first = heading.charAt(0).toLowerCase();
  return `${agent} wants to ${first}${heading.slice(1)}.`;
}

/** `words::voice_cant_approve`: `"Publish the release" is high risk, so voice can't approve it.` */
export function voiceCantApprove(heading: string, money: boolean): string {
  return `"${heading}" ${money ? "spends money" : "is high risk"}, so voice can't approve it.`;
}

/** `words::allowing_in`: "Allowing once in 5 seconds". */
export function allowingIn(seconds: number): string {
  return seconds === 1 ? "Allowing once in 1 second" : `Allowing once in ${seconds} seconds`;
}
