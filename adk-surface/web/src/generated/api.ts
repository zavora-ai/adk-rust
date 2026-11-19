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
  /** "It can do no more than ADK-Rust, and has no shell or web." */
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

/**
 * `POST /surface/agents/confirm/options` body: the change the person is about to confirm —
 * `addAgent` or `changeAgent` with the digest its preview showed — bound on the server to
 * exactly this command (spec 3.3 §7.2: the web home adds an agent at high risk after
 * confirming it is you).
 */
export interface AgentConfirmOptionsRequest {
  /** The change. */
  command: Command;
}

/** One agent a person added (spec 3.3 §5.7): what **Settings** shows. */
export interface AgentDetail {
  /** The agent as threads and decisions name it. */
  agent: AgentRef;
  /** Why the kernel cannot build it: "No key for Anthropic". */
  cantStart?: string | null;
  /** When it was last changed. */
  changedAt: string;
  /**
   * The surface it was last changed on — `web`, `terminal`, `phone` — when the kernel
   * knows it; a change refused as out of date names it.
   */
  changedOn?: string | null;
  /** Its definition. */
  draft: AgentDraft;
  /**
   * What the home lets it do beyond its definition, "On this home it can also:", from the
   * uses the host granted every agent it runs; empty for an agent that does not run, and
   * absent when empty (spec 3.3 §5.7, S-I12, S-I13).
   */
  granted?: GrantedUse[];
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
  /**
   * The agents the person removed, whose threads stay readable through "Removed agents"
   * at the roster's foot (spec 3.3 §5.7). Absent when none.
   */
  removed?: RemovedAgent[];
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

/**
 * The home's answer to an `AppRequest`: the JSON-RPC `result` or `error` the page passes
 * back to the app, and what it shows the person.
 */
export interface AppAnswer {
  /** The JSON-RPC error, when it did not. */
  error?: AppError | null;
  /** What the page shows the person. */
  prompt?: AppPrompt | null;
  /** The JSON-RPC result, when the request succeeded. */
  result?: unknown;
}

/** Why an app was blocked (spec 3.3 §6.1 `genui-apps`). */
export type AppBlockReason = "approvalButtons" | "undeclaredSites" | "thisComputer" | "notAnApp";

/** A JSON-RPC error for an app. */
export interface AppError {
  /** The JSON-RPC code. */
  code: number;
  /** What happened. */
  message: string;
}

/**
 * `POST /surface/genui/sessions/{session}/apps/{call}/message`: the person pressed **Send
 * it** on what an app suggested (spec 3.3 §6.1 `genui-apps`). The home sends it as the
 * person's message, inside an untrusted-content block labelled as the app's suggestion.
 */
export interface AppMessageRequest {
  /** The message the app suggested. */
  message: string;
}

/**
 * What `GET /surface/genui/sessions/{session}/apps/{call}` answers (spec 3.3 §6.1
 * `genui-apps`): the app to frame, why it was blocked, or that it is not shown here.
 */
export type AppOpen = {
  /** The apps origin, which the page frames and nothing else. */
  frameOrigin: string;
  /** The app's page on the apps origin. */
  frameUrl: string;
  /** The tallest the frame grows, in CSS pixels. */
  maxHeight: number;
  /** Whether the app asked for a border around it. */
  prefersBorder: boolean;
  /** The device permissions it asked for, all refused here. */
  refusedPermissions: string[];
  state: "shown";
  /** The arguments of the call that showed it: `ui/notifications/tool-input`. */
  toolInput: unknown;
  /** Its result as an MCP `CallToolResult`: `ui/notifications/tool-result`. */
  toolResult: unknown;
} | {
  /** The technical reason, for the developer panel. */
  detail: string;
  /** Why. */
  reason: AppBlockReason;
  state: "blocked";
  /** What the thread says. */
  text: string;
} | {
  /** The tool's text result. */
  resultText: string;
  state: "elsewhere";
  /** What the thread says. */
  text: string;
};

/**
 * What the page shows a person for an app's request (spec 3.3 §6.1 `genui-apps`): the app
 * only ever asks; the person's own press does what it asks.
 */
export type AppPrompt = {
  kind: "notice";
  /** The notice. */
  text: string;
} | {
  /** "Not now". */
  dismissLabel: string;
  kind: "message";
  /** The message the app suggests. */
  message: string;
  /** "It will be marked as suggested by the app". */
  note: string;
  /** "Send it". */
  sendLabel: string;
  /** "The app wants to send: “Remind me to take an umbrella tomorrow.”" */
  text: string;
} | {
  /** "Cancel". */
  dismissLabel: string;
  kind: "link";
  /** "Open". */
  openLabel: string;
  /** "Open example.com?" */
  text: string;
  /** The link, an `https` or `http` URL. */
  url: string;
} | {
  /** Its contents in base64, when the app sent bytes. */
  blob?: string | null;
  /** "Cancel". */
  dismissLabel: string;
  kind: "download";
  /** Its media type; never one a browser opens as a page. */
  mimeType: string;
  /** The file's name, sanitized. */
  name: string;
  /** "Save". */
  saveLabel: string;
  /** "The Weather app wants to save forecast.csv (2 KB)" */
  text: string;
  /** Its contents as text, when the app sent text. */
  textContent?: string | null;
};

/**
 * A request an app made of the home, as the page relays it: `POST
 * /surface/genui/sessions/{session}/apps/{call}/requests` (spec 3.3 §6.1 `genui-apps`). Only
 * requests with an `id` reach the home; the page answers notifications itself.
 */
export interface AppRequest {
  /**
   * The JSON-RPC method: `ui/initialize`, `tools/call`, `ui/message`, `ui/open-link`,
   * `ui/update-model-context`, `ping`.
   */
  method: string;
  /** Its parameters. */
  params?: unknown;
}

/**
 * An app an MCP server's tool showed in a thread (spec 3.3 §6.1 `genui-apps`): on the normal
 * card background, labelled as unable to approve anything. The page opens it sandboxed on a
 * second origin; the phone, the terminal and chat show its text result instead.
 */
export interface AppView {
  /** The agent whose call showed it. */
  agent: AgentRef;
  /** The server as a person reads it: "Weather". */
  appName: string;
  /** When. */
  at: string;
  /** The call that showed it: the app's id in its session. */
  callId: string;
  /** "Open the home on this computer to see this app": where the app is not shown. */
  elsewhereText: string;
  /** "App from Weather · it can't approve anything". */
  headerText: string;
  /** The tool's text result, which every surface can show. */
  resultText: string;
  /** The MCP server, by its name on this home. */
  server: string;
  /** "app from Weather · open it in the home": the terminal's line. */
  terminalText: string;
  /** The tool that showed it, by its name on the server. */
  tool: string;
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

/** An axis. */
export type Axis = "x" | "y" | "z";

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

/** A camera view. */
export type CameraPreset = "fit" | "front" | "top" | "side" | "iso";

/**
 * A line under a decision's details that a feature adds through a
 * `CardHint`: where the call comes from, with an optional link into the
 * home, such as "From the invoice-run workflow · Open in Runs" (spec 3.3 §7.1). Never on a
 * chat card (S-I5).
 */
export interface CardDetail {
  /**
   * Where the link goes: a path in the home, such as `/ui/runs/r-1`. A renderer follows only
   * a path on its own server.
   */
  href?: string | null;
  /** The words of the link after the line: "Open in Runs". */
  linkText?: string | null;
  /** The line, in plain words: "From the invoice-run workflow". */
  text: string;
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

/**
 * `GET /surface/genui/catalogs` answer (spec 3.3 §6.1 `genui`): the catalogs this home
 * renders, `forms` first, as documents of one A2UI version. In 1.0 an extension catalog's
 * document holds only the components it adds, which a surface mixes with `forms`.
 */
export interface CatalogDocuments {
  /** Each catalog's document, by the A2UI catalog meta-schema of that version. */
  catalogs: unknown[];
  /** The A2UI version of the documents: `1.0` or `v0.9.1`. */
  version: string;
}

/** A catalog this home renders. */
export interface CatalogView {
  /** The components it renders. */
  components: string[];
  /** Its id. */
  id: string;
  /** Its name and version: `forms 1.0`. */
  label: string;
  /**
   * The upstream components it leaves out, because they could fake a decision or cover
   * the home.
   */
  leftOut: string[];
}

/** What a chart's horizontal axis holds. */
export type ChartAxis = {
  /** The categories' names, in order. */
  labels: string[];
  type: "category";
} | {
  type: "number";
} | {
  type: "time";
};

/** How a chart is drawn. */
export type ChartKind = "line" | "area" | "bar" | "scatter" | "pie" | "sparkline";

/** One series of a chart. */
export interface ChartSeries {
  /** Its name, in the legend. */
  name: string;
  /** Its horizontal values on a number or time axis; empty on a category axis. */
  x?: number[];
  /** Its values. */
  y: number[];
}

/** A labelled point on a number or time axis. */
export interface ChartTick {
  /** Where, in the axis's values. */
  at: number;
  /** What it reads. */
  text: string;
}

/** One claim of a protocol. */
export interface Claim {
  /** Its key, such as `version_0_9_1`. */
  id: string;
  /** What it says: "Version 0.9.1", "No approval controls in drawn UI". */
  label: string;
  /** A line on what it covers. */
  note?: string | null;
  /** Where it holds. */
  scope: ClaimScope;
  /** How far the home supports it. */
  status: ClaimStatus;
}

/** Where a claim holds (adk-ui's vocabulary). */
export type ClaimScope = "runtime" | "client" | "transport" | "host" | "security";

/** How far a home supports a claim (adk-ui's vocabulary). */
export type ClaimStatus = "supported" | "partial" | "host_owned" | "candidate" | "planned" | "legacy";

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
 * A message sent while a turn of its session runs reaches that turn (§6 "steer"): the
 * engine offers it through `Source::steer`, the runtime screens it as it screens a turn's
 * own message, and the agent takes it at its next safe point. A turn that ends first hands
 * the message back, and it goes as the session's next message, once.
 * 
 * | Command | A turn of the session runs | Answers |
 * |---|---|---|
 * | `Send` | Offered to the turn; it waits in the session's queue until the turn takes it or hands it back | At once: `Queued` |
 * | `Steer` | The same | Once the message has a fate: `Steered`, or `Queued` when handed back |
 * | `Queue` | Never offered: the session's next message | At once: `Queued` |
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
  type: "steer";
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
} | {
  /** The declared action the person used. */
  actionId: string;
  /** The session the form is in. */
  sessionId: string;
  /** The drawn surface. */
  surfaceId: string;
  type: "submitDrawn";
  /** What the person typed or picked, by the field's data path (`/to`). */
  values?: Record<string, unknown>;
  /** The version the person saw. */
  version: number;
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
  /** The id of the session event that carries the message. */
  eventId: string;
  /** The session. */
  sessionId: string;
  type: "steered";
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
  /** "Finance was changed in the terminal at 10:01". */
  text: string;
  type: "agentOutOfDate";
} | {
  /** The agent. */
  agent: string;
  /** The rule's words. */
  label: string;
  type: "ruleCopied";
} | {
  /** The call asked for. */
  callId: string;
  /** The session. */
  sessionId: string;
  /** The drawn surface. */
  surfaceId: string;
  type: "formSent";
  /** The version sent. */
  version: number;
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
  /**
   * A line under the details from a feature's `CardHint`, such as
   * "From the invoice-run workflow · Open in Runs" (spec 3.3 §7.1). Never on a chat card
   * (S-I5).
   */
  detail?: CardDetail | null;
  /**
   * What an MCP server asks the person, drawn in the home's own controls in place of the
   * call's arguments (MCP elicitation; spec 3.3 §7.2). Never on a chat card (S-I5).
   */
  elicitation?: ElicitationView | null;
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

/** A depth effect. */
export type DepthEffect = "tilt" | "flip" | "layers" | "carousel" | "wall" | "stage";

/**
 * What assistive technology reads for a drawn node (A2UI 1.0 `AccessibilityAttributes`): its
 * label and description resolved and checked like any shown text, how its changes are
 * announced, and whether it is hidden from screen readers (never a field or a button).
 */
export interface DrawnAccessibility {
  /** More it reads after the name: instructions, a format, a result. */
  description?: string | null;
  /** Hidden from screen readers. */
  hidden?: boolean;
  /** A short name, read instead of its visible text. */
  label?: string | null;
  /** How its changes are announced: `polite` or `assertive`; absent is `off`. */
  live?: string | null;
}

/** A declared action of a drawing, as its button shows it. */
export interface DrawnActionView {
  /** The call's arguments: a field's value, or a constant the drawing fixed. */
  args?: DrawnArg[];
  /** The action's id. */
  id: string;
  /** The button's label, agent text checked by rule 5. */
  label: string;
  /** The engine's line under the button: "Sending runs send_invoice and asks you first". */
  note: string;
  /** The tool sending runs. */
  tool: string;
}

/** One argument of a drawn action's call, as a preview of the call shows it. */
export interface DrawnArg {
  /** The argument's name. */
  name: string;
  /** The field whose value it takes, by data path. */
  path?: string | null;
  /** The constant it takes, when it takes no field's value. */
  value?: unknown;
}

/**
 * A chart a component of an extension catalog shows in the home (spec 3.3 §6.1 `genui`,
 * extension catalogs): numbers and words the server already checked by rule 5, never
 * colours, links or markup. The home draws it in its own chart colours.
 */
export interface DrawnChart {
  /** How it is drawn. */
  kind: ChartKind;
  /** The series, at most eight: one per chart colour. */
  series: ChartSeries[];
  /**
   * One sentence that says what the chart shows, for screen readers and the terminal,
   * written by the server: "Line chart of Revenue, 12 points, from 3100 to 9800".
   */
  summary: string;
  /** Labelled points on a number or time axis, such as "10:00" at its epoch milliseconds. */
  ticks?: ChartTick[];
  /** What the horizontal axis holds. */
  x: ChartAxis;
  /** The horizontal axis's name. */
  xLabel?: string | null;
  /** The vertical axis's name. */
  yLabel?: string | null;
}

/**
 * A check a drawn field makes: the catalog's `required`, `regex`, `length`, `numeric`,
 * `email`, `and`, `or` and `not` functions, and the message shown when it fails.
 */
export interface DrawnCheck {
  /** The A2UI function call that must be true, its bindings at data model paths. */
  condition: unknown;
  /** "Enter an email address". */
  message: string;
}

/** Components a home view holds, presented with CSS 3D, as `HomeNode::Depth`. */
export interface DrawnDepth {
  /** How the children are presented. */
  effect: DepthEffect;
  /** A title for each child, for a carousel, wall or stage's controls and the terminal. */
  titles?: string[];
}

/**
 * A component of an extension catalog as a host renderer reads it (spec 3.3 §6.1 `genui`,
 * extension catalogs): its properties with every binding and text function resolved and
 * every string already checked by rule 5. A renderer draws it from these and nothing else,
 * and draws no control: the component only shows.
 */
export interface DrawnExtension {
  /** The catalog's id. */
  catalog: string;
  /**
   * The keys of the nodes its slots hold, in slot order: each is in the subtree of the
   * node's child.
   */
  children?: string[];
  /** The component's type, such as `Metric`. */
  component: string;
  /** Its properties, resolved and checked. */
  props: unknown;
}

/** A field of a drawing: where its value goes, what it takes and what it checks. */
export interface DrawnField {
  /** What it checks before the form is sent, in order. */
  checks?: DrawnCheck[];
  /** What it takes. */
  input: DrawnInput;
  /** The highest value of a slider, or the latest date. */
  max?: unknown;
  /** The lowest value of a slider, or the earliest date. */
  min?: unknown;
  /** The options of a choice. */
  options?: DrawnOption[];
  /**
   * The data model path its value is at, such as `/to`: the key of its value when the
   * form is sent.
   */
  path: string;
  /** The value it starts with. */
  value: unknown;
}

/** The kind of value a drawn field takes. */
export type DrawnInput = "shortText" | "longText" | "number" | "date" | "time" | "dateTime" | "checkbox" | "choice" | "choices" | "slider";

/**
 * What a drawing is: a form a person fills in and sends, a card or table to read, or a
 * screen of catalog components.
 */
export type DrawnKind = "form" | "card" | "table" | "screen";

/**
 * One component of a drawing, resolved for a renderer: templates expanded, every string a
 * person reads already in place (spec 3.3 §7.4 rule 5).
 * 
 * | Component | `text` | `variant` | Other |
 * |---|---|---|---|
 * | `Text` | The text | `h1`–`h5`, `caption`, `body` | — |
 * | `Row`, `Column`, `List` | — | `justify` (Row, Column) or `direction` (List) | `children`; a long `List` has `showAllText` |
 * | `Card` | — | — | `children` |
 * | `Tabs` | — | — | `tabs` titles and one child per tab |
 * | `Divider` | — | `axis` | — |
 * | `Button` | Its label | `primary`, `default`, `borderless` | `action` |
 * | `TextField`, `DateTimeInput`, `CheckBox`, `ChoicePicker`, `Slider` | The label | the catalog's variant | `field` |
 * | `Chart` | — | — | `chart`: a chart an extension component's home view draws |
 * | `Scene` | — | — | `scene`: a 3D scene an extension component's home view draws; its children are its panels, then its text view |
 * | `Depth` | — | — | `depth`: the children an extension component's home view holds, presented with CSS 3D |
 * | An extension component, such as `Metric` | — | — | `extension`, and one child: what the home shows for it |
 */
export interface DrawnNode {
  /**
   * What assistive technology reads for it (A2UI 1.0 `@accessibility`), resolved and
   * checked by rule 5.
   */
  accessibility?: DrawnAccessibility | null;
  /** The declared action a button asks for. */
  action?: string | null;
  /** A chart the home draws natively, from an extension component's home view. */
  chart?: DrawnChart | null;
  /** Its children, in order. */
  children?: DrawnNode[];
  /** The catalog component. */
  component: string;
  /**
   * How the home presents this node's children in depth, from an extension component's
   * home view.
   */
  depth?: DrawnDepth | null;
  /**
   * The extension component this node draws, for a host that draws it with its own
   * renderer; every other surface draws its one child, what the home shows for it.
   */
  extension?: DrawnExtension | null;
  /** A field the person fills in. */
  field?: DrawnField | null;
  /**
   * Unique within the drawing: the component's id, and for a template's copy the list
   * item it was drawn for, `cell-0@/rows/3`.
   */
  key: string;
  /** A 3D scene the home draws natively, from an extension component's home view. */
  scene?: DrawnScene | null;
  /** A long list's "Show all 42", when a renderer cuts it. */
  showAllText?: string | null;
  /** The titles of a `Tabs`' tabs, one per child. */
  tabs?: string[];
  /** What it reads, or its label. */
  text?: string | null;
  /** A style hint from the catalog. */
  variant?: string | null;
}

/** One option of a drawn choice. */
export interface DrawnOption {
  /** What the person reads. */
  label: string;
  /** The value sent. */
  value: string;
}

/** A 3D scene a home view draws, as `HomeNode::Scene`. */
export interface DrawnScene {
  /** Named axes, ticks and a floor grid. */
  axes?: SceneAxes | null;
  /** Where it is seen from. */
  camera: SceneCamera;
  /** Its height in CSS pixels, 200 to 960; the home picks one when it is not given. */
  height?: number | null;
  /** What the scene is, for its summary and default camera. */
  kind: SceneKind;
  /** How it is lit: a preset whose soft light always stays, so nothing goes dark. */
  lighting: SceneLighting;
  /** Up to three more lights. */
  lights?: SceneLight[];
  /** Objects that move. */
  motion?: SceneMotion[];
  /** Its objects, parents before children. */
  objects: SceneObject[];
  /**
   * The held components shown beside objects: written by the server, each the key of one
   * of the node's children.
   */
  panels?: ScenePanel[];
  /** Picking an object fills this field. */
  select?: SceneSelect | null;
  /**
   * One sentence that says what the scene shows, for screen readers and the terminal,
   * written by the server: "3D bar chart of Revenue, 24 bars, from 3 to 98".
   */
  summary: string;
  /**
   * The key of the node's child that says in words what the scene shows, written by the
   * server: shown by "Show as text", where WebGL is missing, and in the terminal.
   */
  textView?: string | null;
  /**
   * What bar and grid heights stand for in the data's own numbers, for the summary and the
   * home's readouts; heights are the data's numbers when not given.
   */
  values?: SceneValues | null;
}

/**
 * A person sent a drawn form: the call they asked for, held as the drawing agent for their
 * decision, or refused by its rules before it was held (spec 3.3 §7.4 rule 5).
 */
export interface DrawnSent {
  /** The action used. */
  actionId: string;
  /** When. */
  at: string;
  /** The call's id: its result names it. */
  callId: string;
  /** The decision it waits on, once held. */
  decisionId?: string | null;
  /** Held for a decision; `false` when the agent's rules refused the call first. */
  held: boolean;
  /** The surface sent. */
  surfaceId: string;
  /** "Sent at 10:03", "The app asked · 10:06", or "Finance can't use send_invoice here". */
  text: string;
  /** The tool asked for. */
  tool: string;
  /** The version sent. */
  version: number;
  /**
   * What asked for it: a form the person sent, or an app from an MCP server (spec 3.3 §6.1
   * `genui-apps`).
   */
  via: SentVia;
}

/**
 * A drawing in a thread (spec 3.3 §6.1 `genui`): drawn on the normal card background,
 * never the decision's, with its strings already resolved on the server.
 */
export interface DrawnSurface {
  /** What its buttons ask for. */
  actions?: DrawnActionView[];
  /** The agent that drew it. */
  agent: AgentRef;
  /** Local `HH:MM` it was drawn at. */
  atText: string;
  /** The render tool's call. */
  callId?: string | null;
  /** The catalog it was checked against, such as `forms 1.0`. */
  catalog: string;
  /**
   * What a newer version says above itself: "Finance changed this form at 10:05", or
   * "Finance removed this at 10:06" for a removal.
   */
  changedText: string;
  /** This version removes the surface. */
  deleted?: boolean;
  /** When it was drawn. */
  drawnAt: string;
  /** "Drawn by Finance · 10:02". */
  drawnText: string;
  /** What this version says once a newer one replaces it: "Earlier version · 10:02". */
  earlierText: string;
  /** What it is. */
  kind: DrawnKind;
  /** What it shows; `None` for a removal. */
  root?: DrawnNode | null;
  /** The surface; a later drawing of it is its next version. */
  surfaceId: string;
  /** The title, agent text checked by rule 5. */
  title?: string | null;
  /** This version. Only the latest version of a surface is live. */
  version: number;
}

/**
 * What an MCP server asks the person while the agent's call waits (MCP elicitation), drawn
 * by the home in its own controls, never by the server: a form whose answer is sent as the
 * decision's edit and checked against the server's schema, or a page the person opens
 * themselves, shown in full and never opened by the home (spec 3.3 §7.2).
 */
export type ElicitationView = {
  /** The fields, in the schema's order. */
  fields: ElicitedField[];
  /** What it asks, in its words. */
  message: string;
  mode: "form";
  /** The server that asks. */
  server: string;
} | {
  /** Its host, shown on its own so a look-alike stands out. */
  host: string;
  /** Why, in its words. */
  message: string;
  mode: "url";
  /** The server that asks. */
  server: string;
  /** The whole address, as sent. */
  url: string;
  /**
   * Why to look twice, when there is a reason: look-alike characters, a name hidden
   * before an `@`, no encryption.
   */
  warning?: string | null;
};

/** One field of an elicitation form. */
export interface ElicitedField {
  /** The value it starts with. */
  default?: unknown;
  /** The schema's description. */
  description?: string | null;
  /** What it takes. */
  kind: ElicitedKind;
  /** What the field is called: the schema's `title`, else its key. */
  label: string;
  /** A text's length bounds, in characters. */
  maxLength?: number | null;
  /** A number's bounds. */
  maximum?: number | null;
  /** A text's length bounds, in characters. */
  minLength?: number | null;
  /** A number's bounds. */
  minimum?: number | null;
  /** Its key in the answer. */
  name: string;
  /** A choice's options. */
  options?: ElicitedOption[];
  /** The answer is refused without it. */
  required: boolean;
}

/** What an elicitation field takes. */
export type ElicitedKind = "text" | "email" | "url" | "date" | "dateTime" | "number" | "integer" | "boolean" | "choice";

/** One option of a choice. */
export interface ElicitedOption {
  /** What the person reads. */
  label: string;
  /** What the answer carries. */
  value: string;
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

/** A drawn form's state after it was sent (spec 3.3 §7.4 rule 5). */
export interface FormState {
  /** The decision it waits on or waited on. */
  decisionId?: string | null;
  /** Its fields can no longer be changed: it was sent and the call was not refused. */
  locked: boolean;
  /** Where it is. */
  status: FormStatus;
  /** The surface. */
  surfaceId: string;
  /** "Sent at 10:03 · waiting for you", "Sent · done at 10:04", … */
  text: string;
  /** The version sent. */
  version: number;
}

/**
 * Where a drawn form is after it was sent, from its decision and its call's result
 * (spec 3.3 §7.4 rule 5).
 */
export type FormStatus = "waiting" | "sent" | "done" | "saidNo" | "failed" | "notRun" | "refused" | "ended";

/** `GET /surface/genui/sessions/{session}/forms` answer (spec 3.3 §7.4 rule 5). */
export interface FormsResponse {
  /** Each form sent, its latest send per version. */
  forms: FormState[];
  /** The session. */
  sessionId: string;
}

/**
 * What `GET /api/surface/genui/capabilities` answers: a table of claims per protocol, and
 * the catalogs this home renders.
 */
export interface GenuiCapabilities {
  /** The catalogs this home renders. */
  catalogs: CatalogView[];
  /** A2UI, AG-UI and MCP Apps, in that order. */
  protocols: ProtocolClaims[];
}

/**
 * Something the home lets every agent on it do beyond its definition, because the host
 * gives each agent it runs the tools for it (spec 3.3 §5.7, S-I12, S-I13): shown under
 * "Here is what Finance can do" as "On this home it can also:". Hosts register each with
 * `SurfaceEngineBuilder::grant`.
 */
export interface GrantedUse {
  /** The feature that grants it, such as `genui`: one line per id. Never shown. */
  id: string;
  /**
   * The modes in which an agent can use it; empty for every mode. A surface lists it for an
   * agent only in one of them, so what an agent's settings say it can do is true (S-I13).
   */
  modes?: Mode[];
  /**
   * What it can do, in plain words: "Ask to change things on your screens — always asks
   * you first".
   */
  text: string;
}

/** One of the three switches of "What it can see" (spec 3.3 §5.6). */
export type InputSource = "mic" | "camera" | "screen";

/** How large a label is. */
export type LabelSize = "small" | "medium" | "large";

/** A kind of light. */
export type LightKind = "point" | "spot" | "directional";

/** A line's arrowheads. */
export type LineEnds = "none" | "end" | "both";

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

/** How an object moves. */
export type MotionKind = "spin" | "bob" | "pulse" | "orbit" | "path";

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

/** One protocol's claims. */
export interface ProtocolClaims {
  /** What this home claims, in order. */
  claims: Claim[];
  /** Its key: `a2ui`, `ag_ui`, `mcp_apps`. */
  id: string;
  /** Its name: `A2UI`, `AG-UI`, `MCP Apps`. */
  name: string;
}

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
   * What the caller typed while a decision of the session was open or a turn ran, oldest
   * first; each is sent once the session is free, or leaves the list when the running turn
   * takes it.
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

/** Why a drawing was refused, in a person's terms (spec 3.3 §7.4 rule 5). */
export type RefusalKind = "looksLikeDecision" | "asksForSecret" | "cannotShow";

/**
 * A drawing that was not shown (spec 3.3 §7.4 rule 5): a plain notice in the thread; the
 * technical reason only in the developer panel and the terminal.
 */
export interface RefusedDrawing {
  /** The agent that tried to draw it. */
  agent: AgentRef;
  /** When. */
  at: string;
  /** Local `HH:MM:SS`. */
  atText: string;
  /** The render tool's call. */
  callId?: string | null;
  /** The component at fault, when one is. */
  component?: string | null;
  /** Why, in a person's terms. */
  kind: RefusalKind;
  /**
   * The technical reason: "catalog forms 1.0 · component c7 Button · label matches
   * reserved word \"Allow once\" after normalisation".
   */
  reason: string;
  /** "Finance tried to show something that looks like a decision. It wasn't shown." */
  text: string;
  /** The render tool. */
  tool: string;
}

/**
 * An agent the person removed, whose threads stay readable (spec 3.3 §5.7, §13 decision 18;
 * owner, at the wave 3 gate: "Removed agents" at the roster's foot).
 */
export interface RemovedAgent {
  /** The agent as its threads name it. */
  agent: AgentRef;
  /** "Removed on 25 Oct". */
  removedText: string;
  /** Its threads, newest first. */
  threads: RemovedThread[];
}

/** One thread of a removed agent, read only. */
export interface RemovedThread {
  /** When it last changed. */
  at: string;
  /** The session to open. */
  sessionId: string;
  /** "25 Oct 10:02". */
  when: string;
}

/** Whether a call can be taken back (spec 3.3 §7.1). */
export type Reversibility = "cantBeUnsent" | "cantBeUndone" | "canBeRemoved" | "canBeChangedBack";

/** How much harm a call can do, as approvers see it (spec 3.3 §7.1). */
export type Risk = "low" | "medium" | "high";

/** A scene's axes. */
export interface SceneAxes {
  /** A grid on the ground. */
  floor?: boolean;
  /** Labelled points along them. */
  ticks?: SceneTick[];
  /** The x axis's name. */
  x?: string | null;
  /** The y axis's name: up. */
  y?: string | null;
  /** The z axis's name. */
  z?: string | null;
}

/** Where a scene is seen from. */
export interface SceneCamera {
  /** Where the camera is, in place of the preset's. */
  at?: number[] | null;
  /** The person can't turn or zoom it. */
  fixed?: boolean;
  /** Its vertical field of view, 15 to 90 degrees. */
  fov?: number | null;
  /** A view the home frames to fit the scene. */
  preset: CameraPreset;
  /** It turns slowly about the scene, until the person turns it. */
  spin?: boolean;
  /** What it looks at, in place of the scene's centre. */
  target?: number[] | null;
}

/** A colour of the home's palette. */
export type SceneColor = "chart1" | "chart2" | "chart3" | "chart4" | "chart5" | "chart6" | "chart7" | "chart8" | "ink" | "neutral" | "muted" | "ramp";

/** An object's surface. */
export type SceneFinish = "matte" | "glossy" | "flat";

/** What a scene is. */
export type SceneKind = "space" | "bars" | "points" | "surface" | "network" | "globe";

/** A light added to the preset. */
export interface SceneLight {
  /** Where it is; a directional light shines from there towards the centre. */
  at: number[];
  /** Its colour; white when not given. */
  color?: SceneColor | null;
  /** How bright, 0 to 2. */
  intensity: number;
  /** What kind. */
  kind: LightKind;
}

/** A lighting preset. */
export type SceneLighting = "studio" | "soft" | "flat";

/** How an object looks: the home's colours, never one the agent picks. */
export interface SceneLook {
  /** Its colour. */
  color: SceneColor;
  /** Its surface. */
  finish: SceneFinish;
  /** How opaque, 0.15 to 1; 1 when not given. */
  opacity?: number | null;
  /** Drawn as edges only. */
  wireframe?: boolean;
}

/** An object that moves. Under reduced motion nothing moves; the person can pause any motion. */
export interface SceneMotion {
  /** How far it swings or how much it grows: 0 to 100 for `bob`, 0 to 1 for `pulse`. */
  amount?: number | null;
  /** The axis it turns or moves along. */
  axis: Axis;
  /** Seconds before it starts, 0 to 30. */
  delay?: number | null;
  /** How it moves. */
  kind: MotionKind;
  /** The points a `path` motion follows, `x, y, z` flat, in its parent's space. */
  path?: number[];
  /** How many times, 1 to 100; it goes on when not given. */
  repeat?: number | null;
  /** How long one turn, swing or trip takes, 0.2 to 120. */
  seconds: number;
  /** The object. */
  target: string;
}

/** One object of a scene. */
export interface SceneObject {
  /** Where it is, in its parent's space. */
  at: number[];
  /** Unique within the scene: what parents, panels, motion and picking name it by. */
  id: string;
  /** What it is called, shown beside it as page text. */
  label?: string | null;
  /** How it looks. */
  look: SceneLook;
  /** The group it belongs to: it moves, turns and scales with it. */
  parent?: string | null;
  /** How it is scaled along each axis. */
  scale?: number[] | null;
  /** Its shape. */
  shape: SceneShape;
  /**
   * A state it is in, drawn in the state's colour beside its label's word; an object with
   * a state has a label, so colour is never the only sign.
   */
  state?: SceneState | null;
  /** How it is turned about each axis, in degrees. */
  turn?: number[] | null;
  /**
   * The option value picking it chooses (see `SceneSelect`); an object with one has a
   * label, so the person sees what they pick.
   */
  value?: string | null;
}

/** A held component shown beside an object. */
export interface ScenePanel {
  /** The object it is shown beside. */
  anchor: string;
  /** The key of the node's child it shows. */
  key: string;
}

/**
 * Picking an object fills a choice field the drawing shows, with the object's `value`
 * (spec 3.3 §7.4 rule 5: a scene's pick fills a shown choice field; it never sends). The
 * field stays the way to choose by keyboard and screen reader, and a form's Send still asks.
 */
export interface SceneSelect {
  /**
   * What picking does, in words, written by the server: "Picking one fills Dock. Nothing
   * is sent."
   */
  note: string;
  /** The data model path of the choice field it fills, absolute. */
  path: string;
}

/** A scene object's shape. Flat number lists hold one point after another. */
export type SceneShape = {
  type: "group";
} | {
  /** Width, height and depth. */
  size: number[];
  type: "box";
} | {
  /** Its radius. */
  radius: number;
  type: "sphere";
} | {
  /** Its height. */
  height: number;
  /** The bottom's radius. */
  radiusBottom: number;
  /** The top's radius. */
  radiusTop: number;
  type: "cylinder";
} | {
  /** From the centre to the middle of the tube. */
  radius: number;
  /** The tube's radius. */
  tube: number;
  type: "torus";
} | {
  /** Along z. */
  depth: number;
  type: "plane";
  /** Along x. */
  width: number;
} | {
  /** Arrowheads. */
  arrow: LineEnds;
  /** Dashed. */
  dashed: boolean;
  /** The points, flat. */
  points: number[];
  /** Separate segments, each a pair of points, rather than one line through them all. */
  segments: boolean;
  /** A smooth curve through the points. */
  smooth: boolean;
  type: "line";
} | {
  /** Where each bar stands, `x, z` flat. */
  at: number[];
  /** Each bar's height. */
  heights: number[];
  /** Each bar's series, 0 to 7, drawn in that chart colour; empty for the object's own. */
  series?: number[];
  type: "bars";
  /** Each bar's width and depth. */
  width: number;
} | {
  /** Where each point is, flat. */
  at: number[];
  /** Each point's series, 0 to 7, drawn in that chart colour; empty for the object's own. */
  series?: number[];
  /** Each point's radius. */
  size: number;
  type: "points";
} | {
  /** Points along x. */
  columns: number;
  /** Along z. */
  depth: number;
  /** One height per point, row after row. */
  heights: number[];
  /** Points along z. */
  rows: number;
  type: "grid";
  /** Along x. */
  width: number;
} | {
  /** Its radius. */
  radius: number;
  type: "globe";
} | {
  /** How large. */
  size: LabelSize;
  /** What it reads. */
  text: string;
  type: "label";
};

/** A state an object is in. */
export type SceneState = "ok" | "warning" | "critical" | "info";

/** A labelled point on an axis. */
export interface SceneTick {
  /** Where along it. */
  at: number;
  /** Which axis. */
  axis: Axis;
  /** What it reads. */
  text: string;
}

/**
 * How a scene's heights map to the data's numbers: a height `h` stands for
 * `h * scale + offset`.
 */
export interface SceneValues {
  /** What a height of 0 stands for. */
  offset: number;
  /** What one unit of height stands for; above 0. */
  scale: number;
}

/** What asked for a `DrawnSent` call (spec 3.3 §6.1 `genui`, `genui-apps`). */
export type SentVia = "form" | "app";

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
} | {
  kind: "drawn";
  /** The drawing, its strings resolved on the server. */
  surface: DrawnSurface;
} | {
  kind: "drawnRefused";
  /** What the thread says, and the reason the developer panel shows. */
  refusal: RefusedDrawing;
} | {
  kind: "drawnSent";
  /** The call and the form it came from. */
  sent: DrawnSent;
} | {
  /** The app. */
  app: AppView;
  kind: "app";
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
} | {
  /** The action, as `VoiceUpdate::PendingAction` named it. */
  actionId: string;
  type: "cancelAction";
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
  /** The action, for `VoiceClientMessage::CancelAction`. */
  actionId: string;
  /** When it runs. */
  cancelUntil: string;
  /** What it will do, in the kernel's words: "Move News digest to 19:00". */
  heading: string;
  /** "Snoozing in 5 s". */
  text: string;
  type: "pendingAction";
} | {
  /** The action, when one was pending. */
  actionId?: string | null;
  /** It ran. */
  done: boolean;
  /** What it did or would have done: "Moved News digest to 19:00". */
  heading?: string | null;
  /** What happened, in plain words: "Cancelled · nothing changed". */
  text: string;
  type: "actionOutcome";
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
