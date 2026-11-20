import { useEffect, useMemo, useState } from "react";
import type { AppView, DecisionCard as Card, DrawnSent, DrawnSurface, FeedEntry, ImageData, RefusedDrawing, ShellView, ThreadItem } from "../generated/api";
import type { HomeClient } from "../client";
import { AppFrame } from "../genui/AppFrame";
import type { AppsSeam } from "../genui/apps";
import { DrawnView } from "../genui/DrawnView";
import { useFormState } from "../genui/forms";
import { RefusedNotice } from "../genui/Refused";
import { labels } from "../labels";
import { MarkdownResponse } from "../MarkdownResponse";
import { visible, visibleJson } from "../safe";
import { DecisionCard } from "./DecisionCard";
import type { Send, TakeOverSeam } from "./DecisionCard";
import { Icon } from "./Icon";
import { MessageImages, RunCard } from "./RunCard";
import { canvasWindowPath } from "../screens/CanvasWindow";

type Activity = Extract<ThreadItem, { kind: "activity" }>;
type Result = Extract<ThreadItem, { kind: "result" }>;
type Run = Extract<ThreadItem, { kind: "run" }>;

/** One line of a thread, ready to draw. */
type Line =
  | { kind: "message"; key: string; from: "you" | "agent"; text: string; images?: ImageData[] }
  | { kind: "activity"; key: string; text: string; result?: Result; run?: Run; at?: string; endAt?: string }
  | { kind: "decision"; key: string; id?: string; tool: string; text: string }
  | { kind: "suggestions"; key: string; replies: string[] }
  | {
      kind: "drawn";
      key: string;
      surface: DrawnSurface;
      /** A newer version of the surface follows: this one is collapsed. */
      superseded: boolean;
      /** An earlier version of the surface precedes it. */
      changed: boolean;
      /**
       * The earlier versions folded into this, the latest one, oldest first: the versions no
       * form was sent from, which would otherwise each take a line of the thread.
       */
      earlier?: DrawnSurface[];
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
  const folded = new Map<string, DrawnSurface[]>();
  for (const entry of entries) {
    entry.items.forEach((item, index) => {
      const key = `${entry.seq}:${index}`;
      switch (item.kind) {
        case "message":
          out.push({ kind: "message", key, from: item.from, text: item.text, images: item.images ?? undefined });
          break;
        case "activity": {
          if (item.callId && drawing.has(item.callId)) break;
          const line: Extract<Line, { kind: "activity" }> = { kind: "activity", key, text: (item as Activity).text, at: entry.at };
          out.push(line);
          if (item.callId) byCall.set(item.callId, line);
          break;
        }
        case "result": {
          // A form's call ends on its form.
          if (item.callId && claimed.has(item.callId)) break;
          const activity = item.callId ? byCall.get(item.callId) : undefined;
          if (activity) {
            activity.result = item;
            activity.endAt = entry.at;
          } else out.push({ kind: "activity", key, text: item.tool, result: item, at: entry.at, endAt: entry.at });
          break;
        }
        case "suggestions":
          out.push({ kind: "suggestions", key, replies: item.replies });
          break;
        case "run": {
          // A command's run ends on its activity, as a result does (spec 3.3 §5.1).
          const activity = item.callId ? byCall.get(item.callId) : undefined;
          if (activity) activity.run = item;
          else out.push({ kind: "activity", key, text: item.run.command ?? item.tool, run: item });
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
          const superseded = (latest.get(surfaceId) ?? version) > version;
          // An earlier version a form was sent from keeps its place, with its decision; the
          // rest fold into the latest version as one "earlier versions" control.
          if (superseded && !sent) {
            folded.set(surfaceId, [...(folded.get(surfaceId) ?? []), item.surface]);
            break;
          }
          out.push({
            kind: "drawn",
            key,
            surface: item.surface,
            superseded,
            changed: (first.get(surfaceId) ?? version) < version,
            earlier: superseded ? undefined : folded.get(surfaceId),
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
  shell,
  pinned,
  onPin,
  onSuggest,
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
  /** Where the thread's agent runs commands, for each run's badge (spec 3.3 §5.7). */
  shell?: ShellView;
  /** The drawing shown beside the thread, by its surface. */
  pinned?: string;
  /** Shows a drawing beside the thread, or brings it back (`undefined`). */
  onPin?: (surfaceId: string | undefined) => void;
  /** Puts a suggested reply in the message box; the person still sends it. */
  onSuggest?: (text: string) => void;
}) {
  const [settled, setSettled] = useState<Card[]>([]);
  const settle = (card: Card) => {
    setSettled((all) => [...all.filter((known) => known.id !== card.id), card]);
    onSettled(card);
  };
  const shown = lines(entries);
  // Suggested replies stand only under the agent's latest message, until the person writes.
  const lastYou = shown.map((line) => line.kind === "message" && line.from === "you").lastIndexOf(true);
  const lastSuggestions = shown.map((line) => line.kind === "suggestions").lastIndexOf(true);
  const offered = lastSuggestions > lastYou ? lastSuggestions : -1;
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
      {worklog(shown).map((unit) => {
        if (unit.kind === "log") return <WorkLog key={unit.key} steps={unit.steps} />;
        const line = unit.line;
        switch (line.kind) {
          case "suggestions":
            return shown[offered] === line && onSuggest ? (
              <div key={line.key} className="suggestions" role="group" aria-label={labels.suggestedReplies}>
                {line.replies.map((reply) => (
                  <button key={reply} type="button" className="suggestion" onClick={() => onSuggest(reply)}>
                    {visible(reply)}
                  </button>
                ))}
              </div>
            ) : null;
          case "message":
            return line.from === "you" ? (
              <div key={line.key} className="message-you-group">
                {line.images && line.images.length > 0 && <MessageImages images={line.images} />}
                {line.text && (
                  <div className="message message-you">
                    <span className="visually-hidden">{labels.you}: </span>
                    {line.text}
                  </div>
                )}
              </div>
            ) : (
              <div key={line.key} className="message message-agent">
                <MarkdownResponse>{line.text}</MarkdownResponse>
              </div>
            );
          case "activity":
            return line.run ? (
              <RunCard key={line.key} tool={line.run.tool} what={line.text} ok={line.run.ok} run={line.run.run} shell={shell} />
            ) : (
              <ActivityChip key={line.key} line={line} />
            );
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
          case "drawn": {
            const beside = !line.superseded && onPin !== undefined;
            // A drawing shown beside the thread is drawn there only; here it leaves a line.
            if (beside && pinned === line.surface.surfaceId) {
              return (
                <p key={line.key} className="drawn-beside">
                  <span>
                    <strong>{visible(line.surface.title ?? labels.drawing)}</strong> · {labels.shownBeside}
                  </span>
                  <button type="button" className="link" onClick={() => onPin?.(undefined)}>
                    {labels.bringBack}
                  </button>
                </p>
              );
            }
            return (
              <div key={line.key} className="drawn-line">
                {beside && (
                  <button type="button" className="pin-beside" onClick={() => onPin?.(line.surface.surfaceId)}>
                    <svg className="icon" viewBox="0 0 24 24" aria-hidden="true">
                      <rect x="3.5" y="4.5" width="17" height="15" rx="2.5" />
                      <path d="M13.5 4.5v15" />
                    </svg>
                    {labels.showBeside}
                  </button>
                )}
                <DrawnLine
                  line={line}
                  open={open}
                  client={client}
                  send={send}
                  session={session}
                  onSettled={settle}
                  focusDecision={focusDecision}
                  takeOver={takeOver}
                />
              </div>
            );
          }
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
/**
 * The canvas beside a thread (spec 3.3 §5.1): one drawing kept in view while the conversation
 * goes on, at its latest version as the agent patches it, with its earlier versions and the
 * A2UI it arrived as. Its form sends and its decisions work here as in the thread.
 */
export function CanvasPane({
  entries,
  pinned,
  open,
  client,
  send,
  session,
  onSettled,
  focusDecision,
  takeOver,
  onClose,
  popOut = true,
}: {
  entries: FeedEntry[];
  pinned: string;
  open: Card[];
  client: HomeClient;
  send: Send;
  session?: string;
  onSettled: (card: Card) => void;
  focusDecision?: string;
  takeOver?: TakeOverSeam;
  onClose: () => void;
  /** Offers to open the drawing in its own window. */
  popOut?: boolean;
}) {
  const [tab, setTab] = useState<"drawing" | "versions" | "wire">("drawing");
  const line = lines(entries).find(
    (candidate): candidate is Extract<Line, { kind: "drawn" }> =>
      candidate.kind === "drawn" && candidate.surface.surfaceId === pinned && !candidate.superseded,
  );
  const earlier = line?.earlier ?? [];
  return (
    <aside className="canvas" aria-label={labels.canvas}>
      <header className="canvas-head">
        <h2 className="canvas-title">{line ? visible(line.surface.title ?? labels.drawing) : labels.drawing}</h2>
        {line && <span className="canvas-version">v{line.surface.version}</span>}
        {popOut && line && session && (
          <button
            type="button"
            className="icon-button canvas-popout"
            aria-label={labels.openOwnWindow}
            title={labels.openOwnWindow}
            onClick={() => window.open(canvasWindowPath(session, pinned), "_blank", "popup,width=760,height=900")}
          >
            <svg className="icon" viewBox="0 0 24 24" aria-hidden="true">
              <path d="M14 4.5h5.5V10M19.5 4.5 11 13M10 6H6.5A2 2 0 0 0 4.5 8v9.5a2 2 0 0 0 2 2H16a2 2 0 0 0 2-2V14" />
            </svg>
          </button>
        )}
        <button type="button" className="icon-button canvas-close" aria-label={labels.closeCanvas} onClick={onClose}>
          <svg className="icon" viewBox="0 0 24 24" aria-hidden="true">
            <path d="M6 6l12 12M18 6 6 18" />
          </svg>
        </button>
      </header>
      {line ? (
        <>
          <div className="canvas-tabs" role="tablist" aria-label={labels.canvas}>
            {(
              [
                ["drawing", labels.drawing],
                ["versions", labels.earlierVersions(earlier.length)],
                ["wire", "A2UI"],
              ] as const
            ).map(([key, label]) => (
              <button key={key} type="button" role="tab" aria-selected={tab === key} onClick={() => setTab(key)}>
                {label}
              </button>
            ))}
          </div>
          <div className="canvas-body" role="tabpanel">
            {tab === "drawing" && (
              <DrawnLine
                line={{ ...line, earlier: undefined }}
                open={open}
                client={client}
                send={send}
                session={session}
                onSettled={onSettled}
                focusDecision={focusDecision}
                takeOver={takeOver}
              />
            )}
            {tab === "versions" &&
              (earlier.length === 0 ? (
                <p className="canvas-empty">{labels.noEarlierVersions}</p>
              ) : (
                earlier
                  .slice()
                  .reverse()
                  .map((surface) => <DrawnView key={surface.version} surface={surface} superseded />)
              ))}
            {tab === "wire" && <pre className="canvas-wire">{visibleJson(JSON.stringify(line.surface, null, 2))}</pre>}
          </div>
        </>
      ) : (
        <p className="canvas-empty">{labels.canvasGone}</p>
      )}
    </aside>
  );
}

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
      earlier={line.earlier}
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

/** The fewest steps in a row that fold into one work-log line. */
export const WORKLOG_FOLD = 3;

type ActivityLine = Extract<Line, { kind: "activity" }>;
type Unit = { kind: "line"; line: Line } | { kind: "log"; key: string; steps: ActivityLine[] };

/**
 * The thread's lines with each run of `WORKLOG_FOLD` or more plain tool steps in a row folded
 * into one work-log line. A run of a command keeps its own card; the last step of a thread
 * still working stays in view, outside the fold.
 */
export function worklog(shown: Line[]): Unit[] {
  const units: Unit[] = [];
  let steps: ActivityLine[] = [];
  const flush = () => {
    if (steps.length >= WORKLOG_FOLD) {
      const open = steps[steps.length - 1]?.result === undefined ? steps.pop() : undefined;
      units.push({ kind: "log", key: `log-${steps[0]!.key}`, steps });
      if (open) units.push({ kind: "line", line: open });
    } else {
      units.push(...steps.map((line) => ({ kind: "line" as const, line })));
    }
    steps = [];
  };
  for (const line of shown) {
    if (line.kind === "activity" && !line.run) {
      steps.push(line);
      continue;
    }
    flush();
    units.push({ kind: "line", line });
  }
  flush();
  return units;
}

/** Steps folded into one line: how many, how many failed, the last one; open for the rest. */
function WorkLog({ steps }: { steps: ActivityLine[] }) {
  const failed = steps.filter((step) => step.result && !step.result.ok).length;
  const last = steps[steps.length - 1]!;
  // How long the folded work took: from the first step to the last step's result.
  const from = Date.parse(steps[0]?.at ?? "");
  const to = Date.parse(last.endAt ?? last.at ?? "");
  const seconds = Number.isFinite(from) && Number.isFinite(to) && to >= from ? Math.round((to - from) / 1000) : undefined;
  return (
    <details className={`work-log${failed ? " work-log-failed" : ""}`}>
      <summary>
        {seconds !== undefined && seconds > 0 && <span className="work-log-time">{labels.workedFor(seconds)}</span>}
        <span className="work-log-count">{labels.workSteps(steps.length)}</span>
        {failed > 0 && <span className="work-log-failed-count">{labels.workFailed(failed)}</span>}
        <span className="work-log-last">{visible(last.text)}</span>
      </summary>
      <div className="work-log-steps">
        {steps.map((step) => (
          <ActivityChip key={step.key} line={step} />
        ))}
      </div>
    </details>
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
