/**
 * A screen action said aloud (spec 3.3 §5.6): "Heard “snooze news digest”", the engine's
 * cancel window counting down, what will happen in the kernel's words, and Cancel; then how
 * it ended.
 */
import { useEffect, useState } from "react";
import { verbLabels } from "./labels";
import { trusted } from "./trust";

export type SpokenAction =
  | { kind: "pending"; actionId: string; heard?: string; heading: string; cancelUntil: string }
  | { kind: "ended"; done: boolean; heard?: string; heading?: string; text: string };

export function VoiceAction({ action, onCancel }: { action: SpokenAction; onCancel: (actionId: string) => void }) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (action.kind !== "pending") return;
    const timer = window.setInterval(() => setNow(Date.now()), 250);
    return () => window.clearInterval(timer);
  }, [action.kind]);
  const pending = action.kind === "pending";
  const seconds = pending ? Math.max(0, Math.ceil((Date.parse(action.cancelUntil) - now) / 1000)) : 0;
  const pill = pending ? `Snoozing in ${seconds} s` : action.done ? verbLabels.done : verbLabels.cancelled;
  if (!pending && !action.heading) {
    return (
      <section className="voice-action voice-action-plain" aria-live="polite">
        {action.heard && <p className="voice-action-heard">{verbLabels.heard(action.heard)}</p>}
        <p className="voice-action-text">{action.text}</p>
      </section>
    );
  }
  return (
    <section className="voice-action" aria-live="polite">
      <div className="voice-action-top">
        {action.heard && <p className="voice-action-heard">{verbLabels.heard(action.heard)}</p>}
        <span className={`voice-action-pill${pending ? " counting" : action.done ? " done" : ""}`}>{pill}</span>
      </div>
      <h2 className="voice-action-heading">{action.heading}</h2>
      <div className="voice-action-foot">
        <span className="voice-action-sub">
          {pending ? `${verbLabels.comingUp} · ${verbLabels.changeItBack}` : action.text}
        </span>
        {pending && (
          <button
            type="button"
            className="button button-secondary"
            onClick={(event) => {
              if (trusted(event)) onCancel(action.actionId);
            }}
          >
            {verbLabels.cancel}
          </button>
        )}
      </div>
    </section>
  );
}
