import { describe, expect, it } from "vitest";
import { topologyActivity } from "../src/topologyActivity";
import type { RuntimeEvent, TopologyMember } from "../src/types";

const members = [
  { name: "supervisor", description: "", coordinator: true, capabilities: {} },
  { name: "technical", description: "", coordinator: false, capabilities: {} },
] as TopologyMember[];

describe("topologyActivity", () => {
  it("animates the coordinator at the start of a run", () => {
    expect(topologyActivity(members, [], true)).toEqual({
      activeMember: "supervisor",
      activeEdge: undefined,
    });
  });

  it("animates the exact incoming handoff edge and target", () => {
    const events: RuntimeEvent[] = [{
      id: "handoff",
      author: "supervisor",
      actions: { transfer_to_agent: "technical" },
    }];
    expect(topologyActivity(members, events, true)).toEqual({
      activeMember: "technical",
      activeEdge: "supervisor->technical",
    });
  });

  it("stops topology motion after the run completes", () => {
    expect(topologyActivity(members, [], false)).toEqual({});
  });
});
