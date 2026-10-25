/**
 * Typed seams for what the web home shows once the kernel serves it (spec 3.3 §10). Each
 * seam is an interface the page renders through when one is given; without it the page
 * shows what exists read-only. The routes arrive with the kernel work packages named below;
 * wiring one is a client implementing the interface, with no change to the screens.
 */
import type {
  AreaMode,
  AreaSettings,
  AwayPage,
  AwayRecord,
  ChannelAccount,
  ChannelWaiting,
  KernelRecordLine,
  QuietWindow,
} from "./kernel";

/**
 * K3: each area's quiet time, daily spending limit and mode, read and changed while the kernel
 * runs (spec 3.3 §10 K3). Only the person changes them: the kernel refuses a caller without the
 * `operator` scope and any paired device's token, and the page shows its answer.
 */
export interface LimitsSeam {
  /** Every area's settings in force; `undefined` when the server serves no kernel settings. */
  areas(): Promise<AreaSettings[] | undefined>;
  /** One area's settings in force, read just before a change so its old value is the kernel's. */
  area(partition: string): Promise<AreaSettings>;
  /** Sets the area's quiet time; `null` turns it off. Resolves to the area after the change. */
  setQuietTime(partition: string, window: QuietWindow | null): Promise<AreaSettings>;
  /** Sets the area's daily spending limit in micro-USD; `null` removes it. */
  setSpendingLimit(partition: string, microUsd: number | null): Promise<AreaSettings>;
  setMode(partition: string, mode: AreaMode): Promise<AreaSettings>;
}

/**
 * K4: "While you were away" over the `/proc` ledger, with a cursor per device (spec 3.3 §10 K4).
 * Reading never moves the cursor; only `markSeen` does.
 */
export interface WhileAwaySeam {
  /** Everything after this viewer's cursor; `undefined` when the server keeps no ledger. */
  read(): Promise<AwayPage | undefined>;
  /** Marks every line through ledger row `through` seen. */
  markSeen(through: number): Promise<void>;
  /** One line's record. */
  record(seq: number): Promise<AwayRecord>;
}

/** How a remembered fact came to be (spec 3.3 §8.1 Memory). */
export type Provenance = "youSaidThis" | "youConfirmedThis" | "aGuess";

/** One fact an agent remembers. */
export interface MemoryFact {
  id: string;
  text: string;
  provenance: Provenance;
  /** What noticed a guess: an agent, or an area. */
  noticedBy?: string;
}

/** An overnight tidy-up that can be undone, in words: "Overnight, your agents merged two notes into one." */
export interface TidyUp {
  id: string;
  text: string;
}

/** What undoing a tidy-up did: how many notes came back, were forgotten, and were kept as changed since. */
export interface UndoOutcome {
  restored: number;
  forgotten: number;
  kept: number;
}

/**
 * K5: what agents remember, confirm, fix, forget and undo (spec 3.3 §10 K5). Only the person
 * changes it: the kernel refuses a paired device. `facts` answers `undefined` where the agents
 * keep no long-term memory.
 */
export interface MemorySeam {
  facts(): Promise<{ facts: MemoryFact[]; tidyUp?: TidyUp } | undefined>;
  confirm(id: string): Promise<void>;
  fix(id: string, text: string): Promise<void>;
  forget(id: string): Promise<void>;
  undo(tidyUp: string): Promise<UndoOutcome | void>;
}

/** One action on record, as a sentence. */
export interface RecordLine {
  /** Its place on the record. */
  seq?: number;
  at: string;
  atText: string;
  text: string;
}

/** The record check `adk-rust audit verify` makes. */
export interface RecordCheck {
  intact: boolean;
  count: number;
  checkedAt: string;
  /** Why the check failed, in the verifier's words. */
  problem?: string;
  /**
   * The latest records no signed checkpoint covers yet, when that is all the check found: they
   * are on record, but the check can't vouch for them until the log signs its next checkpoint.
   */
  unsigned?: number;
}

/**
 * K5: the record of what agents did, and its check (spec 3.3 §10 K5). `check` answers
 * `undefined` where the kernel keeps no signed record.
 */
export interface RecordSeam {
  check(): Promise<RecordCheck | undefined>;
  /** The latest `limit` actions on record, newest first, as the kernel lists them. */
  lines(limit: number): Promise<KernelRecordLine[]>;
}

/** What one agent of one area spent today, from the kernel's daily totals. */
export interface AgentSpend {
  /** The agent: the target its processes run, as the roster names it. */
  agent: string;
  /** Its area: a partition, `default` on a kernel without areas. */
  area: string;
  /** Micro-USD today (UTC), as the area's "spent today" counts it. */
  spentToday: number;
}

/**
 * The roster's spending (spec 3.3 §12 phase 4): each agent's spend today in each area, from the
 * same daily totals as each area's "spent today", so the agents of an area add up to it.
 * `undefined` without a kernel.
 */
export interface RosterSeam {
  spending(): Promise<AgentSpend[] | undefined>;
}

/** A phone or app allowed to decide (K2 pairing). */
export interface Device {
  id: string;
  name: string;
  /** What it may do, in plain words: "yes once, or no". */
  may: string;
}

/** A pairing code as the kernel issued it (spec 3.3 §10 K2). */
export interface PairingIssued {
  /** What the person types on the device. */
  code: string;
  /** The link that opens the pairing screen with the code, when the server has a home URL. */
  url?: string;
  /** When the code lapses. */
  expiresAt?: string;
}

/**
 * K2: where the person can say yes from. `HttpClient.devices()` serves it over the kernel's
 * pairing routes; `list` answers `undefined` when the kernel pairs no devices.
 */
export interface DevicesSeam {
  list(): Promise<Device[] | undefined>;
  remove(id: string): Promise<void>;
  /** Starts pairing a phone; resolves to the code, the link that opens it, and when it lapses. */
  pair(): Promise<PairingIssued>;
  /**
   * The pairing link as a QR code, an `image/svg+xml` data URL (spec 3.3 §8.3); `undefined`
   * when the server serves no phone.
   */
  qr?(url: string): Promise<string | undefined>;
  /**
   * Whether "Use your face or fingerprint on this computer" is offered (spec 3.3 §8.2): the
   * server serves the phone, this browser has a platform passkey, and none is set up yet.
   */
  canAddPasskey?(): Promise<boolean>;
  /** Sets up the person's passkey on this computer, for high-risk answers in the home. */
  addPasskey?(): Promise<void>;
  /** Chat accounts that asked to pair and wait for their code to be entered (K1). */
  waiting?(): Promise<ChannelWaiting[]>;
  /**
   * Pairs the chat account that was sent `code`, as `adk-rust pair accept <code>` does (K1):
   * the kernel takes it from the person only, with the `operator` scope. Resolves to the
   * account the kernel paired, which the page names.
   */
  accept?(code: string): Promise<ChannelAccount | undefined>;
}

/** Every seam the page renders through when given. */
export interface Seams {
  limits?: LimitsSeam;
  whileAway?: WhileAwaySeam;
  memory?: MemorySeam;
  record?: RecordSeam;
  devices?: DevicesSeam;
  roster?: RosterSeam;
}
