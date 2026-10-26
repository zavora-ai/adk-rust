import { useCallback, useEffect, useMemo, useState } from "react";
import type { DecisionCard } from "../generated/api";
import { ApiError, problemText } from "../client";
import { usePoll } from "../poll";
import { applyTheme, storedTheme } from "../theme";
import type { PhoneClient } from "./client";
import { Decide } from "./Decide";
import { Decisions } from "./Decisions";
import { Agents, ThisPhone, TodayLines } from "./Glance";
import { phoneLabels } from "./labels";
import { Pair } from "./Pair";
import { badge } from "./push";
import type { PhoneScreen } from "./route";
import { phonePathOf, phoneScreenOf } from "./route";
import "./phone.css";

/** How far back Decisions shows answered decisions, here or elsewhere. */
const DECIDED_WINDOW_MS = 24 * 60 * 60 * 1000;
/** How often the answered decisions are read again. */
const DECIDED_POLL_MS = 30_000;

type Tab = "threads" | "decisions" | "today" | "memory";

const TABS: Array<{ tab: Tab; label: string }> = [
  { tab: "threads", label: phoneLabels.threads },
  { tab: "decisions", label: phoneLabels.decisions },
  { tab: "today", label: phoneLabels.today },
  { tab: "memory", label: phoneLabels.memory },
];

/**
 * The phone (spec 3.3 §5.4): the web home installed on a phone, woken by push. Decisions
 * carries the badge on its heading, its tab and the app's icon. What a paired device cannot
 * do is not shown: no composer, no Resume all, no pairing, no operator rows. A read that
 * fails is said, never shown as nothing waiting; a pairing that ended offers to pair again.
 */
export function PhoneApp({ client, initial }: { client: PhoneClient; initial?: PhoneScreen }) {
  const [screen, setScreen] = useState<PhoneScreen>(() => initial ?? phoneScreenOf(window.location));
  const [settled, setSettled] = useState<Record<string, DecisionCard>>({});
  const [problem, setProblem] = useState("");
  const [, setPairedAt] = useState(0);

  useEffect(() => applyTheme(storedTheme()), []);
  useEffect(() => {
    const back = () => setScreen(phoneScreenOf(window.location));
    window.addEventListener("popstate", back);
    return () => window.removeEventListener("popstate", back);
  }, []);

  const go = useCallback((next: PhoneScreen) => {
    const path = phonePathOf(next);
    if (window.location.pathname !== path) window.history.pushState(null, "", path);
    setScreen(next);
  }, []);

  const paired = client.paired();
  const status = usePoll(async () => (paired ? await client.status() : undefined));
  const cards = usePoll(async () => (paired ? await client.decisions() : []));
  // What was answered lately, here or elsewhere, is read less often: it reads the history.
  const decided = usePoll(
    async () =>
      paired
        ? (await client.decisions(new Date(Date.now() - DECIDED_WINDOW_MS).toISOString())).filter(
            (card) => card.status === "decided",
          )
        : [],
    DECIDED_POLL_MS,
  );
  const { refresh: refreshCards } = cards;
  const { refresh: refreshDecided } = decided;
  const { refresh: refreshStatus } = status;

  // An answer given here shows at once, before the next read.
  const shown = useMemo(() => {
    const list = [...(cards.value ?? []), ...(decided.value ?? [])].map((card) => settled[card.id] ?? card);
    const seen = new Set<string>();
    const unique = list.filter((card) => !seen.has(card.id) && seen.add(card.id));
    for (const card of Object.values(settled)) if (!seen.has(card.id)) unique.push(card);
    return unique;
  }, [cards.value, decided.value, settled]);
  const waiting = status.value?.decisions ?? shown.filter((card) => card.status === "waiting").length;
  useEffect(() => badge(waiting), [waiting]);
  // The device's token no longer works: the phone was removed or its pairing ended.
  const failed = cards.error ?? status.error ?? decided.error;
  const unpaired = failed instanceof ApiError && failed.status === 401;
  const readProblem = failed === undefined ? undefined : problemText(failed);

  const onSettled = useCallback(
    (card: DecisionCard) => {
      setSettled((current) => ({ ...current, [card.id]: card }));
      void refreshCards();
      void refreshDecided();
      void refreshStatus();
    },
    [refreshCards, refreshDecided, refreshStatus],
  );

  if (!paired || screen.screen === "pair") {
    return (
      <div className="phone">
        <Pair
          client={client}
          code={screen.screen === "pair" ? screen.code : undefined}
          onDone={() => {
            setPairedAt(Date.now());
            go({ screen: "decisions" });
            void refreshCards();
            void refreshStatus();
          }}
        />
      </div>
    );
  }

  async function stopAll() {
    setProblem("");
    try {
      await client.stop();
      await refreshStatus();
    } catch (error) {
      setProblem(problemText(error));
    }
  }

  const tab: Tab = screen.screen === "decide" ? "decisions" : screen.screen;
  const line = [status.value?.mode?.status, status.value?.spend?.text].filter(Boolean).join(" · ");
  const title = TABS.find((entry) => entry.tab === tab)?.label ?? phoneLabels.decisions;

  return (
    <div className="phone">
      {screen.screen === "decide" ? (
        <Decide client={client} id={screen.id} onBack={() => go({ screen: "decisions" })} onSettled={onSettled} />
      ) : (
        <main className="phone-main">
          <header className="phone-header">
            {line && <p className="phone-status">{line}</p>}
            <div className="phone-title-row">
              <h1 className="phone-display">
                {title}
                {tab === "decisions" && waiting > 0 && (
                  <span className="phone-badge" aria-label={`${waiting} waiting`}>
                    {waiting}
                  </span>
                )}
              </h1>
              <button type="button" className="phone-stop" onClick={() => void stopAll()}>
                {phoneLabels.stopAll}
              </button>
            </div>
            {status.value?.stopped && <p className="phone-note">{phoneLabels.allStopped}</p>}
            {unpaired && (
              <div className="phone-note" role="alert">
                <p>{phoneLabels.pairingEnded}</p>
                <button
                  type="button"
                  className="phone-button phone-button-secondary"
                  onClick={() => void client.signOut().then(() => go({ screen: "pair" }))}
                >
                  {phoneLabels.pairAgain}
                </button>
              </div>
            )}
            {problem && (
              <p className="phone-problem" role="alert">
                {problem}
              </p>
            )}
          </header>
          {tab === "decisions" && (
            <Decisions
              cards={shown}
              problem={readProblem}
              onOpen={(card) => go({ screen: "decide", id: card.id })}
            />
          )}
          {tab === "threads" && <Agents client={client} />}
          {tab === "today" && <TodayLines client={client} />}
          {tab === "memory" && <ThisPhone client={client} onSignedOut={() => go({ screen: "pair" })} />}
        </main>
      )}
      {screen.screen !== "decide" && (
      <nav className="phone-tabs" aria-label="Sections">
        {TABS.map((entry) => (
          <button
            key={entry.tab}
            type="button"
            className={`phone-tab${entry.tab === tab ? " phone-tab-active" : ""}`}
            aria-current={entry.tab === tab ? "page" : undefined}
            onClick={() => go({ screen: entry.tab })}
          >
            {entry.label}
            {entry.tab === "decisions" && waiting > 0 && (
              <span className="phone-badge phone-badge-small" aria-hidden="true">
                {waiting}
              </span>
            )}
          </button>
        ))}
      </nav>
      )}
    </div>
  );
}
