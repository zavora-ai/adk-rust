/**
 * The exact command on the phone (spec 3.3 §7.1): the engine's text, which it cuts short for a
 * long call, or the call drawn whole from its arguments once the person expands it — the same
 * call the home's `CallView` draws. The text is escaped where it is shown (S-I9).
 */
import type { CallView } from "../generated/api";

/** `call` in full, from its tool and every argument. */
export function wholeCall(call: CallView): string {
  const args = call.args;
  const json = (value: unknown) => JSON.stringify(value, null, 2) ?? "null";
  if (args !== null && typeof args === "object" && !Array.isArray(args)) {
    const fields = Object.entries(args as Record<string, unknown>).map(
      ([key, value]) => `\n  ${key}: ${json(value).replace(/\n/g, "\n  ")},`,
    );
    return `${call.tool}(${fields.join("")}${fields.length ? "\n" : ""})`;
  }
  return `${call.tool}(${args === null || args === undefined ? "" : json(args)})`;
}

/** What the phone shows of `call`: the engine's text, or the whole call once `expanded`. */
export function shownCall(call: CallView, expanded: boolean): string {
  return call.truncated && expanded ? wholeCall(call) : call.text;
}
