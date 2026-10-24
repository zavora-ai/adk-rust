import type { CSSProperties } from "react";
import type { AgentRef } from "../generated/api";

/** A rounded monogram in the agent's own colour (spec 3.3 §9). */
export function Avatar({ agent, size = "md" }: { agent: AgentRef; size?: "sm" | "md" | "lg" }) {
  const style = { "--agent": agent.avatar.color } as CSSProperties;
  return (
    <span className={`avatar avatar-${size}`} style={style} aria-hidden="true">
      {agent.avatar.monogram}
    </span>
  );
}
