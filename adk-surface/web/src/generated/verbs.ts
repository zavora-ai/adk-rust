// Generated from adk-surface's screen-action types by `scripts/check-surface-api.sh --write`.
// Do not edit: CI fails when this file drifts from the Rust source (spec 3.3 §6).

/** Something in the caller's Coming up, and the times it may move to (spec 3.3 §5.1). */
export interface ComingUpItem {
  /** The area it is in. */
  area: string;
  /** When it wakes, when it waits for one time. */
  at?: string | null;
  /** When it wakes, as a person reads it: "18:00", "Tue 18:00"; empty without one time. */
  atText: string;
  /** The times it may move to: an hour later, the same time tomorrow — those before it ends. */
  choices: SnoozeChoice[];
  /** When it ends if it has not run. */
  ends?: string | null;
  /** The agenda item's id. */
  id: string;
  /** Its title. */
  title: string;
  /** Why it cannot be snoozed, in plain words; absent when it can. */
  whyNot?: string | null;
}

/** What a screen action acts on (spec 3.3 §2 item 7). This wave allows only Coming up. */
export type ObjectKind = "comingUp";

/** One thing an action acts on (spec 3.3 §2 item 7). */
export interface ObjectRef {
  /** Its id: the agenda item's id for Coming up. */
  id: string;
  /** What kind of thing. */
  kind: ObjectKind;
}

/**
 * How an input was given (spec 3.3 §6, §11): every command carries one, and it reaches the
 * audit record, so "What your agents did" can say "from Telegram".
 */
export type Origin = {
  kind: "keyboard";
} | {
  kind: "pointer";
} | {
  kind: "touch";
} | {
  kind: "voice";
} | {
  /** The channel's name, such as `Telegram`. */
  channel: string;
  kind: "chat";
} | {
  /** The requesting agent. */
  agent: string;
  kind: "agent";
};

/** One time an item may move to (spec 3.3 §5.1). */
export interface SnoozeChoice {
  /** The new time as a person reads it: "19:00", "Tue 18:00". */
  atText: string;
  /** "1 hour later" or "Tomorrow, same time". */
  label: string;
  /** The new time, the `to` of `snooze`. */
  to: string;
  /** It is the same time tomorrow. */
  tomorrow: boolean;
}

/**
 * The surface a person uses (spec 3.3 §5, §7.2).
 * 
 * What a surface may decide is fixed by §7.2 and computed by
 * `rules::allowed_answers`; a surface never grants more
 * than the web home.
 */
export type SurfaceKind = "web" | "terminal" | "phone" | "tray" | "chat" | "voice";

/**
 * `POST /surface/verbs/call` body: the action, and the surface it was given on and how. The
 * caller is the server's; a declared surface or origin only says where the person was.
 */
export interface VerbCallRequest {
  /** The action. */
  intent: VerbIntent;
  /** How the input was given. */
  origin: Origin;
  /** The surface; defaults to the web home. */
  surface?: SurfaceKind;
}

/** What an action did (spec 3.3 §2 item 7): shown once it ran, never before. */
export interface VerbDone {
  /** What a read returned, or the changed thing; `null` when nothing more. */
  result: unknown;
  /** What it acted on: `agenda:<id>`. */
  target?: string | null;
  /** What happened, in plain words: "Moved News digest to 19:00". */
  text: string;
  /** What it acted on, in plain words: the item's title. */
  title?: string | null;
  /** The action. */
  verb: string;
}

/**
 * A request to take an action (spec 3.3 §2 item 7): the shape of agentrix's `VerbIntent`,
 * re-declared. It carries no origin and no surface: those come from the server-authenticated
 * `Caller`.
 * 
 * The arguments are the action's own; an object the action acts on fills the argument
 * `item` when the arguments leave it out.
 */
export interface VerbIntent {
  /** Its arguments. */
  args?: unknown;
  /** What it acts on. */
  objects?: ObjectRef[];
  /** The record's target, `agenda:<id>`; filled from `objects` when absent. */
  target?: string | null;
  /** The action's name. */
  verb: string;
}

/** One action of the catalogue, as `GET /surface/verbs` lists it (spec 3.3 §6.1). */
export interface VerbView {
  /** Agents may ask for it; it always waits for a person when it changes something. */
  agentsAsk: boolean;
  /** What it does. */
  description: string;
  /** What it changes: `read`, `write_local`, … */
  effect: string;
  /** The button's words. */
  label: string;
  /** The name. */
  name: string;
  /** What it acts on. */
  objects: ObjectKind[];
  /** It only delays work, so it still runs while everything is stopped. */
  onlyDelays: boolean;
  /** It can be changed back. */
  reversible: boolean;
}

/** `GET /surface/verbs` answer (spec 3.3 §2 item 7). */
export interface VerbsResponse {
  /** The caller's Coming up, soonest first, with the times each item may move to. */
  comingUp: ComingUpItem[];
  /** The actions this home offers. */
  verbs: VerbView[];
}
