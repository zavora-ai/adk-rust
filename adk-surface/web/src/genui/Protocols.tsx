import { useCallback, useEffect, useState } from "react";
import type { GenuiCapabilities, RefusedDrawing, ThreadResponse } from "../generated/api";
import type { HomeClient } from "../client";
import { problemText } from "../client";
import { isAbsent } from "../features";
import { visible } from "../safe";
import { genuiLabels } from "./labels";
import "./genui.css";

type Read = { kind: "loading" } | { kind: "absent" } | { kind: "failed"; text: string } | { kind: "read"; claims: GenuiCapabilities };

/** The drawings refused in a session's thread, newest last. */
export function refusedIn(page: ThreadResponse): RefusedDrawing[] {
  return page.entries.flatMap((entry) => entry.items.flatMap((item) => (item.kind === "drawnRefused" ? [item.refusal] : [])));
}

/**
 * The developer panel's Protocols section for agent-drawn UI (spec 3.3 §4, §6.1 `genui`): what
 * this home claims of A2UI, AG-UI and MCP Apps, each claim with its status and scope, the
 * catalogs it renders, and the drawings refused in the session shown, with their technical
 * reasons. A server built without `genui` says so.
 */
export function GenuiProtocols({ client, sessionId }: { client: HomeClient; sessionId?: string }) {
  const [read, setRead] = useState<Read>({ kind: "loading" });
  const [refused, setRefused] = useState<RefusedDrawing[]>([]);
  const load = useCallback(() => {
    setRead({ kind: "loading" });
    client
      .get<GenuiCapabilities>("/surface/genui/capabilities")
      .then((claims) => setRead({ kind: "read", claims }))
      .catch((error: unknown) =>
        setRead(isAbsent(error) ? { kind: "absent" } : { kind: "failed", text: problemText(error) }),
      );
  }, [client]);
  useEffect(load, [load]);
  useEffect(() => {
    if (!sessionId || read.kind !== "read") return;
    let live = true;
    client
      .get<ThreadResponse>(`/surface/sessions/${encodeURIComponent(sessionId)}/thread`)
      .then((page) => live && setRefused(refusedIn(page)))
      .catch(() => live && setRefused([]));
    return () => {
      live = false;
    };
  }, [client, sessionId, read.kind]);

  return (
    <div className="genui-protocols">
      <h2 className="card-label">
        {genuiLabels.heading} <span className="mono muted">{genuiLabels.route}</span>
      </h2>
      {read.kind === "absent" && <p className="muted">{genuiLabels.disabled}</p>}
      {read.kind === "failed" && (
        <div>
          <p>{genuiLabels.cantLoad}</p>
          <p className="mono muted">{visible(read.text)}</p>
          <button type="button" className="button button-secondary" onClick={load}>
            {genuiLabels.retry}
          </button>
        </div>
      )}
      {read.kind === "read" && (
        <>
          {read.claims.protocols.map((protocol) => (
            <section key={protocol.id} aria-label={protocol.name}>
              <h3 className="card-label">
                {protocol.name} <span className="mono muted">{protocol.id}</span>
              </h3>
              <table className="genui-claims">
                <thead>
                  <tr>
                    <th scope="col">{genuiLabels.claim}</th>
                    <th scope="col">{genuiLabels.status}</th>
                    <th scope="col">{genuiLabels.scope}</th>
                  </tr>
                </thead>
                <tbody>
                  {protocol.claims.map((claim) => (
                    <tr key={claim.id}>
                      <td>{claim.label}</td>
                      <td>
                        <span className={`genui-status genui-status-${claim.status}`}>
                          {genuiLabels.statuses[claim.status]}
                        </span>
                      </td>
                      <td>{genuiLabels.scopes[claim.scope]}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </section>
          ))}
          <h3 className="card-label">{genuiLabels.catalogs}</h3>
          {read.claims.catalogs.map((catalog) => (
            <div key={catalog.id} className="genui-catalog">
              <strong>{catalog.label}</strong> <span className="mono muted">{catalog.id}</span>
              <ul className="genui-components">
                {catalog.components.map((component) => (
                  <li key={component}>{component}</li>
                ))}
              </ul>
              <p className="muted">
                {genuiLabels.leftOut} {catalog.leftOut.join(", ")}
              </p>
            </div>
          ))}
          <h3 className="card-label">
            {genuiLabels.refused}
            {refused.length > 0 && <span className="muted"> · {genuiLabels.refusedCount(refused.length)}</span>}
          </h3>
          <p className="muted">{genuiLabels.explainer}</p>
          {!sessionId ? (
            <p className="muted">{genuiLabels.openThread}</p>
          ) : refused.length === 0 ? (
            <p className="muted">{genuiLabels.noRefused}</p>
          ) : (
            <ul className="genui-refused">
              {refused.map((refusal, index) => (
                <li key={`${refusal.callId ?? index}`}>
                  <span className="drawn-mono">{refusal.atText}</span>
                  <strong>{visible(refusal.agent.displayName)}</strong>
                  <span className="drawn-mono">{visible(refusal.reason)}</span>
                </li>
              ))}
            </ul>
          )}
        </>
      )}
    </div>
  );
}
