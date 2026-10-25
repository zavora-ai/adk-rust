/** Formatting the page does in the browser, matching the engine's words (spec 3.3 §8). */

/** Micro-USD as dollars and cents, rounded to the nearest cent: `$1.84`. */
export function dollars(microUsd: number): string {
  const cents = Math.floor((microUsd + 5_000) / 10_000);
  return `$${Math.floor(cents / 100)}.${String(cents % 100).padStart(2, "0")}`;
}

/** A time as local `HH:MM`. */
export function hhmm(at: Date): string {
  return `${String(at.getHours()).padStart(2, "0")}:${String(at.getMinutes()).padStart(2, "0")}`;
}

/** A date as "30 November". */
export function dayMonth(at: Date): string {
  return at.toLocaleDateString("en-GB", { day: "numeric", month: "long" });
}

/** Whether two dates fall on the same local day. */
export function sameDay(a: Date, b: Date): boolean {
  return a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate();
}

/**
 * When something started: "22:04" within the last day, as the spec's "Since 22:04" reads the
 * evening before; "23 October, 22:04" further back, so an older time never reads as today's.
 */
export function dayAndTime(at: Date, now: Date = new Date()): string {
  return now.getTime() - at.getTime() < 24 * 3_600_000 ? hhmm(at) : `${dayMonth(at)}, ${hhmm(at)}`;
}
