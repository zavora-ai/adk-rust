// Generated from the Rust types of adk-surface by `scripts/check-surface-api.sh --write`.
// Do not edit: CI fails when this file drifts from the Rust source (spec 3.3 §6).

/**
 * The definition on the decision that adds a suggested agent (spec 3.3 §5.7 "The decision
 * that adds an agent", S-I13): every field, from the inbox entry the kernel wrote, never from
 * the model.
 */
export interface AgentCardView {
  /** What it can do, in plain words. */
  abilities: string[];
  /** The agent it would add. */
  agent: AgentRef;
  /** What it would start with: "Nothing yet". */
  allowedWithoutAsking: string;
  /** "It can do less than ADK-Rust: no shell, no web". */
  compared?: string | null;
  /** "What it does", in one line. */
  description: string;
  /** The suggestion, for **Change it first**. */
  draft: AgentDraft;
  /** The id the suggested agent would get: what **Change it first** replaces. Never shown. */
  id: string;
  /** The instructions, quoted, cut short past the call limit. */
  instructions: string;
  /** The instructions are cut short: they must be expanded before the agent can be added. */
  instructionsTruncated: boolean;
  /**
   * The definition, one line per field: area, how much it asks first, what it runs on,
   * files, shell, web, packs, drivers, the daily limit.
   */
  lines: CardLine[];
}

/**
 * What a person may choose for an agent on this kernel (spec 3.3 §5.7, decision 21): the
 * models of the providers whose key the kernel has, the packs, the areas and the colours.
 */
export interface AgentChoices {
  /** The areas. */
  areas: AreaChoice[];
  /** The eight colours. */
  colors: ColorChoice[];
  /** The models, the primary agent's first. */
  models: ModelView[];
  /** The packs. */
  packs: string[];
}

/** An agent's colour: one of the eight avatar colours, never a free value (spec 3.3 §5.7). */
export type AgentColor = "orange" | "green" | "blue" | "violet" | "slate" | "amber" | "teal" | "pink";

/** One agent a person added (spec 3.3 §5.7): what **Settings** shows. */
export interface AgentDetail {
  /** The agent as threads and decisions name it. */
  agent: AgentRef;
  /** Why the kernel cannot build it: "No key for Anthropic". */
  cantStart?: string | null;
  /** When it was last changed. */
  changedAt: string;
  /** Its definition. */
  draft: AgentDraft;
  /** Its kernel target, for commands. Never shown. */
  id: string;
  /** How much it asks first, with its words. */
  mode: ModeView;
  /** "Added by you on 25 Oct", or "Suggested by ADK-Rust, added by you on 25 Oct". */
  originText: string;
  /** Where it is in its life. */
  state: AgentLife;
  /** The version a change is made against. Never shown. */
  version: number;
}

/**
 * An agent's definition as **Set it up yourself** and **Settings** edit it, in the order of
 * spec 3.3 §5.7 "The definition".
 */
export interface AgentDraft {
  /** Its area. */
  area: string;
  /** Its colour. */
  color: AgentColor;
  /** Its daily spending limit, micro-USD. */
  dailyLimitMicroUsd?: number | null;
  /** What it does, in one line. */
  description: string;
  /** Its name: "Finance". */
  displayName: string;
  /** How it works. */
  instructions: string;
  /** How much it asks first. */
  mode: Mode;
  /** What it runs on; none for the primary agent's model. */
  model?: ModelView | null;
  /** What it can use. */
  uses: UsesDraft;
}

/** Where an agent is in its life, as a surface shows it (spec 3.3 §5.7, §8.1). */
export type AgentLife = "suggested" | "active" | "paused" | "removing" | "removed" | "refused";

/**
 * **Here is what Finance can do** (spec 3.3 §5.7): what a change lets an agent do, its risk,
 * and the digest the change is confirmed with.
 */
export interface AgentPreview {
  /** What it can do, one plain sentence each. */
  abilities: string[];
  /** The change: `create`, `change`, `pause`, `resume` or `remove`. */
  action: string;
  /** The first six characters of the digest: the terminal's `--confirm` code. */
  code: string;
  /** The digest the change applies with. Never shown. */
  digest: string;
  /** What removing it ends. */
  ended?: EndedWork | null;
  /** The fields a change changes. */
  fields?: string[];
  /** "Here is what Finance can do". */
  heading: string;
  /** The agent's kernel target; for an agent not added yet, the id it would get. Never shown. */
  id: string;
  /** It can spend money. */
  money: boolean;
  /** Confirming needs presence: "Confirm it's you" in the home, or a paired local terminal. */
  needsPresence: boolean;
  /** "Now": the definition after it. */
  now?: AgentDraft | null;
  /** The risk of the result. */
  risk: Risk;
  /** "Medium risk". */
  riskText: string;
  /** "Was": the definition before a change. */
  was?: AgentDraft | null;
  /**
   * The change lets the agent do more: it needs what adding it at that risk needs, and it
   * first ends the agent's waiting decisions.
   */
  widens: boolean;
}

/** What `POST /surface/agents/preview` previews (spec 3.3 §5.7 "Show, then confirm"). */
export type AgentPreviewRequest = {
  action: "create";
  /** The definition. */
  draft: AgentDraft;
  /** The suggestion it replaces, when it came from **Change it first**. */
  replaces?: string | null;
} | {
  action: "change";
  /** The agent. */
  agent: string;
  /** The definition after the change. */
  draft: AgentDraft;
  /** The version seen. */
  version: number;
} | {
  action: "remove";
  /** The agent. */
  agent: string;
  /** The version seen. */
  version: number;
};

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
  /**
   * An agent the person added: it has settings beyond its mode and rules (spec 3.3 §5.7).
   * Absent when false.
   */
  added?: boolean;
  /** The agent. */
  agent: AgentRef;
  /** Its area, for an agent the person added (spec 3.3 §5.7). */
  area?: string | null;
  /** The caller may change it here (spec 3.3 §7.2). Absent when false. */
  canChange?: boolean;
  /** How much it asks first, for an agent the person added (spec 3.3 §5.7). */
  mode?: ModeView | null;
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
export type AgentStatus = "working" | "waitingForYou" | "asleep" | "done" | "failed" | "ready" | "paused" | "cantStart";

/** `GET /surface/agents` answer. */
export interface AgentsResponse {
  /** The caller's agents. */
  agents: AgentRow[];
  /**
   * The roster ends with **New agent**: the source keeps agents a person adds and the
   * caller may add one here (spec 3.3 §5.7). Absent when false.
   */
  canAdd?: boolean;
  /**
   * The agent a message goes to when the person names none: the tray's "Ask Finance"
   * (spec 3.3 §5.5). Absent when the source has no single primary agent.
   */
  primary?: AgentRef | null;
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

/**
 * `POST /surface/decisions/{id}/answer` body: an answer from a paired device, at the version
 * it saw (spec 3.3 §7.2, S-I2).
 */
export interface AnswerRequest {
  /** The answer. */
  answer: Answer;
  /** The changed arguments of an `edit`. */
  editedArgs?: unknown;
  /** The device showed the call in full (§7.1). */
  expanded?: boolean;
  /** Why, for a `deny`; reported to the agent. */
  reason?: string | null;
  /** The card's `version`. */
  version: number;
}

/** An area a person may put an agent in. */
export interface AreaChoice {
  /** The drivers mounted for it. */
  drivers: string[];
  /** The area's id. */
  id: string;
  /** Its mode: an agent there asks at least this much. */
  mode: ModeView;
  /** Its name: "Home". */
  name: string;
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
  /**
   * The source keeps an activity record. Without one, `lines` is empty whatever ran, so a
   * surface says nothing is recorded rather than that nothing happened.
   */
  recorded: boolean;
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

/** One line of an agent's definition on a card: "Area · Home". */
export interface CardLine {
  /** "Runs on". */
  label: string;
  /** The value is a technical name, shown in mono. Absent when false. */
  mono?: boolean;
  /** "claude-sonnet-5-5". */
  value: string;
}

/** Where a decision is in its life. */
export type CardStatus = "waiting" | "decided";

/** A colour a person may choose. */
export interface ColorChoice {
  /** The colour. */
  color: AgentColor;
  /** What is drawn, `#RRGGBB`. */
  hex: string;
  /** Its name: "Green". */
  label: string;
}

/**
 * A command from a surface (spec 3.3 §6). Every command runs as an authenticated
 * `Caller`, whose subject, surface and `Origin` reach its audit record.
 * 
 * §6 also names "steer", a message that reaches a turn while it runs. No runtime under the
 * engine takes input in the middle of a turn, so there is no steer command: a message sent
 * during a turn is the session's next message (`Send`), and
 * `StopAll` stops a turn that has gone wrong.
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
} | {
  /** The agent: a roster row's name. */
  agent: string;
  /** The new session. */
  sessionId: string;
  /** The first message. */
  text: string;
  type: "newThread";
} | {
  /** The digest of the preview the person confirmed. */
  digest: string;
  /** The definition. */
  draft: AgentDraft;
  /** The suggestion **Change it first** replaces, by the agent id it would get. */
  replaces?: string | null;
  type: "addAgent";
} | {
  /** The agent. */
  agent: string;
  /** The digest of the preview the person confirmed. */
  digest: string;
  /** The definition after the change. */
  draft: AgentDraft;
  type: "changeAgent";
  /** The version seen. */
  version: number;
} | {
  /** The agent. */
  agent: string;
  type: "pauseAgent";
} | {
  /** The agent. */
  agent: string;
  type: "resumeAgent";
} | {
  /** The agent. */
  agent: string;
  /** The digest of the preview that named what ends. */
  digest: string;
  type: "removeAgent";
  /** The version seen. */
  version: number;
} | {
  /** The agent. */
  agent: string;
  /** The rule. */
  ruleId: string;
  type: "copyRuleToAgent";
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
} | {
  /** The agent now. */
  agent: AgentDetail;
  type: "agentAdded";
} | {
  /** The agent now. */
  agent: AgentDetail;
  type: "agentChanged";
} | {
  /** The agent now. */
  agent: AgentDetail;
  type: "agentPaused";
} | {
  /** The agent now. */
  agent: AgentDetail;
  type: "agentResumed";
} | {
  /** What ended. */
  ended: EndedWork;
  type: "agentRemoved";
} | {
  /** The agent now. */
  agent: AgentDetail;
  /** "Finance was changed elsewhere at 10:01". */
  text: string;
  type: "agentOutOfDate";
} | {
  /** The agent. */
  agent: string;
  /** The rule's words. */
  label: string;
  type: "ruleCopied";
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

/** A passkey the person set up on a computer for the web home. */
export interface ComputerPasskey {
  /** When it was set up. */
  createdAt: string;
  /** Its id, which **Remove** names. */
  id: string;
}

/**
 * `POST /surface/decisions/{id}/confirm/options` answer: the options for
 * `navigator.credentials.get`, single use, for two minutes.
 */
export interface ConfirmOptions {
  /** What the confirmation answers, with the assertion. */
  challengeId: string;
  /** `PublicKeyCredentialRequestOptionsJSON`: user verification required. */
  publicKey: unknown;
}

/**
 * `POST /surface/decisions/{id}/confirm/options` body: the answer the person is about to
 * confirm, bound on the server to this decision, its version and these arguments (spec 3.3
 * §8.2).
 */
export interface ConfirmOptionsRequest {
  /** `allowOnce` or `edit`. */
  answer: Answer;
  /** The changed arguments of an `edit`. */
  editedArgs?: unknown;
  /** The surface showed the call in full (§7.1). */
  expanded?: boolean;
  /** The card's `version`. */
  version: number;
}

/** `POST /surface/decisions/{id}/confirm` body: the passkey's assertion for the bound answer. */
export interface ConfirmRequest {
  /** The answer, when the page sends it again: it must be the bound one. */
  answer?: Answer | null;
  /** The options' `challengeId`. */
  challengeId: string;
  /** The `PublicKeyCredential` as JSON (`credential.toJSON()`). */
  credential: unknown;
  /** The changed arguments, when the page sends them again: they must be the bound ones. */
  editedArgs?: unknown;
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
  /**
   * How far the answer reaches, shown after the text: "done once" for allow once, the
   * standing rule's label for always allow (spec 3.3 §5.4); `None` when no surface
   * recorded the answer.
   */
  scopeText?: string | null;
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
  /** The agent a suggestion would add, shown in place of the call (spec 3.3 §5.7, S-I13). */
  addsAgent?: AgentCardView | null;
  /** Who asks. */
  agent: AgentRef;
  /** The answers this surface may give; empty once decided. */
  allowed: Allowed;
  /** "Also on your phone and in Telegram". */
  alsoShown?: string | null;
  /** The exact call: the details under the plain words. */
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
  /**
   * What the heading alone does not say, in plain words, for a decision the kernel itself
   * asks: who suggests the work and when it runs once allowed, such as "Finance suggests
   * this. Once you say yes, it runs every 5 minutes." (spec 3.3 §7.1, §8.1). Never on a
   * chat card (S-I5).
   */
  summary?: string | null;
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

/** `GET /surface/phone/push/deliveries` answer, for the operator. */
export interface DeliveriesResponse {
  /** The latest acknowledged wake-ups, oldest first. */
  deliveries: Delivery[];
}

/** One wake-up a phone acknowledged, for "Push delivery" (spec 3.3 §5.4). */
export interface Delivery {
  /** The phone. */
  deviceId: string;
  /** The time between, in milliseconds. */
  latencyMs: number;
  /** When the phone acknowledged it. */
  receivedAt: string;
  /** When the kernel sent it. */
  sentAt: string;
}

/** What removing an agent ends, counted (spec 3.3 §5.7 "Pausing and removing"). */
export interface EndedWork {
  /** Decisions waiting for you that end without an answer. */
  decisions: number;
  /** Follow-ups withdrawn. */
  followUps: number;
  /** Routines ended. */
  routines: number;
  /** "Allowed without asking" rules removed. */
  rules: number;
  /**
   * "Removing Finance stops 1 piece of work, ends 2 decisions waiting for you and 1
   * routine. Its threads stay."
   */
  text: string;
  /** Pieces of work stopped. */
  work: number;
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

/** Where an agent's files are: none, read, or read and change (spec 3.3 §5.7). */
export type FileUse = "none" | "read" | "readWrite";

/** One of the three switches of "What it can see" (spec 3.3 §5.6). */
export type InputSource = "mic" | "camera" | "screen";

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

/** A model an agent runs on; never a key (spec 3.3 decision 21). */
export interface ModelView {
  /** The model, shown in mono: `claude-sonnet-5-5`. */
  model: string;
  /** The provider: `anthropic`, `openai`, `gemini`. */
  provider: string;
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

/**
 * `POST /surface/phone/passkey/options` answer: the options for
 * `navigator.credentials.create`, as WebAuthn's JSON form gives them (spec 3.3 §8.2).
 */
export interface PasskeyOptions {
  /**
   * `PublicKeyCredentialCreationOptionsJSON`: a platform authenticator, user verification
   * required, attestation none.
   */
  publicKey: unknown;
  /** What the registration answers, with the credential. */
  registrationId: string;
}

/** `POST /surface/phone/passkey` body: the new passkey. */
export interface PasskeyRegistration {
  /** The `PublicKeyCredential` as JSON (`credential.toJSON()`). */
  credential: unknown;
  /** The options' `registrationId`. */
  registrationId: string;
}

/** A paired device's phone attachments, as "Where you can say yes from" lists them. */
export interface PhoneDevice {
  /** The device. */
  deviceId: string;
  /** It asks for the person's face or fingerprint. */
  passkey: boolean;
  /** It is woken by push. */
  push: boolean;
}

/** `GET /surface/phone/devices` answer, for the operator. */
export interface PhoneDevicesResponse {
  /** The caller's computer passkeys. */
  computerPasskeys: ComputerPasskey[];
  /** Every paired device with a passkey or a push subscription. */
  devices: PhoneDevice[];
}

/**
 * `GET /surface/phone` answer: what the phone needs to subscribe and to confirm it is you
 * (spec 3.3 §5.4, §8.2).
 */
export interface PhoneInfo {
  /**
   * The caller may set up a passkey now: a phone within 10 minutes of pairing without one,
   * or the person at the web home.
   */
  canAddPasskey: boolean;
  /** The caller has a passkey here: this phone's, or for the web home, the person's. */
  passkey: boolean;
  /** This phone is subscribed to wake-ups. */
  push: boolean;
  /** The kernel's push key, which the phone subscribes with (`applicationServerKey`). */
  pushKey: string;
  /** The passkeys' relying party: the home's host. */
  rpId: string;
}

/** A pill on a briefing line (spec 3.3 §5.1 Today). */
export type Pill = "done" | "failed" | "waitingForYou" | "youDecided" | "suggestion" | "question";

/** The keys a browser gives with its push subscription. */
export interface PushKeys {
  /** The subscription's authentication secret, base64url. */
  auth: string;
  /** The subscription's P-256 public key, base64url. */
  p256dh: string;
}

/** `POST /surface/phone/push/received` body: the phone's acknowledgement of a wake-up. */
export interface PushReceivedRequest {
  /** The wake-up's `id`. */
  id: string;
}

/**
 * `PUT /surface/phone/push` body: the phone's push subscription, as `PushSubscription.toJSON()`
 * gives it (spec 3.3 §5.4).
 */
export interface PushSubscriptionRequest {
  /** The push service URL: https, on Apple's, Google's, Mozilla's or Microsoft's service. */
  endpoint: string;
  /** The browser's keys. */
  keys: PushKeys;
}

/** `POST /surface/phone/qr` body: the pairing link to draw, for the operator. */
export interface QrRequest {
  /** The link: it starts with the home's URL. */
  url: string;
}

/** `GET /surface/sessions/{session}/queued` answer (spec 3.3 §7.4 rule 2, S-I4). */
export interface QueuedResponse {
  /**
   * What the caller typed while a decision of the session was open, oldest first; each is
   * sent once the session's decisions are answered.
   */
  queued: string[];
  /** The session. */
  sessionId: string;
}

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
export type Reversibility = "cantBeUnsent" | "cantBeUndone" | "canBeRemoved";

/** How much harm a call can do, as approvers see it (spec 3.3 §7.1). */
export type Risk = "low" | "medium" | "high";

/** Where an agent's commands run: nowhere, in the sandbox, or on this computer. */
export type ShellUse = "none" | "sandboxed" | "host";

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
  /**
   * Minutes east of UTC of the server's time zone now: the kernel reads a quiet window that
   * carries no offset of its own there, so a page in another zone reads it there too
   * (spec 3.3 §5.1).
   */
  hostUtcOffsetMinutes?: number | null;
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

/** `POST /surface/stop` body: **Stop all** from a paired device (spec 3.3 §11 S-I10). */
export interface StopRequest {
  /** Why. */
  reason?: string | null;
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
   * `GET /surface/decisions`: also the decisions answered or ended since then, newest
   * first, after the open ones (spec 3.3 §5.4).
   */
  decidedSince?: string | null;
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
 * `POST /surface/decisions/{id}/take-over` body: the secret typed on a surface, for the card
 * at the version that surface showed (S-I2). Its `Debug` output redacts the secret.
 */
export interface TakeOverRequest {
  /** What the person typed. */
  secret: string;
  /** The surface: the web home (default) or the phone, the two that take over (§8.2). */
  surface?: SurfaceKind;
  /** The card's `version`. */
  version: number;
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

/** What an agent can use (spec 3.3 §5.7 "What it can use"). */
export interface UsesDraft {
  /** The drivers mounted for its area that it may use. */
  drivers: string[];
  /** Its files. */
  files: FileUse;
  /** One folder inside its area's workspace, relative to it; none for the whole workspace. */
  folder?: string | null;
  /** Its packs. */
  packs: string[];
  /** Its shell. */
  shell: ShellUse;
  /** Its web. */
  web: WebUse;
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

/** A text frame the view sends up the voice socket (spec 3.3 §5.6). */
export type VoiceClientMessage = {
  /** The ticket. */
  ticket: string;
  type: "hello";
} | {
  /** On or off. */
  on: boolean;
  /** Which. */
  source: InputSource;
  type: "switch";
} | {
  type: "playbackEnded";
} | {
  type: "end";
};

/** A realtime model the person can talk on (spec 3.3 §5.6). */
export interface VoiceModel {
  /** What a session asks for: `POST /surface/voice/sessions {model}`. */
  id: string;
  /** The sample rate of the PCM16 mono audio the model takes. */
  inputRate: number;
  /** The name the select shows, such as "Gemini Live". */
  name: string;
  /** The sample rate of the PCM16 mono audio the model speaks. */
  outputRate: number;
}

/**
 * `GET /surface/voice` answer: the models the person can talk on, first the default
 * (spec 3.3 §5.6).
 */
export interface VoiceModels {
  /** The agent the person talks with, by its display name: "Talking with Chief of staff". */
  agent: string;
  /**
   * The agent's name, as `GET /surface/agents` lists it: a conversation opens in Threads
   * under it when it ends.
   */
  agentName: string;
  /** The models, the default first. */
  models: VoiceModel[];
}

/**
 * `POST /surface/voice/sessions` answer (spec 3.3 §5.6).
 * 
 * The ticket works once, within 30 seconds, for the caller it was issued to; the view sends
 * it in its first message on the socket, never in a URL.
 */
export interface VoiceTicket {
  /** The sample rate of the audio the view sends up. */
  inputRate: number;
  /** The sample rate of the audio the view plays. */
  outputRate: number;
  /** The voice conversation's session, listed in Threads under the agent. */
  sessionId: string;
  /** The socket's path: `/voice/socket`. */
  socket: string;
  /** The single-use ticket. */
  ticket: string;
}

/** `POST /surface/voice/sessions` body: a ticket for the voice socket (spec 3.3 §5.6). */
export interface VoiceTicketRequest {
  /** The model's `id`. */
  model: string;
  /**
   * Continue this voice conversation instead of starting one: only one of the caller's own
   * voice sessions (§11 S-I8).
   */
  sessionId?: string | null;
}

/**
 * A text frame the server sends down the voice socket: transient voice state only
 * (spec 3.3 §5.6).
 */
export type VoiceUpdate = {
  /** The sample rate of the audio the view sends up. */
  inputRate: number;
  /** The sample rate of the audio the view plays. */
  outputRate: number;
  /** The voice conversation's session. */
  sessionId: string;
  type: "ready";
} | {
  type: "listening";
} | {
  type: "speaking";
} | {
  /** The transcript. */
  text: string;
  type: "heard";
} | {
  /** The card's `id`. */
  decisionId?: string | null;
  type: "prompted";
  /** The card's `version`. */
  version?: number | null;
} | {
  /** When it takes effect. */
  cancelUntil: string;
  /** The decision. */
  decisionId: string;
  /** "Allowing once in 5 seconds". */
  text: string;
  type: "pending";
} | {
  /** The answer was recorded. */
  decided: boolean;
  /** The decision. */
  decisionId: string;
  /** What happened, in plain words, when it was not recorded. */
  text?: string | null;
  type: "outcome";
} | {
  /** The cards' `id`s, oldest first. */
  decisionIds: string[];
  type: "needsScreen";
} | {
  type: "bargeIn";
} | {
  type: "ended";
} | {
  /** The engine's `surface.*` code, or the source's. */
  code: string;
  /** What happened, in plain words. */
  message: string;
  type: "error";
};

/** What of the web an agent reaches. */
export type WebUse = {
  access: "none";
} | {
  access: "listed";
  /** The sites, such as `example.com`. */
  hosts: string[];
} | {
  access: "any";
};
