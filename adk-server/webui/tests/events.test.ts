import { describe, expect, it } from "vitest";
import { upsertEvent } from "../src/App";
import type { RuntimeEvent } from "../src/types";

function event(text: string | undefined, partial: boolean): RuntimeEvent {
  return {
    id: "response-1",
    author: "billing",
    partial,
    content: text === undefined ? undefined : { role: "model", parts: [{ text }] },
  };
}

describe("upsertEvent", () => {
  it("accumulates streaming text deltas", () => {
    const events = upsertEvent([event("Verify ", true)], event("the invoice.", true));
    expect(events[0]?.content?.parts).toEqual([{ text: "Verify the invoice." }]);
  });

  it("retains accumulated text when a provider completes with null content", () => {
    const events = upsertEvent([event("Verify the invoice.", true)], event(undefined, false));
    expect(events[0]?.partial).toBe(false);
    expect(events[0]?.content?.parts).toEqual([{ text: "Verify the invoice." }]);
  });
});
