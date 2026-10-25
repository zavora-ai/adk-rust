/**
 * "What your agents remember" in the home's words (spec 3.3 §5.1, §8.1, §8.2): each fact's
 * provenance, the overnight tidy-up "Undo" applies to, and what an undo did. The kernel's
 * memory routes (§10 K5) carry counts and ids only; the words are made here.
 */
import { dayMonth, hhmm, sameDay } from "./format";
import type { KernelTidyUp, MemoryResponse, RememberedFact, TidyUpUndo } from "./kernel";
import { undoable } from "./kernel";
import type { MemoryFact, Provenance, TidyUp, UndoOutcome } from "./seams";
import { areaName } from "./settings";

const NUMBERS = ["no", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten"];

/** A count in words up to ten, as a sentence reads it: "two notes", "11 notes". */
export function count(n: number, one: string, many: string): string {
  return `${NUMBERS[n] ?? n.toLocaleString("en-US")} ${n === 1 ? one : many}`;
}

function capital(text: string): string {
  return text.charAt(0).toUpperCase() + text.slice(1);
}

/** The home's provenance of a kernel fact (`FactOrigin`): a guess is "A guess". */
export function provenanceOf(origin: RememberedFact["origin"]): Provenance {
  switch (origin) {
    case "youSaidThis":
      return "youSaidThis";
    case "youConfirmedThis":
      return "youConfirmedThis";
    default:
      return "aGuess";
  }
}

/** A kernel fact as the home lists it; what noticed a guess is named as the person reads it. */
export function factOf(fact: RememberedFact): MemoryFact {
  const noticedBy = fact.noticedBy ? (areaName(fact.noticedBy) ?? capital(fact.noticedBy)) : undefined;
  return { id: fact.id, text: fact.text, provenance: provenanceOf(fact.origin), noticedBy };
}

/**
 * When a tidy-up ran, as a sentence starts: "Overnight," for one between 18:00 and 09:00 in
 * the last day, "Today at 14:02," on today's date, else "On 23 October,".
 */
function whenText(at: Date, now: Date): string {
  const hour = at.getHours();
  const night = hour >= 18 || hour < 9;
  if (night && now.getTime() - at.getTime() < 24 * 3_600_000) return "Overnight,";
  if (sameDay(at, now)) return `Today at ${hhmm(at)},`;
  return `On ${dayMonth(at)},`;
}

/**
 * What a tidy-up did, in one sentence (spec 3.3 §5.1): "Overnight, your agents merged two notes
 * into one." Its counts are the kernel's: `superseded` notes replaced by `merged` new ones,
 * `forgotten` notes, `distilled` things learned from conversations nobody had read.
 */
export function tidyUpText(tidyUp: KernelTidyUp, now: Date = new Date()): string {
  const area = areaName(tidyUp.area);
  const who = area ? `your agents at ${area.toLowerCase()}` : "your agents";
  const did: string[] = [];
  if (tidyUp.superseded > 0 && tidyUp.merged > 0) {
    did.push(`merged ${count(tidyUp.superseded, "note", "notes")} into ${NUMBERS[tidyUp.merged] ?? tidyUp.merged}`);
  } else if (tidyUp.merged > 0) {
    did.push(`wrote ${count(tidyUp.merged, "note", "notes")} from others`);
  }
  if (tidyUp.forgotten > 0) did.push(`forgot ${count(tidyUp.forgotten, "note", "notes")}`);
  if (tidyUp.distilled > 0) {
    did.push(`learned ${count(tidyUp.distilled, "thing", "things")} from your conversations`);
  }
  const list = did.length > 1 ? `${did.slice(0, -1).join(", ")} and ${did[did.length - 1]}` : (did[0] ?? "tidied their notes");
  return `${whenText(new Date(tidyUp.at), now)} ${who} ${list}.`;
}

/** The memory as the home shows it: live facts, and the latest tidy-up "Undo" applies to. */
export function memoryOf(read: MemoryResponse, now: Date = new Date()): { facts: MemoryFact[]; tidyUp?: TidyUp } {
  // Newest first, as the kernel lists them.
  const latest = read.tidyUps.find(undoable);
  return {
    facts: read.facts.map(factOf),
    tidyUp: latest ? { id: latest.id, text: tidyUpText(latest, now) } : undefined,
  };
}

/** What an undo did, as counts of notes. */
export function undoOutcome(undo: TidyUpUndo): UndoOutcome {
  return { restored: undo.restored.length, forgotten: undo.forgotten.length, kept: undo.kept.length };
}

/**
 * What an undo did, in words: "Undone. Two notes are back as they were." A note the person
 * changed since the tidy-up is kept as they left it, and the sentence says so.
 */
export function undoText(outcome: UndoOutcome | void): string {
  if (!outcome) return "Undone.";
  const parts = ["Undone."];
  if (outcome.restored > 0) {
    const notes = capital(count(outcome.restored, "note", "notes"));
    parts.push(`${notes} ${outcome.restored === 1 ? "is" : "are"} back as ${outcome.restored === 1 ? "it was" : "they were"}.`);
  }
  if (outcome.kept > 0) {
    const notes = capital(count(outcome.kept, "note", "notes"));
    parts.push(`${notes} you changed since ${outcome.kept === 1 ? "stays as you left it" : "stay as you left them"}.`);
  }
  return parts.join(" ");
}
