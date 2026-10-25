/**
 * Typed seams for what the web home shows once the kernel serves it (spec 3.3 §10). Each
 * seam is an interface the page renders through when one is given; without it the page
 * shows what exists read-only. The routes arrive with the kernel work packages named below;
 * wiring one is a client implementing the interface, with no change to the screens.
 */
import type { AreaMode, AreaSettings, AwayPage, AwayRecord, ChannelWaiting, QuietWindow } from "./kernel";

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
  /** The agent that noticed a guess. */
  noticedBy?: string;
}

/** An overnight tidy-up that can be undone. */
export interface TidyUp {
  id: string;
  text: string;
}

/** K5: what agents remember, confirm, fix, forget and undo. */
export interface MemorySeam {
  facts(): Promise<{ facts: MemoryFact[]; tidyUp?: TidyUp }>;
  confirm(id: string): Promise<void>;
  fix(id: string, text: string): Promise<void>;
  forget(id: string): Promise<void>;
  undo(tidyUp: string): Promise<void>;
}

/** One action on record, as a sentence. */
export interface RecordLine {
  at: string;
  atText: string;
  text: string;
}

/** The record check `adk-rust audit verify` makes. */
export interface RecordCheck {
  intact: boolean;
  count: number;
  checkedAt: string;
}

/** K5: the record of what agents did, and its check. */
export interface RecordSeam {
  check(): Promise<RecordCheck>;
  recent(): Promise<RecordLine[]>;
}

/** A phone or app allowed to decide (K2 pairing). */
export interface Device {
  id: string;
  name: string;
  /** What it may do, in plain words: "yes once, or no". */
  may: string;
}

/**
 * K2: where the person can say yes from. `HttpClient.devices()` serves it over the kernel's
 * pairing routes; `list` answers `undefined` when the kernel pairs no devices.
 */
export interface DevicesSeam {
  list(): Promise<Device[] | undefined>;
  remove(id: string): Promise<void>;
  /** Starts pairing; resolves to the code the person enters on the device. */
  pair(): Promise<string>;
  /** Chat accounts that asked to pair and wait for their code to be entered (K1). */
  waiting?(): Promise<ChannelWaiting[]>;
  /**
   * Pairs the chat account that was sent `code`, as `adk-rust pair accept <code>` does (K1):
   * the kernel takes it from the person only, with the `operator` scope.
   */
  accept?(code: string): Promise<void>;
}

/** Every seam the page renders through when given. */
export interface Seams {
  limits?: LimitsSeam;
  whileAway?: WhileAwaySeam;
  memory?: MemorySeam;
  record?: RecordSeam;
  devices?: DevicesSeam;
}
