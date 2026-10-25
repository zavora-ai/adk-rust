import { useCallback, useEffect, useId, useMemo, useState } from "react";
import type { Command, DecisionCard, Origin } from "./generated/api";
import type { HomeClient } from "./client";
import { ApiError, problemText } from "./client";
import type { TakeOverSeam } from "./components/DecisionCard";
import { Header } from "./components/Header";
import { Developer } from "./developer/Developer";
import { labels } from "./labels";
import { planned as plan, today as todays } from "./plan";
import { usePoll } from "./poll";
import type { Section } from "./route";
import { pathOf, sectionOf } from "./route";
import { MemoryRules } from "./screens/MemoryRules";
import { Threads } from "./screens/Threads";
import { Today } from "./screens/Today";
import type { Seams } from "./seams";
import { readStored, writeStored } from "./stored";
import { applyTheme, nextTheme, storedTheme } from "./theme";
import type { Theme } from "./theme";

/** Where this tab keeps the session each agent's thread follows. */
const FOLLOWED_KEY = "adk.home.followed";

/**
 * The web home (spec 3.3 §5.1): Threads, Today and Memory & rules under one header with the
 * status line and Stop all. Everything it shows is read from the server; what it keeps itself
 * — the conversation each thread follows — lasts across a reload of the tab (§6).
 */
export function App({
  client,
  seams = {},
  takeOver,
  initialSection,
}: {
  client: HomeClient;
  seams?: Seams;
  takeOver?: TakeOverSeam;
  initialSection?: Section;
}) {
  const [section, setSection] = useState<Section>(() => initialSection ?? sectionOf(window.location.pathname));
  const [theme, setTheme] = useState<Theme>(storedTheme);
  const [problem, setProblem] = useState("");
  const [selected, setSelected] = useState<string>();
  const [focusDecision, setFocusDecision] = useState<string>();
  // A decision of another session is shown in place of the person's conversation until they go
  // back; the conversation each thread follows (`followed`) never moves to it.
  const [visiting, setVisiting] = useState<{ agent: string; session: string }>();
  // Each sign-in reads every section again, with the token.
  const [signIns, setSignIns] = useState(0);
  const [followed, setFollowed] = useState<Record<string, string>>(() => readStored(FOLLOWED_KEY, {}));
  useEffect(() => writeStored(FOLLOWED_KEY, followed), [followed]);

  useEffect(() => applyTheme(theme), [theme]);
  useEffect(() => {
    const back = () => setSection(sectionOf(window.location.pathname));
    window.addEventListener("popstate", back);
    return () => window.removeEventListener("popstate", back);
  }, []);

  const status = usePoll(() => client.status());
  const agents = usePoll(() => client.agents());
  const decisions = usePoll(() => client.decisions());
  // "While you were away" and each area's settings, read once for every section that shows
  // them, so Today, the morning briefing and the status line never disagree (spec 3.3 §5.1).
  const away = usePoll(async () => (seams.whileAway ? await seams.whileAway.read() : undefined), 10_000);
  const areas = usePoll(async () => (seams.limits ? await seams.limits.areas() : undefined), 10_000);
  const kernel = usePoll(async () => {
    const [processes, agenda] = await Promise.all([
      client.processes().catch(() => undefined),
      client.agenda().catch(() => undefined),
    ]);
    return { processes, agenda };
  }, 10_000);

  const { refresh: refreshStatus } = status;
  const { refresh: refreshAgents } = agents;
  const { refresh: refreshDecisions } = decisions;
  const { refresh: refreshKernel } = kernel;
  const { refresh: refreshAway } = away;
  const { refresh: refreshAreas } = areas;
  const refresh = useCallback(() => {
    void refreshStatus();
    void refreshAgents();
    void refreshDecisions();
    void refreshKernel();
    void refreshAway();
    void refreshAreas();
  }, [refreshStatus, refreshAgents, refreshDecisions, refreshKernel, refreshAway, refreshAreas]);

  const send = useCallback(
    async (command: Command, origin: Origin) => {
      const outcome = await client.command(command, origin);
      refresh();
      return outcome;
    },
    [client, refresh],
  );

  const navigate = useCallback((next: Section) => {
    setSection(next);
    if (window.location.pathname !== pathOf(next)) window.history.pushState(null, "", pathOf(next));
    window.scrollTo?.({ top: 0 });
  }, []);

  const follow = useCallback(
    (agent: string, session: string) =>
      setFollowed((all) => (all[agent] === session ? all : { ...all, [agent]: session })),
    [],
  );
  const select = useCallback((agent: string) => {
    setSelected(agent);
    setFocusDecision(undefined);
    setVisiting(undefined);
  }, []);
  // A decision opens in its own session's thread, whichever session the agent's row names;
  // one of another session than the person's conversation is visited, not followed.
  const openDecision = useCallback(
    (card: DecisionCard) => {
      setSelected(card.agent.name);
      setVisiting(
        followed[card.agent.name] === card.sessionId ? undefined : { agent: card.agent.name, session: card.sessionId },
      );
      setFocusDecision(card.id);
      navigate("threads");
    },
    [followed, navigate],
  );

  async function act(command: Command, origin: Origin) {
    setProblem("");
    try {
      await send(command, origin);
    } catch (error) {
      setProblem(problemText(error));
    }
  }

  const unauthorized = [status.error, agents.error].some((error) => error instanceof ApiError && error.status === 401);
  // The last read failed before reaching the server: what is on screen may be stale.
  const unreachable = !unauthorized && status.error !== undefined && !(status.error instanceof ApiError);
  const rows = agents.value ?? [];
  // The plan follows the kernel's reads; the clock is read with them.
  const planned = useMemo(
    () => plan(kernel.value?.processes, kernel.value?.agenda, agents.value, new Date()),
    [kernel.value, agents.value],
  );
  const routinesToday = useMemo(
    () => todays(kernel.value?.processes, kernel.value?.agenda, agents.value, new Date()),
    [kernel.value, agents.value],
  );
  const session = (selected && followed[selected]) ?? rows[0]?.sessionId ?? undefined;

  return (
    <div className="app">
      <a className="skip-link" href="#content">
        {labels.skipToContent}
      </a>
      <Header
        section={section}
        status={unreachable ? undefined : status.value}
        areas={unreachable ? undefined : areas.value}
        decisions={unreachable ? 0 : (decisions.value?.length ?? 0)}
        theme={theme}
        onNavigate={navigate}
        onTheme={() => setTheme(nextTheme)}
        onStop={(origin) => void act({ type: "stopAll" }, origin)}
        onResume={(origin) => void act({ type: "resumeAll" }, origin)}
      />
      {(problem || unauthorized || unreachable) && (
        <p className="banner" role="alert">
          {unauthorized ? labels.signInNeeded : unreachable ? labels.cantReach : problem}
        </p>
      )}
      {unauthorized && client.signIn && (
        <SignIn
          onSignIn={(token) => {
            client.signIn?.(token);
            setSignIns((count) => count + 1);
            refresh();
          }}
        />
      )}
      <div id="content" className="content" key={signIns}>
        {section === "threads" && (
          <Threads
            client={client}
            agents={rows}
            decisions={decisions.value ?? []}
            status={status.value}
            planned={planned}
            send={send}
            onChanged={refresh}
            onToday={() => navigate("today")}
            awayPage={away.value}
            areas={areas.value}
            onDeveloper={() => navigate("developer")}
            takeOver={takeOver}
            selected={selected}
            onSelect={select}
            followed={followed}
            onFollow={follow}
            visiting={visiting}
            onBack={() => {
              setVisiting(undefined);
              setFocusDecision(undefined);
            }}
            focusDecision={focusDecision}
            onOpenDecision={openDecision}
          />
        )}
        {section === "today" && (
          <Today
            client={client}
            agents={rows}
            status={status.value}
            agenda={kernel.value?.agenda}
            away={away.value}
            areas={areas.value}
            onAwayChanged={() => void refreshAway()}
            routinesToday={routinesToday}
            canAddRoutine={kernel.value?.processes !== undefined}
            seams={seams}
            onChanged={refresh}
            onOpenAgent={(agent) => {
              // The person's own conversation first, then any other of the agent's.
              const cards = decisions.value?.filter((card) => card.agent.name === agent) ?? [];
              const waiting = cards.find((card) => card.sessionId === followed[agent]) ?? cards[0];
              if (waiting) {
                openDecision(waiting);
                return;
              }
              select(agent);
              navigate("threads");
            }}
          />
        )}
        {section === "memory" && (
          <MemoryRules client={client} status={status.value} areas={areas.value} seams={seams} onChanged={refresh} />
        )}
        {section === "developer" && <Developer client={client} sessionId={session} />}
      </div>
    </div>
  );
}

/**
 * The server asked for its token (`adk-rust serve --auth-token`): the person types it once,
 * and this tab sends it with every request.
 */
function SignIn({ onSignIn }: { onSignIn: (token: string) => void }) {
  const [token, setToken] = useState("");
  const field = useId();
  return (
    <form
      className="sign-in"
      onSubmit={(event) => {
        event.preventDefault();
        if (token.trim()) onSignIn(token);
      }}
    >
      <label htmlFor={field}>{labels.serverToken}</label>
      <input
        id={field}
        type="password"
        autoComplete="off"
        value={token}
        onChange={(event) => setToken(event.target.value)}
      />
      <button type="submit" className="button button-primary" disabled={!token.trim()}>
        {labels.signIn}
      </button>
    </form>
  );
}
