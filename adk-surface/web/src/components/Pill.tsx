import type { ReactNode } from "react";

export type PillKind = "waiting" | "done" | "failed" | "decided" | "suggestion" | "question" | "neutral" | "since";

/** A status pill: a word and a colour, never colour alone (spec 3.3 §9). */
export function Pill({ kind, dot = false, children }: { kind: PillKind; dot?: boolean; children: ReactNode }) {
  return (
    <span className={`pill pill-${kind}`}>
      {dot && <i className="dot" aria-hidden="true" />}
      {children}
    </span>
  );
}
