import { useCallback, useEffect, useId, useMemo, useState } from "react";
import type { FormEvent, MouseEvent } from "react";
import type {
  AgentsResponse,
  Command,
  CommandOutcome,
  DecisionCard,
  Origin,
  StatusLine,
} from "../generated/api";
import { ApiError, problemText } from "../client";
import { hhmm } from "../format";
import type { AgendaItem, KernelProcess } from "../kernel";
import { processTitle } from "../kernel";
import { originOf } from "../origin";
import { planned } from "../plan";
import { usePoll } from "../poll";
import { visible } from "../safe";
import { applyTheme, storedTheme } from "../theme";
import { TrayDecision } from "./TrayDecision";
import { trayLabels } from "./labels";
import "./tray.css";

/** Where the web home keeps the theme (`theme.ts`), which the panel follows. */
const THEME_KEY = "adk.home.theme";

/** At most this many answered cards stay on the panel, with who answered. */
const SETTLED_SHOWN = 3;

/** What the tray panel reads and does (spec 3.3 §5.5). `HttpClient` built for the tray is one. */
export interface TrayClient {
  status(): Promise<StatusLine>;
  decisions(): Promise<DecisionCard[]>;
  /** The agents and the one a message goes to when the person names none. */
  agentsWithPrimary(): Promise<AgentsResponse>;
  /** The kernel's running processes; `undefined` without a kernel. */
  running(): Promise<KernelProcess[] | undefined>;
  /** The kernel's waiting processes; `undefined` without a kernel. */
  processes(): Promise<KernelProcess[] | undefined>;
  agenda(): Promise<AgendaItem[] | undefined>;
  command(command: Command, origin: Origin): Promise<CommandOutcome>;
  /** One wake-up per kernel event, until `signal` aborts. */
  events?(signal: AbortSignal): AsyncIterable<void>;
}

/** A session for the primary agent when it has none yet: the engine creates it. */
function newSession() {
  return typeof crypto !== "undefined" && "randomUUID" in crypto ? crypto.randomUUID() : `s-${Date.now().toString(36)}`;
}

/**
 * The tray panel (spec 3.3 §5.5): running state, spending against the limit, decisions with a
 * badge, what is working now and next, a quick message box, Open home and Stop all. It says
 * yes once or no to everyday calls and opens the home for everything else (§7.2). Keys,
 * hiding the panel and closing it never decide (§7.4); there is no Resume all (S-I10).
 */
export function TrayPanel({ client }: { client: TrayClient }) {
  const status = usePoll(() => client.status());
  const decisions = usePoll(() => client.decisions());
  const roster = usePoll(() => client.agentsWithPrimary());
  const kernel = usePoll(async () => {
    const [running, waiting, agenda] = await Promise.all([
      client.running().catch(() => undefined),
      client.processes().catch(() => undefined),
      client.agenda().catch(() => undefined),
    ]);
    return { running, waiting, agenda };
  });
  const [settled, setSettled] = useState<DecisionCard[]>([]);
  const [problem, setProblem] = useState("");

  const { refresh: refreshStatus } = status;
  const { refresh: refreshDecisions } = decisions;
  const { refresh: refreshRoster } = roster;
  const { refresh: refreshKernel } = kernel;
  const refresh = useCallback(() => {
    void refreshStatus();
    void refreshDecisions();
    void refreshRoster();
    void refreshKernel();
  }, [refreshStatus, refreshDecisions, refreshRoster, refreshKernel]);

  // Every kernel event reads the panel again (spec 3.3 §12 phase 5: "the tray changes within
  // one event"); a dropped stream reconnects.
  useEffect(() => {
    const events = client.events?.bind(client);
    if (!events) return;
    const abort = new AbortController();
    void (async () => {
      while (!abort.signal.aborted) {
        try {
          for await (const frame of events(abort.signal)) {
            void frame;
            refresh();
          }
        } catch {
          // The kernel went away; the polls say so.
        }
        await new Promise((resolve) => setTimeout(resolve, 2_000));
      }
    })();
    return () => abort.abort();
  }, [client, refresh]);

  // The panel is shown again: read now, not at the next poll.
  useEffect(() => {
    const again = () => {
      if (document.visibilityState !== "hidden") refresh();
    };
    document.addEventListener("visibilitychange", again);
    window.addEventListener("focus", again);
    return () => {
      document.removeEventListener("visibilitychange", again);
      window.removeEventListener("focus", again);
    };
  }, [refresh]);

  // Light or dark as the home's switch sets it, following it while open (§9).
  useEffect(() => {
    applyTheme(storedTheme());
    const follow = (event: StorageEvent) => {
      if (event.key === null || event.key === THEME_KEY) applyTheme(storedTheme());
    };
    window.addEventListener("storage", follow);
    return () => window.removeEventListener("storage", follow);
  }, []);

  const send = useCallback(
    async (command: Command, origin: Origin) => {
      const outcome = await client.command(command, origin);
      refresh();
      return outcome;
    },
    [client, refresh],
  );

  const settle = useCallback((card: DecisionCard) => {
    setSettled((all) => [card, ...all.filter((known) => known.id !== card.id)].slice(0, SETTLED_SHOWN));
  }, []);

  async function stopAll(event: MouseEvent<HTMLButtonElement>) {
    setProblem("");
    try {
      await send({ type: "stopAll" }, originOf(event));
    } catch (error) {
      setProblem(problemText(error));
    }
  }

  const unauthorized = [status.error, decisions.error].some((error) => error instanceof ApiError && error.status === 401);
  const unreachable = !unauthorized && status.error !== undefined && !(status.error instanceof ApiError);
  const line = unreachable || unauthorized ? undefined : status.value;
  const open = unreachable ? [] : (decisions.value ?? []);
  const shown = [...open, ...settled.filter((card) => !open.some((candidate) => candidate.id === card.id))];
  const agents = roster.value?.agents;
  const primary = roster.value?.primary ?? undefined;
  const nameOf = (target: string) =>
    agents?.find((row) => row.agent.name === target)?.agent.displayName ??
    (primary?.name === target ? primary.displayName : target);
  const next = useMemo(
    () => planned(kernel.value?.waiting, kernel.value?.agenda, agents, new Date())[0],
    [kernel.value, agents],
  );
  const working = (kernel.value?.running ?? []).flatMap((process) => {
    const title = processTitle(process, kernel.value?.agenda ?? []);
    return title ? [{ key: process.meta.pid, agent: nameOf(process.run.target), title }] : [];
  });
  const count = line?.decisions ?? 0;

  return (
    <div className="tray">
      <header className="tray-head">
        <div className="tray-brand-row">
          <p className="tray-brand">
            <span className="tray-dot" aria-hidden="true" />
            {trayLabels.brand}
          </p>
          {line && (
            <span className={`tray-state${line.stopped ? " tray-state-stopped" : ""}`}>
              <i className="tray-state-dot" aria-hidden="true" />
              {line.stopped ? (line.stoppedText ?? trayLabels.stopped) : trayLabels.running}
            </span>
          )}
        </div>
        {line && (line.spend || line.quiet) && (
          <div className="tray-limits">
            {line.spend ? <span>{line.spend.text}</span> : <span />}
            {line.quiet && <span>{line.quiet.text}</span>}
          </div>
        )}
        {line?.spend?.limitMicroUsd ? <SpendBar spent={line.spend.spentMicroUsd} limit={line.spend.limitMicroUsd} /> : null}
      </header>

      {(unauthorized || unreachable || problem) && (
        <p className="tray-banner" role="alert">
          {unauthorized ? trayLabels.startWithToken : unreachable ? trayLabels.cantReach : problem}
        </p>
      )}

      <div className="tray-body">
        <section className="tray-decisions" aria-labelledby="tray-decisions-heading">
          <h2 id="tray-decisions-heading" className="tray-heading">
            {trayLabels.decisions}
            {count > 0 && (
              <span className="tray-badge" aria-label={trayLabels.waiting(count)}>
                {count}
              </span>
            )}
          </h2>
          {shown.length === 0 && !unreachable && !unauthorized && <p className="tray-empty">{trayLabels.nothingWaits}</p>}
          {shown.map((card) => (
            <TrayDecision key={card.id} card={card} send={send} onSettled={settle} />
          ))}
        </section>

        <section className="tray-now" aria-labelledby="tray-now-heading">
          <h2 id="tray-now-heading" className="tray-subheading">
            {trayLabels.nowAndNext}
          </h2>
          <ul className="tray-plan">
            {working.map((item) => (
              <li key={item.key}>
                <span>
                  {visible(item.agent)} · {visible(item.title)}
                </span>
                <span className="tray-working">{trayLabels.working}</span>
              </li>
            ))}
            {next ? (
              <li>
                <span>{visible(next.agent ? `${next.agent} · ${next.title}` : next.title)}</span>
                <time className="mono" dateTime={next.at.toISOString()}>
                  {hhmm(next.at)}
                </time>
              </li>
            ) : (
              working.length === 0 && <li className="tray-empty">{trayLabels.nothingComingUp}</li>
            )}
          </ul>
          <AskBox
            name={primary?.displayName}
            session={agents?.find((row) => row.agent.name === primary?.name)?.sessionId ?? undefined}
            disabled={line?.stopped ?? false}
            send={send}
          />
        </section>
      </div>

      <footer className="tray-foot">
        <a className="button button-secondary tray-open-home" href="/ui/" target="_blank" rel="noopener">
          {trayLabels.openHome}
        </a>
        <button
          type="button"
          className="button button-stop tray-stop"
          disabled={line?.stopped ?? false}
          onClick={(event) => void stopAll(event)}
        >
          {trayLabels.stopAll}
        </button>
      </footer>
    </div>
  );
}

/** Spent against the daily limit, clamped to the bar (spec 3.3 §8.1). */
export function SpendBar({ spent, limit }: { spent: number; limit: number }) {
  const percent = Math.min(100, Math.max(0, (spent / limit) * 100));
  return (
    <div
      className="tray-bar"
      role="meter"
      aria-label={trayLabels.spendBar}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round(percent)}
    >
      <span className="tray-bar-fill" style={{ width: `${percent}%` }} />
    </div>
  );
}

/**
 * "Ask Finance": a message to the primary agent's latest conversation, or a new one. While a
 * decision of that conversation is open, it waits behind the decision and is never an answer
 * (§7.4 rule 2).
 */
function AskBox({
  name,
  session,
  disabled,
  send,
}: {
  name?: string;
  session?: string;
  disabled: boolean;
  send: (command: Command, origin: Origin) => Promise<CommandOutcome>;
}) {
  const [fresh] = useState(newSession);
  const [text, setText] = useState("");
  const [note, setNote] = useState("");
  const [problem, setProblem] = useState("");
  const [sending, setSending] = useState(false);
  const field = useId();
  const placeholder = name ? trayLabels.ask(name) : trayLabels.messageYourAgents;

  async function submit(event: FormEvent) {
    event.preventDefault();
    const message = text.trim();
    if (!message || sending) return;
    setSending(true);
    setNote("");
    setProblem("");
    try {
      const outcome = await send({ type: "send", sessionId: session ?? fresh, text: message }, { kind: "keyboard" });
      setText("");
      if (outcome.type === "queued") setNote(outcome.text);
    } catch (error) {
      setProblem(problemText(error));
    } finally {
      setSending(false);
    }
  }

  return (
    <form className="tray-ask" onSubmit={(event) => void submit(event)}>
      <label htmlFor={field} className="visually-hidden">
        {placeholder}
      </label>
      <input
        id={field}
        className="tray-ask-input"
        placeholder={placeholder}
        value={text}
        autoComplete="off"
        disabled={disabled}
        onChange={(event) => setText(event.target.value)}
      />
      {note && (
        <p className="tray-ask-note" role="status">
          {note}
        </p>
      )}
      {problem && (
        <p className="tray-problem" role="alert">
          {problem}
        </p>
      )}
    </form>
  );
}
