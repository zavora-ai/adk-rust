import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import type { Command, DecisionCard, Origin } from "./generated/api";
import type { HomeClient } from "./client";
import { ApiError, problemText } from "./client";
import type { TakeOverSeam } from "./components/DecisionCard";
import { AgentsScreen } from "./agents/AgentsScreen";
import { agentRoutines } from "./agents/routines";
import { agentsPlaceOf } from "./agents/route";
import { Header } from "./components/Header";
import { Developer } from "./developer/Developer";
import { labels } from "./labels";
import { planned as plan, today as todays } from "./plan";
import { usePoll } from "./poll";
import type { Section } from "./route";
import { decisionOf, pathOf, sectionOf } from "./route";
import { MemoryRules } from "./screens/MemoryRules";
import { Threads } from "./screens/Threads";
import { Today } from "./screens/Today";
import { Voice } from "./screens/Voice";
import type { VoiceClient } from "./screens/Voice";
import { VoiceEntryContext, voiceOffered } from "./voice/context";
import type { Seams } from "./seams";
import { readStored, writeStored } from "./stored";
import { applyTheme, nextTheme, storedTheme } from "./theme";
import type { Theme } from "./theme";
import type { WebsiteClient } from "./website/client";
import { websiteLabels } from "./website/labels";
import { Website } from "./website/Website";

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
  website,
}: {
  client: HomeClient;
  seams?: Seams;
  takeOver?: TakeOverSeam;
  initialSection?: Section;
  /** The website's views, where the server serves a website (spec 3.5 §8.2). */
  website?: WebsiteClient;
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
  // The agents screen the address names: New agent or one agent's settings (spec 3.3 §5.7).
  const [agentsPlace, setAgentsPlace] = useState(() => agentsPlaceOf(window.location));
  useEffect(() => {
    const back = () => {
      setSection(sectionOf(window.location.pathname));
      setAgentsPlace(agentsPlaceOf(window.location));
    };
    window.addEventListener("popstate", back);
    return () => window.removeEventListener("popstate", back);
  }, []);

  // The status line carries the mode of the conversation shown: a session's own mode moves on
  // from the agent's default with an approved plan exit or a change elsewhere (spec 3.3 §5.1).
  const conversation = useRef<string | undefined>(undefined);
  const status = usePoll(() => client.status(conversation.current));
  const agents = usePoll(() => client.agents());
  const decisions = usePoll(() => client.decisions());
  // Whether the server serves a website: its tab shows only then (spec 3.5 §8.2).
  const site = usePoll(async () => (website ? ((await website.home()) ?? null) : null), 60_000);
  // Whether the roster ends with New agent, and the primary agent a description goes to.
  const roster = usePoll(async () => (client.agentsWithPrimary ? await client.agentsWithPrimary() : undefined), 10_000);
  const agentsSeam = useMemo(() => client.agentsSeam?.(), [client]);
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

  // The page never scrolls; the section does, under the header, and starts at its top.
  const content = useRef<HTMLDivElement>(null);
  const navigate = useCallback((next: Section) => {
    setSection(next);
    if (window.location.pathname !== pathOf(next)) window.history.pushState(null, "", pathOf(next));
    if (content.current) content.current.scrollTop = 0;
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

  // A deep link to one decision (`/ui/decisions/<id>`, spec 3.3 §7.2): the tray, the phone and
  // voice send the person here for the answers only the home gives. It opens in its thread once,
  // and the address becomes the thread's; a decided one says who answered, a missing one says
  // what went wrong. Without the server's token it waits for the sign-in.
  const linked = useRef(decisionOf(window.location.pathname));
  useEffect(() => {
    const id = linked.current;
    if (id === undefined) return;
    let live = true;
    client
      .decision(id)
      .then((card) => {
        if (!live || linked.current === undefined) return;
        linked.current = undefined;
        window.history.replaceState(null, "", pathOf("threads"));
        openDecision(card);
        if (card.status === "decided") setProblem(card.decided?.text ?? card.statusText);
      })
      .catch((error: unknown) => {
        if (!live || linked.current === undefined) return;
        if (error instanceof ApiError && error.status === 401) return;
        linked.current = undefined;
        window.history.replaceState(null, "", pathOf("threads"));
        setProblem(problemText(error));
      });
    return () => {
      live = false;
    };
    // Read on load, and again after a sign-in; never again once the link is opened.
  }, [client, signIns]);

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
  conversation.current = session;
  useEffect(() => {
    void refreshStatus();
  }, [session, refreshStatus]);

  // Voice, where the server offers it and the page may use the microphone (spec 3.3 §5.6).
  const voiceModels = usePoll(async () => client.voiceModels?.(), 60_000);
  const voice = useMemo(() => voiceClient(client), [client]);
  const voiceEntry = useMemo(
    () =>
      voice && voiceOffered(voiceModels.value?.models.length ?? 0) ? { open: () => navigate("voice") } : undefined,
    [voice, voiceModels.value, navigate],
  );
  if (section === "voice" && voice) {
    // The conversation ended opens in Threads under the agent, in place of its own thread.
    const ended = (sessionId?: string) => {
      const agent = voiceModels.value?.agentName;
      if (sessionId && agent) {
        setSelected(agent);
        setFocusDecision(undefined);
        setVisiting({ agent, session: sessionId });
      }
      navigate("threads");
    };
    return <Voice client={voice} onEnd={ended} />;
  }

  const home = (
    <div className="app">
      <a className="skip-link" href="#content">
        {labels.skipToContent}
      </a>
      <Header
        section={section}
        status={unreachable ? undefined : status.value}
        areas={unreachable ? undefined : areas.value}
        conversationMode={section === "threads" && session ? status.value?.mode?.status : undefined}
        decisions={unreachable ? 0 : (decisions.value?.length ?? 0)}
        website={site.value ? websiteLabels.tab : undefined}
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
      <div id="content" className="content" key={signIns} ref={content}>
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
            canAddAgents={roster.value?.canAdd ?? false}
            removed={roster.value?.removed ?? []}
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
          <MemoryRules
            client={client}
            status={status.value}
            areas={areas.value}
            agents={rows}
            seams={seams}
            onChanged={refresh}
          />
        )}
        {section === "developer" && <Developer client={client} sessionId={session} />}
        {section === "website" && website && <Website client={website} onThreads={() => navigate("threads")} />}
        {section === "agents" && agentsSeam && agentsPlace && (
          <AgentsScreen
            place={agentsPlace}
            seam={agentsSeam}
            rows={rows}
            primary={
              roster.value?.primary
                ? { name: roster.value.primary.name, mode: status.value?.mode ?? undefined }
                : undefined
            }
            decision={(id) => client.decision(id)}
            onDescribe={async (text, origin) => {
              // Describe it: a new thread with the primary agent, holding the request (§5.7).
              const primary = roster.value?.primary?.name ?? rows.find((row) => !row.added)?.agent.name ?? "adk-rust";
              const thread = crypto.randomUUID();
              await client.command({ type: "newThread", agent: primary, sessionId: thread, text }, origin);
              follow(primary, thread);
              select(primary);
              navigate("threads");
            }}
            routinesOf={(agent) => agentRoutines(kernel.value?.processes, kernel.value?.agenda, agent, new Date())}
            confirm={client.confirmAgentChange?.bind(client)}
            onAddRoutine={() => navigate("today")}
            onChanged={refresh}
            onDone={(note) => {
              if (note) setProblem(note);
              navigate("threads");
            }}
          />
        )}
      </div>
    </div>
  );
  return <VoiceEntryContext.Provider value={voiceEntry}>{home}</VoiceEntryContext.Provider>;
}

/** The client's voice routes, when it has them. */
function voiceClient(client: HomeClient): VoiceClient | undefined {
  const { voiceModels, voiceTicket, voiceDecisions, voiceCommand } = client;
  if (!voiceModels || !voiceTicket || !voiceDecisions || !voiceCommand) return undefined;
  return {
    voiceModels: () => voiceModels.call(client),
    voiceTicket: (model, sessionId) => voiceTicket.call(client, model, sessionId),
    voiceDecisions: () => voiceDecisions.call(client),
    voiceCommand: (command, origin) => voiceCommand.call(client, command, origin),
    status: () => client.status(),
  };
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
