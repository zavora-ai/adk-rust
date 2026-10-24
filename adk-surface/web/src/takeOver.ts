/**
 * Take-over entries (spec 3.3 §8.2, §10 H2): a tool asks the person to type a secret. The
 * inbox entry names the tool that asks, the kind of secret and what it is for; never a value.
 */
import type { CallView } from "./generated/api";

/** The reserved tool name of a take-over entry (`adk_harness::take_over::TAKE_OVER_TOOL`). */
export const TAKE_OVER_TOOL = "harness:take-over";

/** What a take-over entry asks for. */
export interface TakeOverAsk {
  tool: string;
  secret: "password" | "oneTimeCode" | "other";
  resource: string;
}

/** The ask of `call`, or `undefined` for any other call. */
export function takeOverOf(call: CallView): TakeOverAsk | undefined {
  if (call.tool !== TAKE_OVER_TOOL) return undefined;
  const args = (call.args ?? {}) as Record<string, unknown>;
  const text = (key: string) => (typeof args[key] === "string" ? (args[key] as string) : "");
  const secret = text("secret");
  return {
    tool: text("tool"),
    secret: secret === "password" || secret === "oneTimeCode" ? secret : "other",
    resource: text("resource"),
  };
}
