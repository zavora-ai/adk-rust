/**
 * The fork dialog's edits (spec 3.3 §12): each changed channel's JSON, checked here before the
 * server checks it again. An edit overwrites the whole channel; no reducer runs.
 */
import { workflowLabels } from "./labels";

/** The largest value one edit may set, in bytes, as the server's `FORK_VALUE_LIMIT`. */
export const FORK_VALUE_LIMIT = 64 * 1024;

export interface Checked {
  /** The edits that parsed, by key. */
  set: Record<string, unknown>;
  /** Why an edit is refused, by key. */
  errors: Record<string, string>;
  /** An edit of a key the graph does not declare, which the dialog shows above its controls. */
  notAChannel?: string;
}

/**
 * Checks the edited JSON text of each key against the graph's `channels`.
 *
 * ```ts
 * check({ note: '"re-run"' }, ["note"]).set; // { note: "re-run" }
 * ```
 */
export function check(edits: Record<string, string>, channels: readonly string[]): Checked {
  const checked: Checked = { set: {}, errors: {} };
  for (const [key, text] of Object.entries(edits)) {
    if (channels.length > 0 && !channels.includes(key)) {
      checked.notAChannel = key;
      continue;
    }
    if (new TextEncoder().encode(text).length > FORK_VALUE_LIMIT) {
      checked.errors[key] = workflowLabels.tooLarge;
      continue;
    }
    try {
      checked.set[key] = JSON.parse(text);
    } catch {
      checked.errors[key] = workflowLabels.notValidJson;
    }
  }
  return checked;
}

/** Whether a fork may be sent: something parsed or nothing edited, and nothing refused. */
export function canFork(checked: Checked): boolean {
  return Object.keys(checked.errors).length === 0 && checked.notAChannel === undefined;
}
