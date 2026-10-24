/**
 * Typed seams for what the web home shows once the kernel serves it (spec 3.3 §10). Each
 * seam is an interface the page renders through when one is given; without it the page
 * shows what exists read-only. The routes arrive with the kernel work packages named below;
 * wiring one is a client implementing the interface, with no change to the screens.
 */
import type { Mode } from "./generated/api";

/** K3: quiet time, the daily spending limit and the mode, changed while the kernel runs. */
export interface LimitsSeam {
  setQuietTime(start: string, end: string): Promise<void>;
  setDailyLimit(microUsd: number | null): Promise<void>;
  setMode(mode: Mode): Promise<void>;
}

/** K4: "While you were away" over the `/proc` ledger, with a cursor per device. */
export interface WhileAwaySeam {
  read(since: string): Promise<WhileAway>;
}

/** What K4 adds to the engine's briefing. */
export interface WhileAway {
  /** Nothing proactive ran and nothing notified during quiet time. */
  quietRespected: boolean;
  /** Where each line's record opens, by the line's place in the ledger. */
  records: Record<number, string>;
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
}

/** Every seam the page renders through when given. */
export interface Seams {
  limits?: LimitsSeam;
  whileAway?: WhileAwaySeam;
  memory?: MemorySeam;
  record?: RecordSeam;
  devices?: DevicesSeam;
}
