// Generated from the Rust types of adk-surface by `scripts/check-surface-api.sh --write`.
// Do not edit: CI fails when this file drifts from the Rust source (spec 3.3 §6).

/** An agent as a decision or a thread names it. */
export interface AgentRef {
  /** The avatar. */
  avatar: Avatar;
  /** The name to show, such as "Finance". */
  displayName: string;
  /** The agent's name in the runtime. */
  name: string;
}

/** One agent in the roster (spec 3.3 §5.1, §8.1). */
export interface AgentRow {
  /** The agent. */
  agent: AgentRef;
  /** The session of its latest work, to open its thread. */
  sessionId?: string | null;
  /** What it is doing. */
  status: AgentStatus;
  /** The status word: "Working", "Waiting for you", "Asleep until 07:00", "Done", "Failed". */
  statusText: string;
  /** When it wakes, while asleep with a wake time. */
  wakeAt?: string | null;
}

/** What an agent is doing (spec 3.3 §8.1). */
export type AgentStatus = "working" | "waitingForYou" | "asleep" | "done" | "failed";

/** `GET /surface/agents` answer. */
export interface AgentsResponse {
  /** The caller's agents. */
  agents: AgentRow[];
}

/** The answers a surface may give one decision (spec 3.3 §7.2). */
export interface Allowed {
  /** A spoken "allow once" takes effect after this many seconds unless cancelled. */
  cancelWindowSecs?: number | null;
  /** Why this surface cannot approve, in plain words, when it cannot. */
  note?: string | null;
  /** Some answers are only in the home: show "Open in home". */
  openInHome: boolean;
  /** The answers, in the order of §7.1: allow once, always allow, edit, deny. */
  options: AnswerOption[];
}

/** An answer to a decision (spec 3.3 §8.2). */
export type Answer = "allowOnce" | "alwaysAllow" | "edit" | "deny";

/** One answer a surface may offer, with its label (spec 3.3 §8.2). */
export interface AnswerOption {
  /** The answer. */
  answer: Answer;
  /** The button's words. */
  label: string;
  /** The call must be expanded first: it is shown truncated. */
  needsExpanded: boolean;
  /** The person must confirm it is them first (high risk on the web home and the phone). */
  needsPresence: boolean;
}

/** An agent's avatar: a rounded monogram in the agent's own colour (spec 3.3 §9). */
export interface Avatar {
  /** The agent's colour, `#RRGGBB`. */
  color: string;
  /** One or two letters. */
  monogram: string;
}

/** "While you were away" (spec 3.3 §5.1, §8.1). */
export interface Briefing {
  /** One line per event, oldest first. */
  lines: BriefingLine[];
  /** Since when. */
  since: string;
  /** "Since 22:04". */
  sinceText: string;
  /** What was spent today. */
  spend?: SpendView | null;
  /** "While you were away". */
  title: string;
  /** Decisions waiting. */
  waiting: number;
}

/** One line of "While you were away", from one `/proc` ledger row (spec 3.3 §5.1, §8.1). */
export interface BriefingLine {
  /** The agent. */
  agent: string;
  /** When. */
  at: string;
  /** Local `HH:MM`. */
  atText: string;
  /** The pill, when the line has one. */
  pill?: Pill | null;
  /** The pill's words. */
  pillText?: string | null;
  /** The ledger row it came from: its record. */
  seq: number;
  /** The sentence. */
  text: string;
}

/** The exact call a decision is about (spec 3.3 §7.1). */
export interface CallView {
  /** Its arguments in full, as the inbox shows them to approvers (secrets removed). */
  args: unknown;
  /** The call as text, `send_email(to: "…", subject: "…")`, cut short when long. */
  text: string;
  /** The tool. */
  tool: string;
  /** `text` is cut short: the call must be expanded before it can be approved. */
  truncated: boolean;
}

/** Where a decision is in its life. */
export type CardStatus = "waiting" | "decided";

/**
 * A command from a surface (spec 3.3 §6). Every command runs as an authenticated
 * `Caller`, whose subject, surface and `Origin` reach its audit record.
 */
export type Command = {
  /** The answer. */
  answer: Answer;
  /** The card's `id`. */
  decisionId: string;
  /** The changed arguments of an `edit`. */
  editedArgs?: unknown;
  /** The surface showed the call in full (§7.1). */
  expanded?: boolean;
  /** Why, for a `deny`; reported to the agent. */
  reason?: string | null;
  type: "decide";
  /** The card's `version`. */
  version: number;
} | {
  /** The decision. */
  decisionId: string;
  type: "cancelAnswer";
} | {
  /** The session. */
  sessionId: string;
  /** The message. */
  text: string;
  type: "send";
} | {
  /** The session. */
  sessionId: string;
  /** The message. */
  text: string;
  type: "queue";
} | {
  /** Why. */
  reason?: string | null;
  type: "stopAll";
} | {
  type: "resumeAll";
} | {
  /** The mode. */
  mode: Mode;
  /** The session. */
  sessionId: string;
  type: "setMode";
};

/** What a command did (spec 3.3 §6). */
export type CommandOutcome = {
  /** The decision after the answer. */
  card: DecisionCard;
  type: "answered";
} | {
  /** The decision. */
  card: DecisionCard;
  /** Who answered, where, how and when. */
  decided: Decided;
  type: "alreadyDecided";
} | {
  /** The decision now. */
  card: DecisionCard;
  type: "changed";
} | {
  /** When it takes effect. */
  cancelUntil: string;
  /** The decision. */
  decisionId: string;
  /** "Allowing once in 5 seconds". */
  text: string;
  type: "pending";
} | {
  /** The decision. */
  decisionId: string;
  type: "answerCancelled";
} | {
  /** The session. */
  sessionId: string;
  type: "sent";
} | {
  /** Its place in the queue, from 1. */
  position: number;
  /** The session. */
  sessionId: string;
  /** "Sent after this card is answered". */
  text: string;
  type: "queued";
} | {
  /** "All agents stopped". */
  text: string;
  type: "stopped";
} | {
  type: "resumed";
} | {
  /** The mode now. */
  mode: ModeView;
  type: "modeSet";
};

/** `POST /surface/commands` body: the command, the surface it was given on and how. */
export interface CommandRequest {
  /** The command. */
  command: Command;
  /** How the input was given. */
  origin: Origin;
  /** The surface; defaults to the web home. */
  surface?: SurfaceKind;
}

/**
 * A decision answered, here or elsewhere (spec 3.3 §8.1 "Decided", §11 S-I2): who answered,
 * where and how, and when.
 */
export interface Decided {
  /** The answer. */
  answer: DecidedAnswer;
  /** When. */
  at: string;
  /** Local `HH:MM`. */
  atText: string;
  /** Who answered: "You" for the caller, else the approver's name; empty when nobody did. */
  by: string;
  /** The caller answered. */
  byYou: boolean;
  /** "You said yes in Telegram at 07:31". */
  text: string;
  /** Where and how, when the answer came through a surface. */
  via?: Via | null;
}

/** How a decision ended. */
export type DecidedAnswer = "yes" | "no" | "edited" | "replied" | "ended" | "unknown";

/**
 * A decision, drawn the same way on every surface from its inbox entry, never from agent
 * output (spec 3.3 §7.1, §11 S-I1).
 */
export interface DecisionCard {
  /** Who asks. */
  agent: AgentRef;
  /** The answers this surface may give; empty once decided. */
  allowed: Allowed;
  /** "Also on your phone and in Telegram". */
  alsoShown?: string | null;
  /** The exact call. */
  call: CallView;
  /** The answer, once decided. */
  decided?: Decided | null;
  /** What will happen, in plain words. */
  heading: string;
  /** The inbox entry: what a command answers. Never shown. */
  id: string;
  /** The call spends money. */
  money: boolean;
  /** Whether it can be taken back. */
  reversibility: Reversibility;
  /** "Can't be unsent". */
  reversibilityText: string;
  /** The risk. */
  risk: Risk;
  /** "Medium risk". */
  riskText: string;
  /** The session the call belongs to. */
  sessionId: string;
  /** "since 18:00". */
  sinceText: string;
  /** Waiting or decided. */
  status: CardStatus;
  /** The pill: "Waiting for you" or "Decided". */
  statusText: string;
  /** How it got here. */
  timeline: TimelineStep[];
  /** The version the surface saw, which its answer carries (S-I2). Never shown. */
  version: number;
  /** Since when it waits. */
  waitingSince: string;
}

/** `GET /surface/decisions` answer. */
export interface DecisionsResponse {
  /** The open decisions, oldest first. */
  decisions: DecisionCard[];
}

/** One event of a session's store, numbered by its position there (spec 3.3 §6). */
export interface FeedEntry {
  /** When. */
  at: string;
  /** Local `HH:MM`. */
  atText: string;
  /** What it shows; empty for events with nothing to show. */
  items: ThreadItem[];
  /**
   * The event's position in the session store, from 1: the `Last-Event-ID` a surface
   * resumes from.
   */
  seq: number;
}

/** One frame of a session's feed (spec 3.3 §6): the snapshot, then the live tail. */
export type FeedFrame = {
  /** The entry. */
  entry: FeedEntry;
  type: "entry";
} | {
  /** The open decisions of the session. */
  cards: DecisionCard[];
  type: "decisions";
} | {
  /** How many events the store holds now. */
  len: number;
  type: "reset";
};

/** How much agents act on their own (spec 3.3 §8.1). */
export type Mode = "justLook" | "suggest" | "askFirst" | "goAhead";

/** The mode with its words. */
export interface ModeView {
  /** The setting's name: "Ask first". */
  label: string;
  /** The mode. */
  mode: Mode;
  /** As a status: "Asks first". */
  status: string;
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

/** A pill on a briefing line (spec 3.3 §5.1 Today). */
export type Pill = "done" | "failed" | "waitingForYou" | "youDecided" | "suggestion" | "question";

/** Quiet time (spec 3.3 §8.1). */
export interface QuietView {
  /** Quiet time holds now. */
  active: boolean;
  /** Local end, `HH:MM`. */
  end: string;
  /** Local start, `HH:MM`. */
  start: string;
  /** "Quiet from 22:00", or "Quiet until 07:00" while it holds. */
  text: string;
}

/** Whether a call can be taken back (spec 3.3 §7.1). */
export type Reversibility = "cantBeUnsent" | "cantBeUndone";

/** How much harm a call can do, as approvers see it (spec 3.3 §7.1). */
export type Risk = "low" | "medium" | "high";

/** Who wrote a message in a thread. */
export type Speaker = "you" | "agent";

/** Today's spend against the daily limit (spec 3.3 §8.1). */
export interface SpendView {
  /** The daily spending limit, micro-USD, when set. */
  limitMicroUsd?: number | null;
  /** Spent today, micro-USD. */
  spentMicroUsd: number;
  /** "$1.84 of $5.00 today", or "$1.84 today" with no limit. */
  text: string;
}

/**
 * The status line every surface shows (spec 3.3 §5.1, §6): the mode word, spend against the
 * daily limit, quiet time and the count of decisions.
 */
export interface StatusLine {
  /** Decisions waiting for the caller: the filled badge. */
  decisions: number;
  /** The mode, when the source runs a harness. */
  mode?: ModeView | null;
  /** Quiet time, when set. */
  quiet?: QuietView | null;
  /** Spend, when the source knows it. */
  spend?: SpendView | null;
  /** Everything is stopped. */
  stopped: boolean;
  /** "All agents stopped", while stopped. */
  stoppedText?: string | null;
}

/**
 * The surface a person uses (spec 3.3 §5, §7.2).
 * 
 * What a surface may decide is fixed by §7.2 and computed by
 * `rules::allowed_answers`; a surface never grants more
 * than the web home.
 */
export type SurfaceKind = "web" | "terminal" | "phone" | "tray" | "chat" | "voice";

/** Query of the reads: the surface the answers are computed for. */
export interface SurfaceQuery {
  /** `GET .../feed`: resume after this `seq` when no `Last-Event-ID` is sent. */
  after?: number | null;
  /**
   * `GET /surface/status`: the session whose mode to show; `GET /surface/decisions`: only
   * that session's decisions.
   */
  session?: string | null;
  /** `GET /surface/briefing`: since when. */
  since?: string | null;
  /** The surface; defaults to the web home. */
  surface?: SurfaceKind | null;
}

/**
 * One thing in a thread (spec 3.3 §5.1): a message, something an agent did, or a decision,
 * which links to its inbox entry and is drawn from the card (§11 S-I1).
 */
export type ThreadItem = {
  /** The author's name: the agent's display name, or "You". */
  author: string;
  /** The person or an agent. */
  from: Speaker;
  kind: "message";
  /** The text, as data. */
  text: string;
} | {
  /** The call, to match its result. */
  callId?: string | null;
  kind: "activity";
  /** "Read file timesheet-october.csv". */
  text: string;
  /** The tool. */
  tool: string;
} | {
  /** The call it answers. */
  callId?: string | null;
  kind: "result";
  /** It succeeded. */
  ok: boolean;
  /** "Done" or "Failed". */
  text: string;
  /** The tool. */
  tool: string;
} | {
  /** The inbox entry, when the call reached the inbox. */
  decisionId?: string | null;
  kind: "decision";
  /** "Waiting for you". */
  text: string;
  /** The tool. */
  tool: string;
};

/** `GET /surface/sessions/{session}/thread` answer. */
export interface ThreadResponse {
  /** Every event of the session store, numbered by position from 1. */
  entries: FeedEntry[];
  /** The session. */
  sessionId: string;
}

/** One step of how a decision got here (spec 3.3 §7.1). */
export interface TimelineStep {
  /** The step the decision is at now. */
  current: boolean;
  /** "Asked at 18:00", "Waited through quiet time", "Waiting for you". */
  text: string;
}

/**
 * Where and how an answer was given, as a decision answered elsewhere shows it
 * (spec 3.3 §8.1 "Decided").
 */
export interface Via {
  /** How the answer was given. */
  origin: Origin;
  /** The surface. */
  surface: SurfaceKind;
}
