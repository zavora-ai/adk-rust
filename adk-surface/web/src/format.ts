/** Formatting the page does in the browser, matching the engine's words (spec 3.3 §8). */

/** Micro-USD as dollars and cents, rounded to the nearest cent: `$1.84`. */
export function dollars(microUsd: number): string {
  const cents = Math.floor((microUsd + 5_000) / 10_000);
  return `$${Math.floor(cents / 100)}.${String(cents % 100).padStart(2, "0")}`;
}

/**
 * Dollars as a person types them, in micro-USD, or `undefined` for what is not an amount. A
 * comma or a point followed by one or two digits at the end is the decimal mark, and one
 * followed by three digits groups thousands, so `5,50` and `5.50` are $5.50 and `1.000,50` and
 * `1,000.50` are $1,000.50. Anything else — `1,2,3`, two decimal marks — is refused rather
 * than read as a hundred times the amount.
 */
export function parseDollars(text: string): number | undefined {
  const amount = text.replace(/[$\s]/g, "");
  const match = /^(\d{1,3}(?:([.,])\d{3}(?:\2\d{3})*)?|\d+)(?:([.,])(\d{1,2}))?$/.exec(amount);
  if (!match) return undefined;
  const [, whole, group, mark, cents] = match;
  if (group && mark && group === mark) return undefined;
  const dollars = Number(whole.replace(/[.,]/g, ""));
  const fraction = cents ? Number(cents.padEnd(2, "0")) : 0;
  return dollars * 1_000_000 + fraction * 10_000;
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

/**
 * When something planned happens, as Coming up reads it: "09:00" today, "Mon 09:00" within the
 * week ahead, so a weekday routine seen on a Saturday says Monday; "2 November, 09:00" later.
 */
export function whenAhead(at: Date, now: Date = new Date()): string {
  if (sameDay(at, now)) return hhmm(at);
  if (at.getTime() - now.getTime() < 6 * 24 * 3_600_000) {
    return `${at.toLocaleDateString("en-GB", { weekday: "short" })} ${hhmm(at)}`;
  }
  return `${dayMonth(at)}, ${hhmm(at)}`;
}
