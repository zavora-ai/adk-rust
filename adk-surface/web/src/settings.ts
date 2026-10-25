/**
 * Each area's quiet time, daily spending limit and mode (spec 3.3 §8.1, §10 K3) in the words
 * the engine's status line uses (`adk_surface::words`), so the header reads the same whether
 * the engine or the kernel's settings fill it.
 */
import { insideQuiet, onThisDevice } from "./away";
import { dollars } from "./format";
import type { Mode } from "./generated/api";
import type { AreaMode, AreaSettings, QuietWindow } from "./kernel";
import { modes } from "./labels";

const fromKernel: Record<AreaMode, Mode> = {
  read_only: "justLook",
  plan: "suggest",
  ask: "askFirst",
  auto: "goAhead",
};

/** The §8.1 mode a kernel mode is. */
export function modeOf(mode: AreaMode): Mode {
  return fromKernel[mode];
}

/** The kernel mode a §8.1 mode sets. */
export function kernelMode(mode: Mode): AreaMode {
  return (Object.keys(fromKernel) as AreaMode[]).find((key) => fromKernel[key] === mode) ?? "ask";
}

/** The mode's word: "Ask first". */
export function modeWord(mode: Mode): string {
  return modes.find((entry) => entry.mode === mode)?.label ?? mode;
}

/** How a mode reads as a status (`words::mode_status`): "Asks first". */
export function modeStatus(mode: Mode): string {
  switch (mode) {
    case "justLook":
      return "Just looks";
    case "suggest":
      return "Suggests";
    case "askFirst":
      return "Asks first";
    case "goAhead":
      return "Goes ahead";
  }
}

/** A quiet window as the home writes it: "22:00–07:00". */
export function windowText(window: QuietWindow): string {
  return `${window.start}–${window.end}`;
}

/** Spend against the limit (`words::spent_of`, `words::spent_today`): "$1.84 of $5.00 today". */
export function spentText(spent: number, limit: number | undefined | null): string {
  return limit === undefined || limit === null ? `${dollars(spent)} today` : `${dollars(spent)} of ${dollars(limit)} today`;
}

/** Quiet time on the status line (`words::quiet_from`, `words::quiet_until`). */
export function quietText(window: QuietWindow, now: Date): string {
  return insideQuiet(window, now) ? `Quiet until ${window.end}` : `Quiet from ${window.start}`;
}

/** An area's name as the person reads it: `default` is no area of its own. */
export function areaName(partition: string): string | undefined {
  return partition === "default" ? undefined : partition.charAt(0).toUpperCase() + partition.slice(1);
}

/**
 * The status line of one area (spec 3.3 §5.1): its mode, spend against its limit, quiet time,
 * read on this device's clock (`host`: the server's offset, where the kernel keeps a window
 * without one).
 */
export function areaStatus(area: AreaSettings, now: Date, host?: number): string[] {
  const name = areaName(area.partition);
  const mode = area.mode ? modeStatus(modeOf(area.mode)) : undefined;
  const window = area.quietTime[0] && onThisDevice(area.quietTime[0], host);
  return [
    mode && (name ? `${name} · ${mode.charAt(0).toLowerCase()}${mode.slice(1)}` : mode),
    spentText(area.spentToday, area.dailySpendingLimit),
    window && quietText(window, now),
  ].filter((part): part is string => !!part);
}

/**
 * "Was 22:00–07:00. Now 23:00–06:00." from what the setting read before and after a change, on
 * this device's clock.
 */
export function quietValue(area: AreaSettings, host?: number): string {
  return area.quietTime.length === 0
    ? "Off"
    : area.quietTime.map((window) => windowText(onThisDevice(window, host))).join(", ");
}

/** The daily limit as the home writes it, or "No limit". */
export function limitValue(area: AreaSettings): string {
  return area.dailySpendingLimit === undefined || area.dailySpendingLimit === null
    ? "No limit"
    : dollars(area.dailySpendingLimit);
}

/** The mode's word, or "Not set". */
export function modeValue(area: AreaSettings): string {
  return area.mode ? modeWord(modeOf(area.mode)) : "Not set";
}
