import { useEffect, useMemo, useState } from "react";
import type { AppView, DecisionCard as Card, DrawnSent, DrawnSurface, FeedEntry, RefusedDrawing, ThreadItem } from "../generated/api";
import type { HomeClient } from "../client";
import { AppFrame } from "../genui/AppFrame";
import type { AppsSeam } from "../genui/apps";
import { DrawnView } from "../genui/DrawnView";
import { useFormState } from "../genui/forms";
import { RefusedNotice } from "../genui/Refused";
import { labels } from "../labels";
import { MarkdownResponse } from "../MarkdownResponse";
import { visible } from "../safe";
import { DecisionCard } from "./DecisionCard";
import type { Send, TakeOverSeam } from "./DecisionCard";
import { Icon } from "./Icon";

type Activity = Extract<ThreadItem, { kind: "activity" }>;
type Result = Extract<ThreadItem, { kind: "result" }>;

/** One line of a thread, ready to draw. */
type Line =
  | { kind: "message"; key: string; from: "you" | "agent"; text: string }
  | { kind: "activity"; key: string; text: string; result?: Result }
  | { kind: "decision"; key: string; id?: string; tool: string; text: string }
  | {
      kind: "drawn";
      key: string;
      surface: DrawnSurface;
      /** A newer version of the surface follows: this one is collapsed. */
      superseded: boolean;
      /** An earlier version of the surface precedes it. */
      changed: boolean;
      /** Its latest send, and that call's result. */
      sent?: DrawnSent;
      result?: Result;
    }
  | { kind: "refused"; key: string; refusal: RefusedDrawing }
  | {
      kind: "app";
      key: string;
      app: AppView;
      /** The calls the app asked for, each with its decision (spec 3.3 §6.1 `genui-apps`). */
      sends: DrawnSent[];
    };

/**
 * The thread's lines: activities joined with their results, decisions in place, and what
 * agents drew (spec 3.3 §7.4 rule 5) with the decision and result of each form's send drawn
 * with the form rather than on their own.
 */
export function lines(entries: FeedEntry[]): Line[] {
  const out: Line[] = [];
  const byCall = new Map<string, Extract<Line, { kind: "activity" }>>();
  // What the forms' sends claim: the latest send per surface and version, and its call.
  const sends = new Map<string, DrawnSent>();
  const latest = new Map<string, number>();
  const first = new Map<string, number>();
  const results = new Map<string, Result>();
  // The render calls whose drawing, or refused drawing, the thread shows in their place, and
  // the MCP tool calls whose app it shows in theirs.
  const drawing = new Set<string>();
  const apps = new Set<string>();
  const sent: DrawnSent[] = [];
  for (const entry of entries) {
    for (const item of entry.items) {
      if (item.kind === "drawnSent") sent.push(item.sent);
      if (item.kind === "app") {
        apps.add(item.app.callId);
        drawing.add(item.app.callId);
      }
      if (item.kind === "result" && item.callId) results.set(item.callId, item);
      if (item.kind === "drawnRefused" && item.refusal.callId) drawing.add(item.refusal.callId);
      if (item.kind === "drawn") {
        if (item.surface.callId) drawing.add(item.surface.callId);
        const { surfaceId, version } = item.surface;
        latest.set(surfaceId, Math.max(latest.get(surfaceId) ?? 0, version));
        first.set(surfaceId, Math.min(first.get(surfaceId) ?? version, version));
      }
    }
  }
  // A call an app asked for is sent under the app, by the app's call; a form's, under the form.
  const appSends = new Map<string, DrawnSent[]>();
  for (const one of sent) {
    if (apps.has(one.surfaceId)) appSends.set(one.surfaceId, [...(appSends.get(one.surfaceId) ?? []), one]);
    else sends.set(`${one.surfaceId}@${one.version}`, one);
  }
  const claimed = new Set([...sends.values()].map((one) => one.callId));
  const decisions = new Set(sent.flatMap((one) => (one.decisionId ? [one.decisionId] : [])));
  for (const entry of entries) {
    entry.items.forEach((item, index) => {
      const key = `${entry.seq}:${index}`;
      switch (item.kind) {
        case "message":
          out.push({ kind: "message", key, from: item.from, text: item.text });
          break;
        case "activity": {
          if (item.callId && drawing.has(item.callId)) break;
          const line = { kind: "activity" as const, key, text: (item as Activity).text };
          out.push(line);
          if (item.callId) byCall.set(item.callId, line);
          break;
        }
        case "result": {
          // A form's call ends on its form.
          if (item.callId && claimed.has(item.callId)) break;
          const activity = item.callId ? byCall.get(item.callId) : undefined;
          if (activity) activity.result = item;
          else out.push({ kind: "activity", key, text: item.tool, result: item });
          break;
        }
        case "decision":
          // A form's decision is drawn under its form.
          if (item.decisionId && decisions.has(item.decisionId)) break;
          out.push({ kind: "decision", key, id: item.decisionId ?? undefined, tool: item.tool, text: item.text });
          break;
        case "drawn": {
          const { surfaceId, version } = item.surface;
          const sent = sends.get(`${surfaceId}@${version}`);
          out.push({
            kind: "drawn",
            key,
            surface: item.surface,
            superseded: (latest.get(surfaceId) ?? version) > version,
            changed: (first.get(surfaceId) ?? version) < version,
            sent,
            result: sent ? results.get(sent.callId) : undefined,
          });
          break;
        }
        case "drawnRefused":
          out.push({ kind: "refused", key, refusal: item.refusal });
          break;
        case "app":
          out.push({ kind: "app", key, app: item.app, sends: appSends.get(item.app.callId) ?? [] });
          break;
        case "drawnSent":
          break;
      }
    });
  }
  return out;
}

/**
 * A session's thread (spec 3.3 §5.1): messages, what agents did, and decisions inline on
 * their own background. Model and tool text is rendered as text or as Markdown without raw
 * HTML (S-I9). Open decisions of the session that no line names follow at the end, and stay
 * there, decided, once answered here. Keyed by session: nothing carries over to another.
 */
export function Thread({
  entries,
  open,
  client,
  send,
  onSettled,
  focusDecision,
  takeOver,
  session,
}: {
  entries: FeedEntry[];
  open: Card[];
  client: HomeClient;
  send: Send;
  onSettled: (card: Card) => void;
  focusDecision?: string;
  takeOver?: TakeOverSeam;
  /** The session shown: what a drawn form is sent in (spec 3.3 §7.4 rule 5). */
  session?: string;
}) {
  const [settled, setSettled] = useState<Card[]>([]);
  const settle = (card: Card) => {
    setSettled((all) => [...all.filter((known) => known.id !== card.id), card]);
    onSettled(card);
  };
  const shown = lines(entries);
  const apps = useMemo(() => client.apps?.(), [client]);
  const named = new Set(
    shown.flatMap((line) =>
      line.kind === "decision" && line.id
        ? [line.id]
        : line.kind === "drawn" && line.sent?.decisionId
          ? [line.sent.decisionId]
          : line.kind === "app"
            ? line.sends.flatMap((one) => (one.decisionId ? [one.decisionId] : []))
            : [],
    ),
  );
  // One list keyed by id, so a card answered here keeps its place when it leaves `open`.
  const rest = [
    ...open,
    ...settled.filter((card) => !open.some((candidate) => candidate.id === card.id)),
  ].filter((card) => !named.has(card.id));
  return (
    <div className="thread" aria-live="polite">
      {shown.map((line) => {
        switch (line.kind) {
          case "message":
            return line.from === "you" ? (
              <div key={line.key} className="message message-you">
                <span className="visually-hidden">{labels.you}: </span>
                {line.text}
              </div>
            ) : (
              <div key={line.key} className="message message-agent">
                <MarkdownResponse>{line.text}</MarkdownResponse>
              </div>
            );
          case "activity":
            return <ActivityChip key={line.key} line={line} />;
          case "refused":
            return <RefusedNotice key={line.key} refusal={line.refusal} />;
          case "app":
            return (
              <AppLine
                key={line.key}
                line={line}
                apps={apps}
                open={open}
                client={client}
                send={send}
                session={session}
                onSettled={settle}
                focusDecision={focusDecision}
                takeOver={takeOver}
              />
            );
          case "drawn":
            return (
              <DrawnLine
                key={line.key}
                line={line}
                open={open}
                client={client}
                send={send}
                session={session}
                onSettled={settle}
                focusDecision={focusDecision}
                takeOver={takeOver}
              />
            );
          case "decision": {
            const card = line.id ? open.find((candidate) => candidate.id === line.id) : undefined;
            return line.id ? (
              <DecisionSlot
                key={line.key}
                id={line.id}
                card={card}
                client={client}
                send={send}
                onSettled={settle}
                focus={focusDecision === line.id}
                takeOver={takeOver}
              />
            ) : (
              <p key={line.key} className="activity">
                {visible(line.text)}
              </p>
            );
          }
        }
      })}
      {rest.map((card) => (
        <DecisionCard
          key={card.id}
          card={card}
          send={send}
          onSettled={settle}
          focusOnMount={focusDecision === card.id}
          takeOver={takeOver}
        />
      ))}
    </div>
  );
}

/**
 * A drawing, and for a form sent from it the decision its Send asked for, under it on the
 * decision's own background (spec 3.3 §7.4 rule 5). The form's state is read from the server
 * as the decision and the call's result change.
 */
function DrawnLine({
  line,
  open,
  client,
  send,
  session,
  onSettled,
  focusDecision,
  takeOver,
}: {
  line: Extract<Line, { kind: "drawn" }>;
  open: Card[];
  client: HomeClient;
  send: Send;
  session?: string;
  onSettled: (card: Card) => void;
  focusDecision?: string;
  takeOver?: TakeOverSeam;
}) {
  const id = line.sent?.decisionId ?? undefined;
  const card = id ? open.find((candidate) => candidate.id === id) : undefined;
  const [decided, setDecided] = useState<Card>();
  const shownCard = card ?? decided;
  const state = useFormState(client, session ?? shownCard?.sessionId, line.sent, shownCard, !!line.result);
  const onSend = session
    ? async (actionId: string, values: Record<string, unknown>) => {
        await send(
          {
            type: "submitDrawn",
            sessionId: session,
            surfaceId: line.surface.surfaceId,
            version: line.surface.version,
            actionId,
            values,
          },
          { kind: "pointer" },
        );
      }
    : undefined;
  const args = shownCard?.call.args;
  return (
    <DrawnView
      surface={line.surface}
      superseded={line.superseded}
      changed={line.changed}
      state={line.sent ? state : undefined}
      onSend={line.superseded ? undefined : onSend}
      sent={
        line.sent && args && typeof args === "object" && !Array.isArray(args)
          ? { actionId: line.sent.actionId, args: args as Record<string, unknown> }
          : undefined
      }
      below={
        id ? (
          <DecisionSlot
            id={id}
            card={card}
            client={client}
            send={send}
            onSettled={(settled) => {
              setDecided(settled);
              onSettled(settled);
            }}
            onRead={setDecided}
            focus={focusDecision === id}
            takeOver={takeOver}
          />
        ) : undefined
      }
    />
  );
}

/**
 * An app an MCP server's tool showed, and under it the decision of each call the app asked
 * for, on the decision's own background outside the app (spec 3.3 §6.1 `genui-apps`). A call
 * the agent's rules refused before holding it is a plain notice.
 */
function AppLine({
  line,
  apps,
  open,
  client,
  send,
  session,
  onSettled,
  focusDecision,
  takeOver,
}: {
  line: Extract<Line, { kind: "app" }>;
  apps?: AppsSeam;
  open: Card[];
  client: HomeClient;
  send: Send;
  session?: string;
  onSettled: (card: Card) => void;
  focusDecision?: string;
  takeOver?: TakeOverSeam;
}) {
  return (
    <AppFrame
      app={line.app}
      seam={apps}
      session={session}
      below={line.sends.map((one) =>
        one.decisionId ? (
          <DecisionSlot
            key={one.callId}
            id={one.decisionId}
            card={open.find((candidate) => candidate.id === one.decisionId)}
            client={client}
            send={send}
            onSettled={onSettled}
            focus={focusDecision === one.decisionId}
            takeOver={takeOver}
          />
        ) : one.held ? null : (
          <p key={one.callId} className="drawn-notice" role="note">
            {visible(one.text)}
          </p>
        ),
      )}
    />
  );
}

function ActivityChip({ line }: { line: Extract<Line, { kind: "activity" }> }) {
  const failed = line.result && !line.result.ok;
  return (
    <p className={`activity${failed ? " activity-failed" : ""}`}>
      {line.result && <Icon name={line.result.ok ? "check" : "cross"} />}
      <span>{visible(line.text)}</span>
      {line.result && <span className="activity-result"> · {visible(line.result.text)}</span>}
    </p>
  );
}

/**
 * A decision a line names: the open card while it is open, then the decided one, read once it
 * leaves the open list. Until that read answers, and if it fails, the card shown last stays.
 */
function DecisionSlot({
  id,
  card,
  client,
  send,
  onSettled,
  onRead,
  focus,
  takeOver,
}: {
  id: string;
  card?: Card;
  client: HomeClient;
  send: Send;
  onSettled: (card: Card) => void;
  /** The decided card, once read. */
  onRead?: (card: Card) => void;
  focus: boolean;
  takeOver?: TakeOverSeam;
}) {
  const [shown, setShown] = useState<Card | undefined>(card);
  useEffect(() => {
    if (card) {
      setShown(card);
      return;
    }
    let live = true;
    client
      .decision(id)
      .then((value) => {
        if (!live) return;
        setShown(value);
        onRead?.(value);
      })
      .catch(() => undefined);
    return () => {
      live = false;
    };
    // `onRead` is told of each read; a new function each render reads nothing again.
  }, [card, client, id]);
  if (!shown) return null;
  return <DecisionCard card={shown} send={send} onSettled={onSettled} focusOnMount={focus} takeOver={takeOver} />;
}
