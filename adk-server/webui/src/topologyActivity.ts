import type { RuntimeEvent, TopologyMember } from "./types";

export type TopologyActivity = {
  activeMember?: string;
  activeEdge?: string;
};

/** Resolves the member and incoming edge that should animate during a run. */
export function topologyActivity(
  members: TopologyMember[],
  events: RuntimeEvent[],
  running: boolean,
): TopologyActivity {
  if (!running) return {};
  const coordinator = members.find((member) => member.coordinator) ?? members[0];
  const latestTransfer = [...events].reverse().find((event) => event.actions?.transfer_to_agent);
  const latestMemberEvent = [...events]
    .reverse()
    .find((event) => members.some((member) => member.name === event.author));
  const transferTarget = latestTransfer?.actions?.transfer_to_agent ?? undefined;
  return {
    activeMember: transferTarget ?? latestMemberEvent?.author ?? coordinator?.name,
    activeEdge: transferTarget && latestTransfer?.author
      ? `${latestTransfer.author}->${transferTarget}`
      : undefined,
  };
}
