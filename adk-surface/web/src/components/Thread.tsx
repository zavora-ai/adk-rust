import { useEffect, useState } from "react";
import type { DecisionCard as Card, FeedEntry, ThreadItem } from "../generated/api";
import type { HomeClient } from "../client";
import { labels } from "../labels";
import { MarkdownResponse } from "../MarkdownResponse";
import { DecisionCard } from "./DecisionCard";
import type { Send, TakeOverSeam } from "./DecisionCard";
import { Icon } from "./Icon";

type Activity = Extract<ThreadItem, { kind: "activity" }>;
type Result = Extract<ThreadItem, { kind: "result" }>;

/** One line of a thread, ready to draw. */
type Line =
  | { kind: "message"; key: string; from: "you" | "agent"; text: string }
  | { kind: "activity"; key: string; text: string; result?: Result }
  | { kind: "decision"; key: string; id?: string; tool: string; text: string };

/** The thread's lines: activities joined with their results, decisions in place. */
export function lines(entries: FeedEntry[]): Line[] {
  const out: Line[] = [];
  const byCall = new Map<string, Extract<Line, { kind: "activity" }>>();
  for (const entry of entries) {
    entry.items.forEach((item, index) => {
      const key = `${entry.seq}:${index}`;
      switch (item.kind) {
        case "message":
          out.push({ kind: "message", key, from: item.from, text: item.text });
          break;
        case "activity": {
          const line = { kind: "activity" as const, key, text: (item as Activity).text };
          out.push(line);
          if (item.callId) byCall.set(item.callId, line);
          break;
        }
        case "result": {
          const activity = item.callId ? byCall.get(item.callId) : undefined;
          if (activity) activity.result = item;
          else out.push({ kind: "activity", key, text: item.tool, result: item });
          break;
        }
        case "decision":
          out.push({ kind: "decision", key, id: item.decisionId ?? undefined, tool: item.tool, text: item.text });
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
}: {
  entries: FeedEntry[];
  open: Card[];
  client: HomeClient;
  send: Send;
  onSettled: (card: Card) => void;
  focusDecision?: string;
  takeOver?: TakeOverSeam;
}) {
  const [settled, setSettled] = useState<Card[]>([]);
  const settle = (card: Card) => {
    setSettled((all) => [...all.filter((known) => known.id !== card.id), card]);
    onSettled(card);
  };
  const shown = lines(entries);
  const named = new Set(shown.flatMap((line) => (line.kind === "decision" && line.id ? [line.id] : [])));
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
                {line.text}
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

function ActivityChip({ line }: { line: Extract<Line, { kind: "activity" }> }) {
  const failed = line.result && !line.result.ok;
  return (
    <p className={`activity${failed ? " activity-failed" : ""}`}>
      {line.result && <Icon name={line.result.ok ? "check" : "cross"} />}
      <span>{line.text}</span>
      {line.result && <span className="activity-result"> · {line.result.text}</span>}
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
  focus,
  takeOver,
}: {
  id: string;
  card?: Card;
  client: HomeClient;
  send: Send;
  onSettled: (card: Card) => void;
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
      .then((value) => live && setShown(value))
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, [card, client, id]);
  if (!shown) return null;
  return <DecisionCard card={shown} send={send} onSettled={onSettled} focusOnMount={focus} takeOver={takeOver} />;
}
