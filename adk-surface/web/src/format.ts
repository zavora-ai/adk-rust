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
