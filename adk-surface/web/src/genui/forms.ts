/**
 * Where each drawn form is once sent (spec 3.3 §7.4 rule 5): read from the server, which words
 * it from the form's decision and its call's result, never from the click.
 */
import { useEffect, useState } from "react";
import type { DecisionCard, DrawnSent, FormState, FormsResponse } from "../generated/api";
import type { HomeClient } from "../client";
import { probe } from "../features";

/** The route of a session's sent forms. */
export function formsPath(session: string): string {
  return `/surface/genui/sessions/${encodeURIComponent(session)}/forms`;
}

/**
 * The state of the form `sent` came from, read again as its decision `card` or its call's
 * result change. A send the agent's rules refused says so itself; a server built without
 * `genui` answers nothing more than the send's own line.
 */
export function useFormState(
  client: HomeClient,
  session: string | undefined,
  sent: DrawnSent | undefined,
  card: DecisionCard | undefined,
  resultSeen: boolean,
): FormState | undefined {
  const [read, setRead] = useState<FormState>();
  const key = sent ? `${sent.surfaceId}@${sent.version}:${sent.callId}` : "";
  useEffect(() => {
    if (!sent || !sent.held || !session) return;
    let live = true;
    probe(() => client.get<FormsResponse>(formsPath(session)))
      .then((page) => {
        const state = page?.forms.find((form) => form.surfaceId === sent.surfaceId && form.version === sent.version);
        if (live && state) setRead(state);
      })
      .catch(() => undefined);
    return () => {
      live = false;
    };
    // The state moves with the decision and the result.
  }, [client, session, key, card?.version, card?.status, resultSeen]);
  if (!sent) return undefined;
  if (!sent.held) {
    return { surfaceId: sent.surfaceId, version: sent.version, status: "refused", text: sent.text, locked: false };
  }
  return (
    read ?? {
      surfaceId: sent.surfaceId,
      version: sent.version,
      status: "waiting",
      text: sent.text,
      locked: true,
      decisionId: sent.decisionId,
    }
  );
}
