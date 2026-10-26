// Generated from adk-website's view types by `scripts/check-surface-api.sh --write`.
// Do not edit: CI fails when this file drifts from the Rust source (spec 3.5 §8.2).

/** An open alarm (spec 3.5 §7.21). */
export interface AlarmLine {
  /** Whether it repeats until a person acknowledges it: an urgent alarm. */
  canAcknowledge: boolean;
  /** The alarm. */
  id: string;
  /** When. */
  raisedAt: string;
  /** What happened. */
  text: string;
  /** Its urgency. */
  urgency: Pill;
}

/** Who has approved a release waiting for its chain, and who still must (A-I23). */
export interface ApprovalView {
  /** The roles the viewer may approve in now. */
  canApprove: string[];
  /** The chain's roles, in order, with who approved in each. */
  lines: SignerLine[];
  /** Who proposed it; they cannot approve it. */
  proposer: string;
  /** One line. */
  text: string;
}

/** One recent batch. */
export interface BatchLine {
  /** The batch. */
  batch: number;
  /** When it closed. */
  closedAt: string;
  /** One line. */
  text: string;
}

/** A spend category against its cap. */
export interface CategoryLine {
  /** Its cap. */
  cap: string;
  /** The category. */
  name: string;
  /** Why it is paused, when it is. */
  paused?: string | null;
  /** Spent as a share of the cap, in percent from 0 to 100. */
  percent: number;
  /** Spent this month. */
  spent: string;
}

/** A change to the charter under review. */
export type CharterCommand = {
  /** The section. */
  section: string;
  type: "review";
} | {
  /** The section. */
  section: string;
  type: "edit";
  /** The new values, by field id. */
  values: Record<string, string>;
} | {
  /** The role, such as `owner`. */
  role: string;
  type: "sign";
};

/** `GET`, `POST /surface/website/charter`. */
export interface CharterResponse {
  /** The charter under review, if a draft exists. */
  charter?: CharterReview | null;
}

/** The charter under review (spec 3.5 §7.18 stage 5). */
export interface CharterReview {
  /** The roles the viewer may sign in now. */
  canSign: string[];
  /** The committed version, once every signer has signed. */
  committed?: CommittedView | null;
  /** The site kind's template it started from. */
  kind: string;
  /** Why it cannot be signed yet. */
  problems: string[];
  /** Sections read so far. */
  reviewed: number;
  /** One card per section. */
  sections: SectionCard[];
  /** The signatures it needs, in the chain's order. */
  signers: SignerLine[];
  /** Where it stands. */
  status: Pill;
  /** The version it becomes. */
  version: number;
}

/** One launch-gate check (spec 3.5 §7.8.1). */
export interface CheckView {
  /** What to fix. */
  findings: string[];
  /** The check, in words. */
  name: string;
  /** What it could not measure here. */
  note?: string | null;
  /** How it ended. */
  status: Pill;
}

/** `POST …/setup/claim` body. */
export interface ClaimRequest {
  /** The setup code from the host's logs. */
  code: string;
  /** A name for this device, such as "Amina's laptop". */
  device?: string | null;
}

/** The device the setup code paired; its token is shown once (spec 3.5 A-I20). */
export interface ClaimedView {
  /** The device's name. */
  device: string;
  /** When the pairing ends. */
  expiresAt: string;
  /** The device's token: the page keeps it and sends it with every request. */
  token: string;
}

/** The committed version. */
export interface CommittedView {
  /** When it took effect. */
  at: string;
  /** Who signed it. */
  signedBy: string[];
  /** The version. */
  version: number;
}

/** The next step of a connection, for the person to take outside the site (A-I19). */
export type ConnectStep = {
  type: "open";
  /** The page. */
  url: string;
} | {
  /** The code. */
  code: string;
  /** Where to use it. */
  how: string;
  type: "code";
} | {
  type: "done";
};

/** One connection (spec 3.5 §7.18 stage 8, A-I19). */
export interface ConnectionCard {
  /** The account it is connected to. */
  account?: string | null;
  /** Whether it can be connected here. */
  canConnect: boolean;
  /** The connection, as sent back. */
  id: string;
  /** What it is for. */
  kind: string;
  /** How it connects, in words. */
  method: string;
  /** The service. */
  service: string;
  /** Where it stands. */
  status: Pill;
}

/** Stage 8: connections. */
export interface ConnectionsView {
  /** The connections. */
  cards: ConnectionCard[];
  /** What the site says before the first connection (A-I19). */
  notice: string;
}

/** A decision discovery found for the owner: a quarantined subject or a legacy feature. */
export interface DecisionView {
  /** The owner's choice. */
  chosen?: string | null;
  /** What applies until the owner decides. */
  default?: string | null;
  /** What was found. */
  detail: string;
  /** Where. */
  evidence: string[];
  /** The card. */
  id: string;
  /** The choices. */
  options: FieldOption[];
  /** What it is about. */
  title: string;
}

/** Stage 8: the deployment and going live (spec 3.5 §7.22, A-I21). */
export interface DeployView {
  /** Whether the owner can go live now. */
  canGoLive: boolean;
  /** The monthly running-cost estimate, by category. */
  cost: Line[];
  /** Whether the estimate fits the budget. */
  fits?: boolean | null;
  /** The live release. */
  live?: string | null;
  /** The deployment shape. */
  shape: string;
  /** The launch waiting for its approval chain. */
  waiting?: ApprovalView | null;
}

/** A design direction (spec 3.5 §7.18 stage 6). */
export interface DesignOption {
  /** Its dark colours. */
  dark?: Swatch | null;
  /** The direction in words. */
  direction: string;
  /** `keep_current` or a theme's name. */
  id: string;
  /** Its light colours. */
  light?: Swatch | null;
  /** Its name. */
  name: string;
}

/** Stage 6. */
export interface DesignView {
  /** What building release 1 did. */
  built?: string | null;
  /** The owner's choice. */
  chosen?: string | null;
  /** Pages the per-change checks held. */
  held: string[];
  /** The directions, and keeping the current design for an existing site. */
  options: DesignOption[];
}

/** Stage 2: what discovery found, each fact with its source (spec 3.5 §7.18). */
export interface DiscoveryView {
  /** Quarantined subjects and legacy features. */
  decisions: DecisionView[];
  /** The facts. */
  facts: FactView[];
  /** Counts of what it read. */
  found: Line[];
  /** The site kind. */
  kind: KindView;
  /** Whether discovery ran. */
  ran: boolean;
  /** What it read. */
  source?: string | null;
}

/** A draft in progress. */
export interface DraftLine {
  /** Who wrote it. */
  author: string;
  /** The draft. */
  id: string;
  /** Its kind's name. */
  kind: string;
  /** Where it stands. */
  status: Pill;
  /** Its title. */
  title: string;
}

/** One draft in the editor. */
export interface DraftView {
  /** The approval chain it waits for. */
  approval?: ApprovalView | null;
  /** Who wrote it. */
  author: string;
  /** The body, in Markdown. */
  body: string;
  /** The live item it changes, if any. */
  changes?: string | null;
  /** One line on the checks. */
  checkText?: string | null;
  /** The per-change checks, once saved. */
  checks: CheckView[];
  /** The kind's fields with their values. */
  fields: FieldSpec[];
  /** The draft. */
  id: string;
  /** The kind, as sent back. */
  kind: string;
  /** The kind's name. */
  kindTitle: string;
  /** The language. */
  language: string;
  /** Its media. */
  media: MediaView[];
  /** How publishing ended. */
  outcome?: string | null;
  /** The preview's pages and how they change. */
  preview: PageView[];
  /** The schedule. */
  schedule: ScheduleInput;
  /** The address part. */
  slug: string;
  /** Where it stands. */
  status: Pill;
}

/** An editor action (spec 3.5 §7.8.2). Uploads have their own call, with the file's bytes. */
export type EditorCommand = {
  /** The kind, such as `product`. */
  kind: string;
  /** Its language; the site's first unless set. */
  language?: string | null;
  type: "new";
} | {
  /** The item. */
  id: string;
  type: "open";
} | {
  /** The body, in Markdown. */
  body?: string | null;
  /** The draft. */
  draft: string;
  /** The kind's fields, by id. */
  fields: Record<string, string>;
  /** The language. */
  language: string;
  /** The schedule. */
  schedule?: ScheduleInput;
  /** The address part. */
  slug: string;
  type: "save";
} | {
  /** The text. */
  alt: string;
  /** The draft. */
  draft: string;
  /** The media file. */
  path: string;
  type: "confirmAlt";
} | {
  /** The draft. */
  draft: string;
  type: "preview";
} | {
  /** The draft. */
  draft: string;
  type: "publish";
} | {
  /** The draft. */
  draft: string;
  /** The role. */
  role: string;
  type: "approve";
} | {
  /** The draft. */
  draft: string;
  /** Why. */
  reason: string;
  type: "reject";
} | {
  /** A new body. */
  body?: string | null;
  /** Fields to change. */
  fields?: Record<string, string>;
  /** The item. */
  id: string;
  /** The correction note readers see. */
  note: string;
  type: "correct";
} | {
  /** The item. */
  id: string;
  /** The notice; 410 Gone when absent. */
  notice?: string | null;
  type: "withdraw";
} | {
  /** The draft. */
  draft: string;
  type: "discard";
};

/** The editor's home: the forms, the content and the drafts (spec 3.5 §7.8.2). */
export interface EditorHome {
  /** Whether the viewer may write. */
  canEdit: boolean;
  /** Who approves a publication, when the charter names a chain. */
  chain?: string | null;
  /** Drafts in progress. */
  drafts: DraftLine[];
  /** The site's content, newest kinds first. */
  items: ItemLine[];
  /** One form per content kind. */
  kinds: KindForm[];
  /** The site's languages. */
  languages: FieldOption[];
}

/** What an editor action did. */
export interface EditorOutcome {
  /** The draft it acted on. */
  draft?: DraftView | null;
  /** The editor after it. */
  home: EditorHome;
  /** One line on what happened. */
  note?: string | null;
}

/** The owner's answer on a discovered fact (spec 3.3 §8.2 "That's right / Fix it / Forget it"). */
export type FactAnswer = "right" | "fix" | "forget";

/** A discovered fact (spec 3.5 §7.18 stage 2, A-I18). */
export interface FactView {
  /** The fact. */
  id: string;
  /** Whether the owner may still answer it. */
  open: boolean;
  /** Where it was found. */
  source?: string | null;
  /** "A guess" until the owner confirms it. */
  status: Pill;
  /** What it says. */
  text: string;
}

/** What a form field takes. */
export type FieldKind = "text" | "longText" | "markdown" | "email" | "tel" | "url" | "number" | "dateTime" | "time" | "choice";

/** One choice of a `FieldKind::Choice` field. */
export interface FieldOption {
  /** What the person reads. */
  label: string;
  /** The value sent back. */
  value: string;
}

/** One field of a form: the interview's, a charter section's or the web editor's. */
export interface FieldSpec {
  /** A line under the field. */
  hint?: string | null;
  /** The field's id, the key of its value. */
  id: string;
  /** What it takes. */
  kind: FieldKind;
  /** Its label. */
  label: string;
  /** The choices of a `FieldKind::Choice` field. */
  options?: FieldOption[];
  /** Whether it must be filled in. */
  required: boolean;
  /** The value it starts with. */
  value?: string | null;
}

/** One interview form (spec 3.5 §7.18 stage 4). */
export interface FormView {
  /** Whether it is answered. */
  answered: boolean;
  /** Its fields, with the answers given. */
  fields: FieldSpec[];
  /** Whether discovery's confirmed facts answered it already. */
  fromDiscovery: boolean;
  /** The form. */
  id: string;
  /** The question. */
  question: string;
}

/** Stage 7: the launch gate's report, page by page (spec 3.5 §7.8.1, §7.18). */
export interface GateView {
  /** The checks. */
  checks: CheckView[];
  /** The preview's pages. */
  pages: PageView[];
  /** Whether every check passed. */
  passed: boolean;
  /** One line. */
  text: string;
}

/** The owner's decision on something held. */
export type HeldDecision = "release" | "discard";

/** A feed delivery held whole (spec 3.5 A-I27). */
export interface HeldDeliveryLine {
  /** The bounds it broke. */
  breaks: string[];
  /** The delivery. */
  delivery: number;
  /** The feed. */
  feed: string;
  /** When it arrived. */
  receivedAt: string;
  /** What it would change. */
  would: string[];
}

/** An item held alone (spec 3.5 §7.8.3). */
export interface HeldItemLine {
  /** The batch it came in. */
  batch: number;
  /** When it was held. */
  heldAt: string;
  /** The item. */
  id: string;
  /** Why it waits. */
  reason: string;
  /** Its title. */
  title: string;
}

/** A payment held for the owner (spec 3.5 §7.14). */
export interface HeldPayment {
  /** How much. */
  amount: string;
  /** What it is for. */
  description: string;
  /** Why it waits. */
  why: string;
}

/** Held batches (spec 3.5 §7.8.3). */
export interface HeldView {
  /** The latest batches. */
  batches: BatchLine[];
  /** Deliveries held whole. */
  deliveries: HeldDeliveryLine[];
  /** Items held alone. */
  items: HeldItemLine[];
}

/** Stage 4. */
export interface InterviewView {
  /** Whether the interview is done. */
  done: boolean;
  /** The forms, in order. */
  forms: FormView[];
  /** The live preview. */
  preview?: PreviewView | null;
}

/** One item of the site's content. */
export interface ItemLine {
  /** The item. */
  id: string;
  /** Its kind's name. */
  kind: string;
  /** Its language. */
  language: string;
  /** Whether it can be corrected or withdrawn: it is live. */
  live: boolean;
  /** When it publishes, for a scheduled item. */
  scheduled?: string | null;
  /** Its address part. */
  slug: string;
  /** Where it stands. */
  state: Pill;
  /** Its title. */
  title: string;
}

/** One job and its level (spec 3.5 §7.11, §7.20). */
export interface JobLine {
  /** The agent that does it. */
  agent?: string | null;
  /** Its area. */
  area: string;
  /** Whether the viewer may lower it. */
  canLower: boolean;
  /** Whether the viewer may raise it. */
  canPromote: boolean;
  /** The job. */
  id: string;
  /** How much it does alone. */
  level: string;
  /** The level a promotion would give it. */
  next?: string | null;
  /** Whether it has earned the offer of a promotion. */
  offered: boolean;
  /** Its record towards a promotion. */
  record?: string | null;
  /** Since when. */
  since: string;
  /** What it is. */
  title: string;
}

/** A content kind's form (spec 3.5 §7.8.2, Q13): structured fields, then the Markdown body. */
export interface KindForm {
  /** What it is for. */
  description: string;
  /** Its fields, before the body. */
  fields: FieldSpec[];
  /** The kind, as sent back. */
  kind: string;
  /** Its name. */
  title: string;
}

/** The kind of site: proposed from the evidence, confirmed by the owner (spec 3.5 §7.16.1). */
export interface KindView {
  /** The confirmed kind. */
  confirmed?: string | null;
  /** Every kind, the proposal and its alternatives first. */
  options: FieldOption[];
  /** The proposed kind. */
  proposed?: string | null;
  /** Why. */
  reasons: string[];
}

/** One KPI this week (spec 3.5 §7.11, §7.24). */
export interface KpiCard {
  /** Its baseline. */
  baseline?: string | null;
  /** The KPI. */
  id: string;
  /** The week before. */
  lastWeek?: string | null;
  /** Its name. */
  name: string;
  /** "Measuring until 8 Nov" or "Tracking". */
  phase: string;
  /** Its target, the owner's. */
  target?: string | null;
  /** This week. */
  thisWeek?: string | null;
  /** The trend, in its direction: up is better. */
  trend: Pill;
}

/** A label and its value, for a card's lines. */
export interface Line {
  /** The label. */
  label: string;
  /** The value, in words. */
  value: string;
}

/** A media file beside a draft (spec 3.5 §7.8.2 "Media"). */
export interface MediaView {
  /** Its alt text. */
  alt?: string | null;
  /** Whether a person confirmed the alt text; the accessibility check needs it. */
  altConfirmed: boolean;
  /** Its media type. */
  contentType: string;
  /** Whether it is a generated illustration. */
  generated: boolean;
  /** Its file name. */
  name: string;
  /** Its path. */
  path: string;
}

/** Cost, revenue and profit and loss (spec 3.5 §7.14). */
export interface MoneyView {
  /** Each category this week. */
  categories: Line[];
  /** Spent this month, against the budget. */
  month: string;
  /** Revenue, profit, cost per lead and per sale, where the site sells. */
  results: Line[];
  /** Spent this week. */
  spend: string;
  /** Whether the month is inside the budget. */
  within: Pill;
}

/** A promotion the jobs earned, for the owner to decide (A-I13). */
export interface OfferLine {
  /** Its level now. */
  from: string;
  /** The job. */
  job: string;
  /** The record behind the offer. */
  record: string;
  /** What it is. */
  title: string;
  /** The level a promotion gives it. */
  to: string;
}

/** An input to onboarding (spec 3.5 §7.18), from the paired device only (A-I20). */
export type OnboardingCommand = {
  /** Where to start. */
  from: StartFrom;
  /** The site's name. */
  name?: string | null;
  /** A phone number for urgent alarms. */
  phone?: string | null;
  /** The profiles' URLs. */
  profiles?: string[];
  type: "start";
  /** The existing site's URL. */
  url?: string | null;
} | {
  /** The answer. */
  answer: FactAnswer;
  /** The fact. */
  id: string;
  /** The corrected text, for `fix`. */
  text?: string | null;
  type: "fact";
} | {
  /** The kind, such as `business`. */
  kind: string;
  type: "kind";
} | {
  /** Whether to freeze it. */
  freeze: boolean;
  type: "protect";
} | {
  /** The card. */
  card: string;
  /** The choice. */
  choice: string;
  type: "decide";
} | {
  /** The form. */
  form: string;
  type: "answer";
  /** The values, by field id. */
  values: Record<string, string>;
} | {
  type: "finishInterview";
} | {
  /** What to do. */
  command: CharterCommand;
  type: "charter";
} | {
  /** `keep_current`, or a theme: `paper`, `harbour`, `meadow`. */
  design: string;
  type: "design";
} | {
  type: "runGate";
} | {
  /** The connection. */
  connection: string;
  type: "connect";
} | {
  /** The connection. */
  connection: string;
  type: "skip";
} | {
  type: "goLive";
} | {
  /** The role. */
  role: string;
  type: "approve";
};

/** What an onboarding command did. */
export interface OnboardingOutcome {
  /** The next step of a connection, for the person to take outside the site. */
  next?: ConnectStep | null;
  /** One line on what happened. */
  note?: string | null;
  /** Onboarding after it. */
  view: OnboardingView;
}

/** Onboarding as the owner sees it (spec 3.5 §7.18). */
export interface OnboardingView {
  /** Stage 5. */
  charter?: CharterReview | null;
  /** Stage 8. */
  connections: ConnectionsView;
  /** The stage to do now; `done` once the site runs. */
  current: string;
  /** Stage 8. */
  deploy: DeployView;
  /** Stage 6. */
  design: DesignView;
  /** Stage 2. */
  discovery: DiscoveryView;
  /** Stage 7. */
  gate?: GateView | null;
  /** Stage 4. */
  interview: InterviewView;
  /** Stage 3, for a site on a legacy runtime. */
  protect?: ProtectView | null;
  /** Stage 9. */
  run?: RunView | null;
  /** The site's name. */
  siteName: string;
  /** The nine stages. */
  stages: StageLine[];
  /** Stage 1. */
  start: StartView;
}

/** A page a release changed (spec 3.3 §7.5). */
export interface PageLine {
  /** How it changed. */
  change: Pill;
  /** The page. */
  path: string;
}

/** One page of the preview, with how it changes (spec 3.3 §7.5). */
export interface PageView {
  /** How it changes. */
  change: Pill;
  /** The page as Markdown. */
  markdown?: string | null;
  /** The page's address. */
  path: string;
  /** Its title. */
  title: string;
}

/** A status word and its tone (spec 3.3 §9). */
export interface Pill {
  /** The word, in sentence case. */
  text: string;
  /** Its tone. */
  tone: Tone;
}

/** A page of the live preview. */
export interface PreviewPage {
  /** What it is for. */
  summary: string;
  /** The page's title. */
  title: string;
}

/** The live preview, rebuilt after each answer (spec 3.5 §7.18 stage 4). */
export interface PreviewView {
  /** The About page as built, in Markdown. */
  about?: string | null;
  /** How many confirmed facts it shows. */
  facts: number;
  /** The pages release 1 will have. */
  pages: PreviewPage[];
  /** Whether the preview is on the host. */
  published: boolean;
  /** When it was rebuilt. */
  rebuiltAt: string;
  /** The site's name. */
  siteName: string;
}

/** Stage 3: the freeze, for a site on a legacy runtime. */
export interface ProtectView {
  /** The owner's decision. */
  freeze?: boolean | null;
  /** What the freeze does. */
  text: string;
  /** The legacy site. */
  url: string;
}

/** One signed release (spec 3.5 §7.7). */
export interface ReleaseLine {
  /** When it was built. */
  at: string;
  /** How many other files it changed. */
  otherFiles: number;
  /** The pages it changed. */
  pages: PageLine[];
  /** Its number. */
  seq: number;
  /** What made it, in words. */
  source: string;
}

/** A review action. */
export type ReviewCommand = {
  type: "compose";
  /** The Monday. */
  week?: string | null;
} | {
  type: "send";
  /** The Monday of the review. */
  week: string;
};

/** One review in the list. */
export interface ReviewLine {
  /** Where and when its summary went. */
  sent?: string | null;
  /** Its title. */
  title: string;
  /** The Monday its week starts. */
  week: string;
}

/** The weekly review page. */
export interface ReviewPage {
  /** Whether the viewer can compose one now. */
  canCompose: boolean;
  /** The owner's channel the summary goes to. */
  channel?: string | null;
  /** The review shown. */
  current?: ReviewView | null;
  /** When the next one is due. */
  nextDue: string;
  /** Every review, newest first. */
  reviews: ReviewLine[];
}

/** One week's review (spec 3.5 §7.21). */
export interface ReviewView {
  /** Alarms that waited for the review. */
  alarms: string[];
  /** The charter version in force. */
  charterVersion?: number | null;
  /** Demotions, with why. */
  demotions: Line[];
  /** Experiments and their evidence. */
  experiments: string[];
  /** The week at a glance. */
  headline: Line[];
  /** Incidents. */
  incidents: string[];
  /** KPIs against baselines and targets, with trends. */
  kpis: KpiCard[];
  /** Leads captured and handed off. */
  leads: string[];
  /** The review as a document, in Markdown. */
  markdown: string;
  /** Money. */
  money?: MoneyView | null;
  /** Promotions the jobs earned. */
  promotions: OfferLine[];
  /** Questions for the owner. */
  questions: string[];
  /** Where and when its summary went. */
  sent?: string | null;
  /** The line sent to the owner's channel. */
  summary: string;
  /** "Week of 19 October". */
  title: string;
  /**
   * What the constant-update engine did: updates, deliveries, what it held and its cost;
   * empty when the site runs no engine (spec 3.5 §7.8.3).
   */
  updates: Line[];
  /** The Monday its week starts. */
  week: string;
  /** What each agent did. */
  work: WorkLine[];
}

/** Stage 9: the 30/60/90-day plan (spec 3.5 §7.18). */
export interface RunView {
  /** When the first weekly review comes. */
  firstReview: string;
  /** Days 1–30, 31–60 and 61–90. */
  phases: string[][];
  /** The levels the jobs can earn, offered for your decision. */
  promotions: Line[];
  /** Until when every job suggests. */
  proposeUntil: string;
}

/** A schedule as the form shows it. */
export interface ScheduleInput {
  /** Not before. */
  embargoUntil?: string | null;
  /** When to withdraw it. */
  expiresAt?: string | null;
  /** When to publish. */
  publishAt?: string | null;
  /** When to go back to the previous version. */
  revertAt?: string | null;
}

/** One section of the charter as a card (spec 3.5 §7.18 stage 5). */
export interface SectionCard {
  /** What can be changed here; empty when nothing can. */
  fields: FieldSpec[];
  /** The section's id. */
  id: string;
  /** What the draft says. */
  lines: Line[];
  /** A rule that applies to it. */
  note?: string | null;
  /** Whether the owner has read it. */
  reviewed: boolean;
  /** What it decides, in one line. */
  summary: string;
  /** Its heading. */
  title: string;
}

/** What the setup screen shows (spec 3.5 §7.17). */
export interface SetupView {
  /** What visitors see until the site is launched. */
  holding: string;
  /** Whether this site asks for a setup code at all. */
  needed: boolean;
  /** Whether the owner's device has paired. */
  paired: boolean;
  /** The site's name. */
  siteName: string;
  /** Where the code is. */
  whereCode: string;
}

/** One signal from the public side, typed and screened (spec 3.5 §7.10). */
export interface SignalLine {
  /** How many arrived. */
  count: number;
  /** The latest. */
  lastAt: string;
  /** What it is. */
  text: string;
}

/** Signals waiting in the back office. */
export interface SignalsView {
  /** The latest open ones. */
  lines: SignalLine[];
  /** How many are open. */
  open: number;
}

/** A signature the charter needs (spec 3.5 A-I23). */
export interface SignerLine {
  /** The people who hold it. */
  holders: string[];
  /** The role's name. */
  label: string;
  /** The role, as sent back to sign. */
  role: string;
  /** When. */
  signedAt?: string | null;
  /** Who signed in it, if anyone. */
  signedBy?: string | null;
}

/** An owner's decision on the site view. */
export type SiteCommand = {
  /** The job. */
  job: string;
  type: "promote";
} | {
  /** The job. */
  job: string;
  /** Why. */
  reason: string;
  type: "lower";
} | {
  /** `release` or `discard`. */
  decision: HeldDecision;
  /** The item. */
  id: string;
  type: "item";
} | {
  /** `release` or `discard`. */
  decision: HeldDecision;
  /** The delivery. */
  delivery: number;
  type: "delivery";
} | {
  /** The alarm. */
  alarm: string;
  type: "acknowledge";
};

/** What a site decision did. */
export interface SiteOutcome {
  /** One line on what happened. */
  note?: string | null;
  /** The view after it. */
  view: SiteViewModel;
}

/** The site as every surface shows it (spec 3.5 §7.24). */
export interface SiteViewModel {
  /** Open alarms. */
  alarms: AlarmLine[];
  /** Held batches. */
  held: HeldView;
  /** The jobs and their levels. */
  jobs: JobLine[];
  /** This week's KPIs. */
  kpis: KpiCard[];
  /** Whether it is live. */
  launched: boolean;
  /** The site's name. */
  name: string;
  /** Its public address. */
  origin: string;
  /** Whether the viewer decides as an owner. */
  owner: boolean;
  /** The latest releases, newest first. */
  releases: ReleaseLine[];
  /** Signals from the public side. */
  signals: SignalsView;
  /** Its status. */
  status: StatusView;
  /** The wallet, when the site has one. */
  wallet?: WalletView | null;
}

/** One stage of onboarding (spec 3.5 §7.18). */
export interface StageLine {
  /** The stage's id. */
  id: string;
  /** 1 to 9. */
  number: number;
  /** Where it stands. */
  state: StageState;
  /** Its name. */
  title: string;
}

/** A stage's place in the list. */
export type StageState = "done" | "current" | "later";

/** Where onboarding starts (spec 3.5 §7.18 "Starting point"). */
export type StartFrom = "site" | "presence" | "nothing";

/** Stage 1. */
export interface StartView {
  /** Where it started, in words. */
  from?: string | null;
  /** Whether urgent alarms have a phone number. */
  phone: boolean;
  /** Whether onboarding started. */
  started: boolean;
}

/** The site's status (spec 3.5 §7.5). */
export interface StatusView {
  /** Why, in words. */
  reason?: string | null;
  /** Since when. */
  since?: string | null;
  /** The state, as a pill. */
  state: Pill;
}

/** A theme's colours. */
export interface Swatch {
  /** Buttons and highlights. */
  accent: string;
  /** Page background. */
  background: string;
  /** Rules and outlines. */
  border: string;
  /** Links. */
  link: string;
  /** Body text. */
  text: string;
}

/**
 * The tone of a pill: a word always comes with it, so status is never colour alone (spec
 * 3.3 §9).
 */
export type Tone = "good" | "waiting" | "bad" | "info" | "neutral";

/** `POST /surface/website/editor/drafts/{id}/media`. */
export interface UploadRequest {
  /** Drafted alt text; a person confirms it before the accessibility check passes. */
  alt?: string | null;
  /** The file, base64-encoded. */
  dataBase64: string;
  /** Whether the image was generated; it is then labelled as an illustration. */
  generated?: boolean;
  /** The file's name. */
  name: string;
}

/** The wallet (spec 3.5 §7.14). */
export interface WalletView {
  /** What is available. */
  balance: string;
  /** The month's caps together. */
  budget: string;
  /** Each category. */
  categories: CategoryLine[];
  /** Payments held for the owner. */
  held: HeldPayment[];
  /** Spent this month. */
  spent: string;
}

/** `GET /surface/website`. */
export interface WebsiteHome {
  /** Whether the site is live. */
  launched: boolean;
  /** The site's name. */
  name: string;
  /** Whether the site asks for a setup code, and its owner has not claimed it. */
  needsSetup: boolean;
  /** Its public address. */
  origin: string;
  /** The caller's roles in the charter: `owner`, `publisher`, `editor`, `approver`, `auditor`. */
  roles: string[];
}

/** What each agent did. */
export interface WorkLine {
  /** The agent. */
  agent: string;
  /** What it did. */
  items: string[];
}
