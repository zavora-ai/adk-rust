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
  /** Where its commands run, when it has a shell and its source says (spec 3.3 §5.7). */
  shell?: ShellView | null;
  /** What it is doing. */
  status: AgentStatus;
  /** The status word: "Working", "Waiting for you", "Asleep until 07:00", "Done", "Failed". */
  statusText: string;
  /** When it wakes, while asleep with a wake time. */
  wakeAt?: string | null;
}

/** What an agent is doing (spec 3.3 §8.1). */
export type AgentStatus = "working" | "waitingForYou" | "asleep" | "done" | "failed" | "ready" | "paused" | "cantStart";

/** An agent's threads, newest first: `GET /surface/agents/{id}/threads`. */
export interface AgentThreads {
  /** The agent, by its roster name. */
  agent: string;
  /** Its threads, newest first. */
  threads: ThreadSummary[];
}

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

/** How a region of one answer stands against the other (board choice V11). */
export type Agreement = "agree" | "differ" | "onlyHere";

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

/** One answer for the picture, as the "Answer from" picker lists it. */
export interface AnswerChoice {
  /** The call's identifier. */
  id: string;
  /** "PP-OCRv6 small". */
  name: string;
  /** "on this computer", "Meta, in the cloud". */
  place: string;
  /** The tool. */
  tool: string;
}

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
  /** The digests of the images the device showed, for a call with images (spec 3.7 §8.2). */
  evidence?: string[];
  /** The device showed the call in full (§7.1). */
  expanded?: boolean;
  /** Why, for a `deny`; reported to the agent. */
  reason?: string | null;
  /** The card's `version`. */
  version: number;
}

/**
 * How a person answered since a time (spec 3.3 §5.1 "How you answered"): how many of each
 * answer, and on which surfaces. `GET /surface/answers`.
 */
export interface AnswerSummary {
  /** "Allow once". */
  allowedOnce: number;
  /** "Always allow". */
  alwaysAllowed: number;
  /** "Deny". */
  denied: number;
  /** Edited, then allowed. */
  edited: number;
  /** Counted from. */
  since: string;
  /** How many were given on each surface, most first. */
  surfaces: SurfaceCount[];
  /** "12 answers: 9 allowed once, 2 always allowed, 1 denied". */
  text: string;
  /** Every answer. */
  total: number;
}

/** One answer, in full. */
export interface AnswerView {
  /** When the answer holds nothing. */
  empty?: EmptyView | null;
  /** The call's identifier. */
  id: string;
  /** Labels with counts, in first-seen order. */
  labels: LabelCount[];
  /** Measurements. */
  measurements?: MeasurementsView | null;
  /** "PP-OCRv6 small". */
  name: string;
  /** "on this computer". */
  place: string;
  /** The regions, in the `RegionSet`'s order; a page's in reading order. */
  regions: RegionView[];
  /** The run and the call. */
  run: RunView;
  /**
   * The region selected first: the escalated or lowest-scored one on a page, the measured
   * one in a photo, the first otherwise.
   */
  selected?: number | null;
  /** "11 regions", "1.1 s", "$0". */
  stats: string[];
  /** The tool. */
  tool: string;
  /** The video and its tracks, for a frame. */
  video?: VideoView | null;
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

/** A badge on the picture. */
export interface BadgeView {
  /** "Live · 10:41:09". */
  text: string;
  /** The dot's colour. */
  tone: Tone;
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

/** One of the phone's budget rows. */
export interface BudgetRow {
  /** "Looking". */
  label: string;
  /** Its colour; `muted` for plain words. */
  tone: Tone;
  /** "2 a second (was 5)". */
  value: string;
}

/** "Frame budget" (board choice C3). */
export interface BudgetView {
  /** "Change the daily frame limit", once the camera paused. */
  limit?: string | null;
  /** Looking, Asking, Frames today. */
  meters: MeterView[];
  /** "Steps down in this order: …", while nothing has stepped down. */
  order?: string | null;
  /** The phone's rows. */
  rows: BudgetRow[];
  /** "for line 2". */
  scope: string;
  /** "What the budget did": each step with its time, then the next. */
  steps: StepView[];
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

/** What a person may do with a camera now. */
export interface CameraActions {
  /** "Raise a card from this frame". */
  raiseCard: boolean;
  /** "Try again now". */
  retry: boolean;
  /**
   * "Use this browser's camera": the camera takes the frames this screen captures
   * (`POST /surface/cameras/{camera}/frames`), as the web home's own camera does.
   */
  sendFrames?: boolean;
  /** "Stop the stream". */
  stop: boolean;
  /** "Camera on / Off". */
  switch: boolean;
  /** "Draw a zone", "Edit the zone". */
  zone: boolean;
}

/** The agent that watches a camera. */
export interface CameraAgent {
  /** The agent. */
  agent: AgentRef;
  /** "Asleep until a defect", "Paused by the frame budget", "Not watching: no frames". */
  line: string;
  /** The session its thread follows: "Open the thread". */
  sessionId: string;
  /** The line's colour. */
  tone: Tone;
}

/** A camera view. */
export type CameraPreset = "fit" | "front" | "top" | "side" | "iso";

/** Something found on the frame, in the camera's pixels. */
export interface CameraRegion {
  /** `[x0, y0, x1, y1]`; absent for a mask drawn alone. */
  bbox?: number[] | null;
  /** What a watch looks for: drawn selected. */
  finding: boolean;
  /** "part", "seal tear", "fiducial", "person": the overlay's colour (board choice S1). */
  label: string;
  /** A measured length. */
  measurement?: MeasurementView | null;
  /** A mask's outline (the Masks layer). */
  outline?: number[][];
  /** The phone's and a tile's tag: "T45", "14.3 mm". */
  shortTag: string;
  /** "part · T45", "seal tear · 0.91 · 5 frames · 14.3 mm", "person · blurred". */
  tag: string;
  /** Where the tag sits. */
  tagAt: TagAt;
  /** The anonymous track. */
  track?: number | null;
}

/** A camera's state, as its pill and its picture show it. */
export type CameraState = "live" | "savingFrames" | "paused" | "lost" | "off" | "stopped" | "frozen";

/** One camera of the grid. */
export interface CameraTile {
  /** The switch may be changed here. */
  canSwitch: boolean;
  /** "watching for seal tears", "shown only, nothing watched", "from the voice screen". */
  caption: string;
  /** The camera's id. */
  id: string;
  /** The switch list's line: "5 a second · 20% of today's frames". */
  line: string;
  /** "Line 2 camera". */
  name: string;
  /** The switch. */
  on: boolean;
  /** The phone's line: "watching", "2 a second", "shown only". */
  shortLine: string;
  /** The picture, with its overlays. */
  stage: StageView;
  /** The state. */
  state: CameraState;
  /** "Live", "Saving frames", "Off". */
  stateText: string;
  /** Its colour. */
  tone: Tone;
}

/** `GET /surface/cameras/{camera}` answer: one camera, watched large (board choice C1). */
export interface CameraView {
  /** What the person may do here now. */
  actions: CameraActions;
  /** The agent that watches it. */
  agent: CameraAgent;
  /** "Frame budget". */
  budget: BudgetView;
  /** The agent's open decisions on this session, such as the held line stop (board choice C13). */
  decisions: DecisionCard[];
  /** The camera's id. */
  id: string;
  /** "RTSP · 1920 × 1080 · on the line 2 server". */
  ingest: string;
  /** "Line 2 camera". */
  name: string;
  /** "Nothing is waiting for you." */
  nothingWaiting: string;
  /** The banner over the picture, when there is something to warn about. */
  notice?: NoticeView | null;
  /** The switch: on, or off (board choice C2). */
  on: boolean;
  /** "Sources": every camera, this one's tile following its state (board choice C15). */
  sources: SourceTile[];
  /** The picture. */
  stage: StageView;
  /** The state. */
  state: CameraState;
  /** "Live", "Saving frames", "Paused until midnight", "Can't reach the camera", "Camera off". */
  stateText: string;
  /** The pill's colour. */
  tone: Tone;
  /** "Tracks now". */
  tracks: TrackRow[];
  /** The words under the tracks: "Tracks are anonymous and start again when the camera does." */
  tracksNote: string;
  /**
   * The phone's words under the budget while the camera is off: "Tracks start again when the
   * camera is back on."
   */
  tracksShortNote?: string | null;
  /** "Watching for". */
  watches: WatchView[];
  /** The zones a person may edit, in the camera's pixels. */
  zones: ZoneView[];
}

/** `GET /surface/cameras` answer: every camera, a grid of them (board choice C8). */
export interface CamerasView {
  /** The agent that watches them. */
  agent?: CameraAgent | null;
  /** Each camera's frames today. */
  budget: MeterView[];
  /** The phone's: "4 · 3 on". */
  countShort: string;
  /** "4 sources · 3 on". */
  countText: string;
  /** The agents' open decisions on these cameras' sessions. */
  decisions: DecisionCard[];
  /** "Select a camera to watch it large. People are blurred before any frame is shown or kept." */
  footer: string;
  /** "Each source has its own frame budget. Stop all pauses every one of them." */
  note: string;
  /** "Nothing is waiting for you." */
  nothingWaiting: string;
  /** The camera shown first, by its id. */
  selected?: string | null;
  /** The phone's line under the large picture: "Line 2 camera · Live · 5 a second". */
  selectedLine?: string | null;
  /** "Each camera has its own frame budget. People are blurred before any frame is shown or kept." */
  shortNote: string;
  /** Every camera. */
  tiles: CameraTile[];
  /** "Cameras". */
  title: string;
}

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

/**
 * A held call's images on its card (spec 3.7 §8.2, X11): what would leave, after redaction,
 * with what was covered, where it goes, what it costs, how long it is kept and why nothing
 * can be sent when that is so.
 */
export interface CardMedia {
  /**
   * This surface may tighten the crop or add a redaction, which holds the call again with
   * the new image (board choices A7, A8).
   */
  canChange: boolean;
  /** What the cost counts: "851 image tokens". */
  costDetail?: string | null;
  /** "Up to $0.0044" while waiting; "$0.0024" once it ran. */
  costText?: string | null;
  /** What was covered before they leave, by class. */
  covered: CoveredLine[];
  /** Every pixel of every image is covered. */
  coveredAll: boolean;
  /** "100% of the image", when so. */
  coveredAllText?: string | null;
  /** What they were cut from: "Page 2 of claim-0412.pdf". */
  cutFrom?: string | null;
  /** How: "nothing resized". */
  cutNote?: string | null;
  /** The fixed line under the card (board choice A21). */
  fixedLine: string;
  /** Where they go. */
  goesTo?: GoesTo | null;
  /** The images, in the order of the call's evidence. */
  images: EvidenceImage[];
  /** "Kept as evidence until 18 Dec". */
  keptText?: string | null;
  /** Why nothing can be sent, when that is so. */
  notice?: MediaNotice | null;
  /**
   * The digest of the image a changed image replaced, when this call was held again with
   * a changed image.
   */
  replaces?: string | null;
  /** Where an authorised viewer opens the source image (board choice A9). */
  sourceHref?: string | null;
  /**
   * "The exact image that would leave", "The 3 images that would leave", "The image that
   * left" when a feature says where the images go (`MediaFacts::to`); otherwise
   * "Attached image", "3 attached images".
   */
  title: string;
  /** What they are: "Document crop", "3 document crops". */
  what?: string | null;
  /** What qualifies `what`: "never the page". */
  whatNote?: string | null;
}

/** `POST /surface/cameras/{camera}/cards` answer. */
export interface CardRaised {
  /** The kept frame's digest, after blurring: the evidence the card and a held call carry. */
  digest: string;
  /** The kept frame in the annotated viewer. */
  href: string;
  /** "Kept as evidence until 18 Dec". */
  keptUntil: string;
  /** What sending it to the agent did: sent, or queued behind a decision or a turn. */
  outcome: CommandOutcome;
  /** The regions that went with it. */
  regions: number;
  /** The short form: `sha256:c52ca5…ce7550`. */
  shortDigest: string;
  /** "Raised to the Inspector. This one frame is kept as evidence until 18 Dec." */
  text: string;
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
  /**
   * The digests of the images the surface showed, in the order of the card's media
   * (spec 3.7 §8.2): an answer that lets a call with images run names the images it
   * lets leave, and is refused when they are not the call's.
   */
  evidence?: string[];
  /** The surface showed the call in full (§7.1). */
  expanded?: boolean;
  /** Why, for a `deny`; reported to the agent. */
  reason?: string | null;
  type: "decide";
  /** The card's `version`. */
  version: number;
} | {
  /** The change. */
  change: ImageChange;
  /** The card's `id`. */
  decisionId: string;
  /** The digest of the image the person changed, as the card showed it. */
  from: string;
  /**
   * Only make the new image and say its digest and size, holding nothing again: what
   * the editor shows before **Use this image** (board choice A8).
   */
  preview?: boolean;
  type: "changeImage";
  /** The card's `version`. */
  version: number;
} | {
  /** The decision. */
  decisionId: string;
  type: "cancelAnswer";
} | {
  /**
   * The images the person sent with it (spec 3.3 §5.1): at most
   * `MAX_IMAGES`. A message with images is never offered to a
   * running turn; it goes as the session's next.
   */
  images?: ImageData[];
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
  type: "stopTurn";
} | {
  /**
   * The images the person sent with it (spec 3.3 §5.1): at most
   * `MAX_IMAGES`. A message with images is never offered to a
   * running turn; it goes as the session's next.
   */
  images?: ImageData[];
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
  /**
   * The images the person sent with it (spec 3.3 §5.1): at most
   * `MAX_IMAGES`. A message with images is never offered to a
   * running turn; it goes as the session's next.
   */
  images?: ImageData[];
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
  /** The image the change would make: its digest and size, and the one it replaces. */
  changed: ImageChanged;
  type: "imagePreview";
} | {
  /** The old decision. */
  card: DecisionCard;
  /** The image change, when an image changed. */
  changed?: ImageChanged | null;
  type: "heldAgain";
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
  /** The session. */
  sessionId: string;
  /** A turn was running and stopped. */
  stopped: boolean;
  /** "Stopped. Your agent waits for your next message." */
  text: string;
  type: "turnStopped";
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

/**
 * A command or code an agent ran (spec 3.3 §5.1), from its tool's result: the last lines it
 * wrote, how it exited and for how long. Every string is data.
 */
export interface CommandRun {
  /**
   * The command line, when the tool's result repeats it; otherwise the call's own
   * activity line says what ran.
   */
  command?: string | null;
  /** How long it ran, when the tool says. */
  durationMs?: number | null;
  /** How it exited, when it did. */
  exitCode?: number | null;
  /** The language of code the sandbox ran: `python`, `javascript`, `command`, …. */
  language?: string | null;
  /** How many lines the result holds in all. */
  lines: number;
  /** The last lines it wrote, at most `RUN_LINES`. */
  output: OutputLine[];
  /** Why it stopped short, when it did. */
  stopped?: RunStop | null;
  /** The tool cut what it wrote; the agent can read the whole of it. */
  truncated?: boolean;
}

/** A region's standing in a comparison. */
export interface CompareMark {
  /** How it stands. */
  agreement: Agreement;
  /** The region's index in its answer. */
  index: number;
  /** The tag over it: its reading where they differ, "only here". */
  tag?: string | null;
}

/** One side of a comparison. */
export interface ComparePane {
  /** The answer. */
  answer: AnswerView;
  /** Each region's standing. */
  marks: CompareMark[];
  /** "PP-OCRv6" — the short name the differences use. */
  shortName: string;
}

/** Two answers for one image, side by side (board choice V11). */
export interface CompareView {
  /** A. */
  a: ComparePane;
  /** Regions both found and read alike. */
  agree: number;
  /** B. */
  b: ComparePane;
  /** Regions both found and read differently. */
  differ: number;
  /** The differences, A's order first. */
  differences: Difference[];
  /** Regions only one found. */
  onlyOne: number;
  /** "Where both found a region, the boxes overlap closely (mean IoU 0.94)." */
  overlapText: string;
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
 * What a region on an evidence image covers, by the class the egress gate redacted it as
 * (spec 3.7 §7.8 step 3).
 */
export type CoverClass = "face" | "personalText" | "schemaField" | "marked" | "other";

/** The words over a picture that is not live. */
export interface CoverView {
  /** The camera-off icon. */
  off: boolean;
  /** A spinner: the source is being tried again. */
  retrying: boolean;
  /** "Nothing is captured or sent. You switched line 2 off at 11:05." */
  text: string;
  /** "Paused until midnight", "Can't reach the line 2 camera", "Camera off". */
  title: string;
}

/** What was covered of one class, as a line of "Covered before it leaves". */
export interface CoveredLine {
  /** The class. */
  class: CoverClass;
  /** How many regions. */
  count: number;
  /** Which images, when the call sends several and not all of them: "in image 3". */
  note?: string | null;
  /** "1 face", "3 personal text", "2 fields your schema marks personal". */
  text: string;
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
  /**
   * The images the call would send, keep or act on, after redaction, with what was
   * covered, where they go and what they cost (spec 3.7 §8.2, X11). Never on a chat card,
   * which carries the image itself or a link (S-I5).
   */
  media?: CardMedia | null;
  /** The call spends money. */
  money: boolean;
  /**
   * The run that waited on this decision ended while it still waits: the answer is kept
   * and used when the agent picks the thread up again (spec 3.7 §8.2, board choice A12).
   */
  parked?: ParkedView | null;
  /**
   * The screen plan the card decides with one answer (spec 3.7 Q8): its window and every
   * step with its target and the screenshot the target was found on. `None` for a card that
   * decides one call.
   */
  plan?: ScreenPlanView | null;
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

/** One difference between two answers. */
export interface Difference {
  /** What A answered: "PO-7781S · 0.58", or "—". */
  a: string;
  /** The region in A. */
  aRegion?: number | null;
  /** What B answered. */
  b: string;
  /** The region in B. */
  bRegion?: number | null;
  /** Its label. */
  label: string;
  /** The region's number: its reading order, or its place. */
  number: number;
  /** "handwriting · read differently", "text · only B found it". */
  title: string;
}

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

/** An answer with nothing in it (board choice V12): an answer, not an error. */
export interface EmptyView {
  /** Asked, Sent, Cost, Run. */
  facts: Fact[];
  /** "The call failed." and its line. */
  failed: FactPart[];
  /** "Hidden by your filters." and its line. */
  hidden: FactPart[];
  /** The sentence, in parts. */
  text: FactPart[];
  /** "Nothing found". */
  title: string;
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

/**
 * A value replaced by an escalation (board choice V6): what the local backend read, what
 * the escalation reads now, and the released crop's digest.
 */
export interface EscalationView {
  /** Who escalated: "Gemini". */
  by: string;
  /** The released crop's digest, short. */
  cropDigest?: string | null;
  /** What the first backend read: `PO-7781S`. */
  readAs?: string | null;
  /** Its score. */
  readAsScore?: number | null;
}

/** One image a held call would send, as its card shows it. */
export interface EvidenceImage {
  /** The feature's one-line description of it. */
  caption?: string | null;
  /** Every pixel of it is covered. */
  coveredAll: boolean;
  /**
   * What was covered of it, in one line: "1 field your schema marks personal", "nothing
   * covered".
   */
  coveredText: string;
  /** `sha256:<hex>` of the image's bytes, in full: the exact call and the terminal show it. */
  digest: string;
  /** The digest in short, `sha256:c52ca5…ce7550` (board choice S4). */
  digestShort: string;
  /** The field it shows: "PO number". */
  field?: string | null;
  /** Height in pixels. */
  height: number;
  /**
   * Where the surface loads the image with the viewer's own authorisation; absent when it
   * is deleted.
   */
  href?: string | null;
  /** Its place in the call's evidence: what the image route takes. */
  index: number;
  /** `image/png`. */
  mimeType: string;
  /** It is no longer kept. */
  purged?: PurgedView | null;
  /** The regions over it, covered before it leaves. */
  regions: EvidenceRegion[];
  /** "1012 × 638 PNG". */
  sizeText: string;
  /** Why it is sent rather than read here: "Handwriting". */
  why?: string | null;
  /** Width in pixels. */
  width: number;
}

/** A region drawn over an evidence image, in the image's own pixels. */
export interface EvidenceRegion {
  /** The class. */
  class: CoverClass;
  /** The tag on a small picture: "face". */
  shortTag: string;
  /**
   * The region's tag on a large picture: "face", or "face · whole person" for a person
   * covered whole.
   */
  tag: string;
  /** Left edge, pixels. */
  x0: number;
  /** Right edge, pixels, exclusive. */
  x1: number;
  /** Top edge, pixels. */
  y0: number;
  /** Bottom edge, pixels, exclusive. */
  y1: number;
}

/** What an example holds (board choice V13). */
export type ExampleScope = "region" | "page" | "nothing";

/** Where a labelled example may be saved (board choice V13). */
export interface ExamplesView {
  /** The source saves examples. */
  canSave: boolean;
  /** The case name offered first, for the selected region. */
  caseName: string;
  /** The file picked first. */
  file: string;
  /** The eval fixture files there are, relative to the fixtures' root. */
  files: string[];
}

/**
 * A labelled line of a region's details: "Answered in · Model-input pixels, 960 × 960,
 * letterboxed".
 */
export interface Fact {
  /** "Answered in". */
  label: string;
  /** The value, in parts. */
  parts: FactPart[];
}

/** One part of a fact's value. */
export interface FactPart {
  /** How they read. */
  style: FactStyle;
  /** The words. */
  text: string;
}

/** How part of a fact reads. */
export type FactStyle = "plain" | "mono" | "muted" | "strong";

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
} | {
  /** The entry, as it reads now. */
  entry: FeedEntry;
  type: "changed";
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

/** Why the server dropped a frame a screen sent. */
export type FrameDropped = "off" | "stopped" | "frozen" | "notReading";

/**
 * `POST /surface/cameras/{camera}/frames` answer: what became of one frame this screen
 * captured.
 */
export interface FrameTaken {
  /**
   * Why it did not. While the camera is off, stopped or paused by Stop all, the screen stops
   * capturing: a camera switched off sends nothing.
   */
  dropped?: FrameDropped | null;
  /** The frame went on to the camera's pipeline. */
  taken: boolean;
}

/** A frame the hub holds, served by its digest. */
export interface FrameView {
  /** When it was captured. */
  at: string;
  /** "10:41:09". */
  atText: string;
  /** `sha256:` of its bytes. */
  digest: string;
  /** `/api/surface/cameras/{camera}/frames/{digest}`: the bytes, while the hub holds them. */
  href: string;
}

/** `POST /surface/cameras/{camera}/freeze` body: the frame on screen, by its digest. */
export interface FreezeFrame {
  /** The frame's `sha256:` digest; the hub must still hold it. */
  frame: string;
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

/** Where the images go, in words. */
export interface GoesTo {
  /** The hosts, joined: `api.anthropic.com`. */
  host: string;
  /** The model: `claude-sonnet-5-5`. */
  model?: string | null;
  /** Who receives them: "Claude". */
  provider?: string | null;
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

/**
 * A change to an evidence image before it leaves (board choices A7, A8): the crop can only
 * shrink and covering can only grow, so a change never shows more than the image it changes.
 */
export interface ImageChange {
  /** Boxes `[x0, y0, x1, y1]` in the image's pixels to fill solid black. */
  cover?: number[][];
  /** The new edges `[x0, y0, x1, y1]` in the image's pixels, inside the image. */
  crop?: number[] | null;
  /** The image's place in the call's evidence. */
  image: number;
}

/** What a change made: the image it replaces and the new one. */
export interface ImageChanged {
  /** The digest of the image the change replaces. */
  from: string;
  /** The new image's height. */
  height: number;
  /** The digest of the new image, held again for the person's decision. */
  to: string;
  /** The new image's width. */
  width: number;
}

/**
 * An image of a person's message (spec 3.3 §5.1): PNG, JPEG, WebP or GIF, base64. A
 * command carries at most `MAX_IMAGES`, each at most
 * `MAX_IMAGE_BYTES` once decoded; the engine checks each image's
 * bytes, not only its declared type.
 */
export interface ImageData {
  /** The image, base64 (standard alphabet, padded). */
  data: string;
  /** `image/png`, `image/jpeg`, `image/webp` or `image/gif`. */
  mimeType: string;
}

/** One of the three switches of "What it can see" (spec 3.3 §5.6). */
export type InputSource = "mic" | "camera" | "screen";

/** A label and how many regions carry it: a filter chip. */
export interface LabelCount {
  /** How many. */
  count: number;
  /** The label. */
  label: string;
}

/** How large a label is. */
export type LabelSize = "small" | "medium" | "large";

/**
 * `POST /api/surface/vision/{session}/examples`: save a labelled example (board choices V11,
 * V12, V13).
 */
export interface LabelledExample {
  /** The answer the example starts from. */
  answer: string;
  /** The case's name: `inv-2291-p1-po-number`. */
  caseName: string;
  /** The eval fixtures file, relative to the fixtures' root: `evals/vision/invoices.test.json`. */
  file: string;
  /** The region, corrected, kept or drawn; absent for an example with nothing in it. */
  region?: RegionCorrection | null;
  /** What the example holds. */
  scope: ExampleScope;
}

/** A kind of light. */
export type LightKind = "point" | "spot" | "directional";

/** A line's arrowheads. */
export type LineEnds = "none" | "end" | "both";

/** A mask to draw: COCO run-length encoding, column-major, unset run first. */
export interface MaskView {
  /** Pixels set. */
  area: number;
  /** The runs. */
  counts: number[];
  /** Height of the mask's raster. */
  height: number;
  /** Width of the mask's raster. */
  width: number;
}

/** A measurement tile (board choice V9). */
export interface MeasureTile {
  /** "Longest". */
  label: string;
  /** "± 0.3 mm". */
  uncertainty: string;
  /** "14.3 mm". */
  value: string;
}

/** A measured length, drawn as a line with its ends. */
export interface MeasurementView {
  /** One end, in the camera's pixels. */
  from: number[];
  /** "14.3 mm". */
  text: string;
  /** The other end. */
  to: number[];
}

/** An answer's measurements (board choice V9). */
export interface MeasurementsView {
  /**
   * "In the belt's plane, from the fiducial: 40.0 mm is 245 px, so 6.12 px per mm. Valid
   * only in that plane."
   */
  calibration: string;
  /** The longest length's ends, in source pixels. */
  line?: number[][] | null;
  /** The region measured. */
  region: number;
  /** The tiles. */
  tiles: MeasureTile[];
}

/** A notice on the card about its images: why nothing can be sent. */
export interface MediaNotice {
  /** Why it says so. */
  kind: MediaNoticeKind;
  /** The rest. */
  text: string;
  /** The lead words, in bold: "Nothing to send." */
  title: string;
}

/** Why a card's images cannot be sent. */
export type MediaNoticeKind = "purged" | "allCovered";

/** One meter. */
export interface MeterView {
  /** "Looking", "Asking sam-3.1 about unclear tracks", "Frames today". */
  label: string;
  /** "As set", "At most 6 a minute · 14 today · $0.14", "Renews at midnight". */
  note?: string | null;
  /** How full the bar is, in hundredths. */
  percent: number;
  /** The bar's colour. */
  tone: Tone;
  /** "5 a second", "2 in the last minute", "61,240 of 300,000". */
  value: string;
}

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

/** What a banner offers. */
export type NoticeAction = {
  kind: "retry";
  /** The phone's words. */
  shortText: string;
  /** The words. */
  text: string;
} | {
  /** Where it opens. */
  href: string;
  kind: "limit";
  /** The phone's words. */
  shortText: string;
  /** The words. */
  text: string;
};

/** The banner over the picture. */
export interface NoticeView {
  /** What the banner offers. */
  action?: NoticeAction | null;
  /** The error, credentials hidden: "connection refused · rtsp://[credentials hidden]@…". */
  detail?: string | null;
  /** The phone's shorter text. */
  shortText: string;
  /** The phone's shorter title. */
  shortTitle: string;
  /** What follows the title. */
  text: string;
  /** "Saving frames.", "Parts on line 2 are passing without inspection." */
  title: string;
  /** Its colour. */
  tone: Tone;
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

/** A line a run wrote. */
export interface OutputLine {
  /** It went to standard error. Absent when false. */
  stderr?: boolean;
  /** The line, as data. */
  text: string;
}

/** A decision whose run stopped waiting for it (spec 3.7 §8.2, board choice A12). */
export interface ParkedView {
  /** When the run ended. */
  at: string;
  /** Local `HH:MM`. */
  atText: string;
  /** "Document desk stopped waiting at 10:24 and finished its run without this. …". */
  text: string;
}

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

/** What kind of picture an answer is about, which decides its layers and words. */
export type PictureKind = "photo" | "document" | "videoFrame";

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

/** An evidence image that is no longer kept (board choice A13). */
export interface PurgedView {
  /** The day it went, "18 Dec", when its retention ran out. */
  day?: string | null;
  /** The shorter line a small picture shows: "Image deleted 18 Dec". */
  short: string;
  /** "Evidence is kept for 30 days. What it was, …". */
  text: string;
  /** "This image was deleted on 18 Dec". */
  title: string;
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

/** `POST /surface/cameras/{camera}/cards` body: a card from a frozen frame (board choice C11). */
export interface RaiseCard {
  /** The frozen frame, by its digest; the hub must still hold it. */
  frame: string;
  /** "What should it look at?" */
  note: string;
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

/** A region as a person corrected or drew it. */
export interface RegionCorrection {
  /** The box `[x0, y0, x1, y1]` in the image's pixels. */
  box: number[];
  /**
   * The region it corrects, by its index in the answer; absent for a region the person
   * drew.
   */
  index?: number | null;
  /** The label. */
  label: string;
  /** The text, for a page's block. */
  text?: string | null;
}

/** One region of an answer, as the viewer draws and lists it. */
export interface RegionView {
  /** It touches the picture's edge. */
  atEdge: boolean;
  /** Its pixels are blurred in the picture: a person. */
  blurred: boolean;
  /** The box as the details show it: `1250, 466 → 1532, 540`, in source pixels. */
  boxText: string;
  /** The transform chain as steps (board choice V4). */
  chain: string[];
  /** The list's quieter words: the text, "mask · 14.3 mm", "box · at the edge". */
  detail: string;
  /** A value an escalation replaced. */
  escalation?: EscalationView | null;
  /** The details, in order (board choices V4, V5, V6). */
  facts: Fact[];
  /** The details' heading: "5 · handwriting", "Seal tear", "T4 · forklift". */
  heading: string;
  /** Its place in the answer's `RegionSet`: what an example and the export name. */
  index: number;
  /** The label or block kind: "handwriting", "seal tear". */
  label: string;
  /** The mask, when the shape is one. */
  mask?: MaskView | null;
  /** Its measurement in one line: "14.3 mm". */
  measurement?: string | null;
  /** Its place in reading order, from 1, for a page. */
  order?: number | null;
  /** The polygon's points, when the shape is one. */
  polygon?: number[][];
  /** The score. */
  score?: number | null;
  /** The score as the list shows it: "0.58". */
  scoreText: string;
  /** The shape's kind. */
  shape: ShapeKind;
  /** The tag over the picture: "part · 0.96", "seal tear · 0.91 · 14.3 mm". */
  tag: string;
  /** The text it reads, for a page's block. */
  text?: string | null;
  /** The track, for a video frame. */
  track?: number | null;
  /** "T4". */
  trackName?: string | null;
  /** Left edge in source pixels. */
  x0: number;
  /** Right edge in source pixels, exclusive. */
  x1: number;
  /** Top edge in source pixels. */
  y0: number;
  /** Bottom edge in source pixels, exclusive. */
  y1: number;
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

/** Why a run stopped short. */
export type RunStop = "timeout" | "memoryExceeded" | "policyViolation" | "error";

/** The run and the call that produced an answer. */
export interface RunView {
  /** The agent, by its name in the runtime. */
  agent: string;
  /** The tool and the call: "vision_read · c-31", in parts. */
  call: FactPart[];
  /** "Document desk · 10:31". */
  text: string;
}

/** What saving an example did. */
export interface SavedExample {
  /** The case's name. */
  caseName: string;
  /** The file, relative to the fixtures' root. */
  file: string;
  /** The image file the case points at, relative to the eval file. */
  image: string;
  /**
   * "Saved to evals/vision/invoices.test.json as inv-2291-p1-po-number. The run's answer
   * stays as it was."
   */
  text: string;
}

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

/** One step of a `ScreenPlanView`. */
export interface ScreenPlanStepView {
  /** What the step does: `left_click`, `type`, …. */
  operation: string;
  /** The action as the agent proposed it. */
  proposed: unknown;
  /** The digest of the screenshot the target was found on, when the runtime gave one. */
  screenshot?: string | null;
  /**
   * Where on the screen, in points: `(100, 300)-(190, 340) pt`. `None` for a step that acts
   * where the step before it put the focus, such as typing.
   */
  target?: string | null;
}

/**
 * A previewed screen plan a decision card decides with one answer (spec 3.7 Q8): up to N
 * desktop actions in one window. **Allow** runs the steps once each, in order, each checked
 * against its preview on a fresh screenshot before it runs; a step whose target moved is asked
 * again on a card of its own. Never a standing rule.
 */
export interface ScreenPlanView {
  /**
   * The plan's fingerprint, `sha256:…`, over its window, steps and targets: the answer binds
   * it.
   */
  digest: string;
  /** The steps, in the order they run. */
  steps: ScreenPlanStepView[];
  /** The window every step acts in: the application, then the window. */
  window: string;
}

/** What asked for a `DrawnSent` call (spec 3.3 §6.1 `genui`, `genui-apps`). */
export type SentVia = "form" | "app";

/**
 * A part of the web home a server serves beside the engine's own routes (spec 3.3 §6.1),
 * listed in `StatusLine::serves`.
 * 
 * adk-surface's routers name their part as they are built:
 * `web::verbs::router`,
 * `web::workflow::router`, `voice::router` and
 * `mobile::router`. The engine names `Cameras` when it has a camera hub and
 * `Kernel` when its source's kernel built its control-plane routes. A host
 * names a part mounted from elsewhere with `SurfaceEngine::serve`,
 * such as the website's desk.
 */
export type Served = "kernel" | "cameras" | "verbs" | "voice" | "website" | "workflow" | "phone";

/** The kind of a region's shape. */
export type ShapeKind = "point" | "box" | "polygon" | "mask" | "keypoints";

/** Where an agent's commands run (spec 3.3 §5.7), as its harness reports it. */
export type ShellPlace = {
  kind: "host";
} | {
  /** The enforcer: `seatbelt`, `bubblewrap`; absent when the source does not say. */
  enforcer?: string | null;
  kind: "sandbox";
} | {
  /** The image. */
  image: string;
  kind: "container";
} | {
  kind: "remote";
  /** It runs off this computer, so commands cannot reach its files. */
  offHost: boolean;
  /** The provider: `e2b`, `daytona`, `modal`, …. */
  provider: string;
} | {
  kind: "degraded";
  /** Why it is unavailable. */
  reason: string;
  /** The sandbox asked for. */
  requested: string;
};

/** Where an agent's commands run: nowhere, in the sandbox, or on this computer. */
export type ShellUse = "none" | "sandboxed" | "host";

/** Where an agent's commands run, and the words that say so. */
export interface ShellView {
  /** Where. */
  place: ShellPlace;
  /** "Commands run in the sandbox (Seatbelt)". */
  text: string;
}

/** One camera in "Sources", the strip under a single camera (board choice C15). */
export interface SourceTile {
  /** The camera's id. */
  id: string;
  /** "Line 3 camera". */
  name: string;
  /** The camera shown large. */
  selected: boolean;
  /** The small picture. */
  stage: StageView;
  /** The state. */
  state: CameraState;
  /** "Live", "Saving frames", "Paused", "Lost", "Off". */
  stateText: string;
  /** Its colour. */
  tone: Tone;
}

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

/** How the picture is drawn. */
export type StageLook = "live" | "dimmed" | "grey" | "dark";

/** The picture and what is drawn over it. */
export interface StageView {
  /** "Live · 10:41:09", "Last frame · 15:12:03". */
  badge?: BadgeView | null;
  /** The words over the picture when it is not live. */
  cover?: CoverView | null;
  /** The frame shown, when one is held. */
  frame?: FrameView | null;
  /** The camera's height in pixels. */
  height: number;
  /** How the picture is drawn. */
  look: StageLook;
  /** "Looking 5 a second · overlays 0.1 s behind" (board choice C10). */
  rate?: string | null;
  /** What was found on the frame. */
  regions: CameraRegion[];
  /** Where each track was over the frames held. */
  trails: TrailView[];
  /** The camera's width in pixels; every region is in these pixels. */
  width: number;
  /** The zones. */
  zones: ZoneView[];
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
  /**
   * The parts of the home this server serves beside the engine's own routes, in order: the
   * web home reads no route of a part it leaves out, so a server built or mounted without
   * one answers no request for it. Absent only from a server older than this field.
   */
  serves: Served[];
  /** Spend, when the source knows it. */
  spend?: SpendView | null;
  /** Everything is stopped. */
  stopped: boolean;
  /** "All agents stopped", while stopped. */
  stoppedText?: string | null;
}

/** One step the governor took, or the next. */
export interface StepView {
  /** "14:02", or "next". */
  at: string;
  /** The next step, not taken yet. */
  next: boolean;
  /** "Looked less often: 5 → 2 a second (90% used)". */
  text: string;
}

/** `POST /surface/stop` body: **Stop all** from a paired device (spec 3.3 §11 S-I10). */
export interface StopRequest {
  /** Why. */
  reason?: string | null;
}

/** How many answers were given on one surface. */
export interface SurfaceCount {
  /** How many. */
  count: number;
  /** The surface. */
  surface: SurfaceKind;
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
  /** `GET /surface/briefing` and `GET /surface/answers`: since when. */
  since?: string | null;
  /** The surface; defaults to the web home. */
  surface?: SurfaceKind | null;
}

/** Where a region's tag sits: above its box, below it, or inside it at the frame's edge. */
export type TagAt = "above" | "below" | "in";

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
  /** The images the person sent with it (spec 3.3 §5.1). Absent when none. */
  images?: ImageData[];
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
  /**
   * The image a vision tool answered about, by digest: the thread links the call to
   * the annotated viewer (spec 3.7 §8.2, board choice V1).
   */
  image?: string | null;
  kind: "result";
  /** It succeeded. */
  ok: boolean;
  /** "Done" or "Failed". */
  text: string;
  /** The tool. */
  tool: string;
} | {
  /** The call it answers. */
  callId?: string | null;
  kind: "run";
  /** It succeeded: it exited with 0. */
  ok: boolean;
  /** The run. */
  run: CommandRun;
  /** "Done" or "Failed". */
  text: string;
  /** The tool. */
  tool: string;
} | {
  /** The call that offered them. */
  callId?: string | null;
  kind: "suggestions";
  /** The replies, checked: one to four short lines, as data. */
  replies: string[];
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
} | {
  /** The error's code, for a developer: `model.internal`. */
  code?: string | null;
  kind: "turnFailed";
  /**
   * Why, as the runtime said it: "no recorded answer for turn 3". Absent when the
   * runtime recorded no reason.
   */
  reason?: string | null;
  /** "Claims Desk couldn't answer". */
  text: string;
};

/** `GET /surface/sessions/{session}/thread` answer. */
export interface ThreadResponse {
  /** Every event of the session store, numbered by position from 1. */
  entries: FeedEntry[];
  /** The session. */
  sessionId: string;
}

/**
 * One of an agent's threads (spec 3.3 §5.1 "Thread history"): its session and when it last
 * changed.
 */
export interface ThreadSummary {
  /** When it last changed. */
  at: string;
  /** The session to open. */
  sessionId: string;
  /** "25 Oct 10:02". */
  when: string;
}

/** One step of how a decision got here (spec 3.3 §7.1). */
export interface TimelineStep {
  /** The step the decision is at now. */
  current: boolean;
  /** "Asked at 18:00", "Waited through quiet time", "Waiting for you". */
  text: string;
}

/** The colour a line of words takes. */
export type Tone = "ok" | "warn" | "fail" | "muted" | "plain";

/** One track now. */
export interface TrackRow {
  /** A watch looks for what was found on it: the row is marked. */
  finding: boolean;
  /** "T43". */
  id: string;
  /** "part". */
  label: string;
  /** "seal tear 0.91 · in the zone · 5 frames". */
  note: string;
  /** The phone's note: "seal tear 0.91 · in the zone". */
  shortNote: string;
  /** The swatch: the finding's label, else the track's. */
  swatch: string;
}

/** One track of a video (board choice V10). */
export interface TrackView {
  /** People are blurred. */
  blurred: boolean;
  /** "139", "1.2k": frames seen. */
  framesText: string;
  /** The track's identifier. */
  id: number;
  /** "forklift". */
  label: string;
  /** "T4". */
  name: string;
  /** When it is seen, in seconds. */
  spans: number[][];
  /** Where it was, oldest first. */
  trail: number[][];
}

/** Where a track was over the frames held: its trail. */
export interface TrailView {
  /** Its centres, newest first, in the camera's pixels. */
  points: number[][];
  /** The track. */
  track: number;
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

/** The video a frame comes from (board choice V10). */
export interface VideoView {
  /** Where the frame is, in seconds. */
  atS: number;
  /** "10:34.1". */
  atText: string;
  /** "frame 19,023 · sampled 5 a second". */
  frameText: string;
  /** How long the video is, in seconds. */
  lengthS: number;
  /** "20:00". */
  lengthText: string;
  /** "Tracks are anonymous …". */
  note: string;
  /** The tracks. */
  tracks: TrackView[];
}

/** The picture. */
export interface ViewerImage {
  /** `sha256:<hex>` in full. */
  digest: string;
  /** `sha256:556b49…a22654` (board choice S4). */
  digestShort: string;
  /** Height in pixels. */
  height: number;
  /** Where the surface loads it with the viewer's own authorisation. */
  href: string;
  /** `image/png`. */
  mimeType: string;
  /** Width in pixels. */
  width: number;
}

/** What the annotated viewer shows of one image in one session (spec 3.7 §8.2, X12). */
export interface ViewerView {
  /** The agent the back link returns to. */
  agent: string;
  /** Its display name: "Document desk". */
  agentName: string;
  /** The answer shown, when there is one. */
  answer?: AnswerView | null;
  /** Every answer for this image in the session, newest first. */
  answers: AnswerChoice[];
  /** A feature's line under the picture. */
  caption?: string | null;
  /** Two answers side by side, when asked. */
  compare?: CompareView | null;
  /** Saving labelled examples. */
  examples: ExamplesView;
  /** The picture. */
  image: ViewerImage;
  /** What kind of picture it is. */
  kind: PictureKind;
  /** "1654 × 2339 · INV-2291.pdf, page 1 at 200 dpi". */
  meta: string;
  /** The score a region needs to be shown at first. */
  minScore: number;
  /** The session. */
  sessionId: string;
  /** "INV-2291 · page 1". */
  title: string;
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

/** One watch condition (board choice C1 "Watching for"). */
export interface WatchView {
  /** It looks at frames now. */
  active: boolean;
  /** ". Then wait 30 s before waking again for the same track." */
  after: string;
  /** "Score 0.80 or more, 3 frames in a row, inside". */
  before: string;
  /** Its id. */
  id: string;
  /** The label it looks for: its colour. */
  label: string;
  /** "Seal tear". */
  name: string;
  /** "Watching", "Paused", "No frames", "Camera off". */
  pill: string;
  /** What follows the zone on the phone: " · not watching" when it does not look. */
  shortAfter: string;
  /** The phone's line: "Seal tear, 0.80+, 3 frames in". */
  shortBefore: string;
  /** "Woke the Inspector 3 times today". */
  woke: string;
  /** The zone it looks inside: "Inspection window". */
  zone?: string | null;
}

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

/**
 * `PUT /surface/cameras/{camera}/zones/{zone}` body: the zone's new name and corners, and the
 * corners the person saw before changing them.
 */
export interface ZoneChange {
  /** `[x0, y0, x1, y1]` in the camera's pixels. */
  corners: number[];
  /** The zone's name. */
  name: string;
  /** The corners the person started from: a zone changed since is refused, not overwritten. */
  was: number[];
}

/** `PUT /surface/cameras/{camera}/zones/{zone}` answer. */
export interface ZoneSaved {
  /** "Saved. The seal tear watch uses it from the next frame." */
  text: string;
  /** The zone now. */
  zone: ZoneView;
}

/** A zone, in the camera's pixels (board choice C12). */
export interface ZoneView {
  /** `[x0, y0, x1, y1]` in the camera's pixels. */
  corners: number[];
  /** "700, 200 → 1600, 880". */
  cornersText: string;
  /** Its id. */
  id: string;
  /** "Inspection window". */
  name: string;
  /** The watches that use it: "Seal tear watch". */
  usedBy: string[];
}
