import { Suspense, lazy, useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import { ImageLoaderContext, sameOriginLoader } from "./evidence/images";
import type { ImageLoader } from "./evidence/images";
import type { Command, DecisionCard, Origin, Served } from "./generated/api";
import type { HomeClient } from "./client";
import { ApiError, problemText } from "./client";
import type { TakeOverSeam } from "./components/DecisionCard";
import { AgentsScreen } from "./agents/AgentsScreen";
import { agentRoutines } from "./agents/routines";
import { agentsPlaceOf } from "./agents/route";
import { Header } from "./components/Header";
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
import { CommandPalette } from "./components/CommandPalette";
import { voiceLabels } from "./voice/labels";
import { DecisionsInbox } from "./screens/Decisions";
import { CanvasWindow } from "./screens/CanvasWindow";
import type { PaletteItem } from "./components/CommandPalette";
import { applyTheme, nextTheme, storedTheme } from "./theme";
import type { Theme } from "./theme";
import type { VerbsClient } from "./verbs/client";
import type { WebsiteClient } from "./website/client";
import { websiteLabels } from "./website/labels";
import { Website } from "./website/Website";
import type { Correction } from "./viewer/Viewer";
import type { ViewerPlace } from "./viewer/route";
import { viewerOf, viewerPath } from "./viewer/route";
import type { WorkflowClient } from "./workflow/client";
import type { Opened } from "./camera/Camera";
import type { CameraPlace } from "./camera/route";
import { cameraOf, cameraPath } from "./camera/route";

// The annotated viewer loads only when it opens: the home's main chunk stays within its budget.
const Viewer = lazy(() => import("./viewer/Viewer").then((module) => ({ default: module.Viewer })));
// So does the live camera surface (spec 3.7 §8.2).
const Camera = lazy(() => import("./camera/Camera").then((module) => ({ default: module.Camera })));

/** Where this tab keeps the session each agent's thread follows. */
const FOLLOWED_KEY = "adk.home.followed";

/**
 * The web home (spec 3.3 §5.1): Threads, Today and Memory & rules under one header with the
 * status line and Stop all. Everything it shows is read from the server; what it keeps itself
 * — the conversation each thread follows — lasts across a reload of the tab (§6).
 */
// The developer panel loads when opened: few people open it, and it is large (graph, tree,
// trace, protocols).
const Developer = lazy(() => import("./developer/Developer").then((module) => ({ default: module.Developer })));

export function App({
  client,
  seams = {},
  takeOver,
  initialSection,
  initialAgent,
  website,
  verbs,
  workflow,
  initialViewer,
  initialCamera,
}: {
  client: HomeClient;
  seams?: Seams;
  takeOver?: TakeOverSeam;
  initialSection?: Section;
  /** The agent whose thread opens first, by its name in the runtime; the first row's otherwise. */
  initialAgent?: string;
  /** The website's views, where the server serves a website (spec 3.5 §8.2). */
  website?: WebsiteClient;
  /** Screen actions, where the server takes them (spec 3.3 §2 item 7). */
  verbs?: VerbsClient;
  /** The workflow observer's Runs tab, where the server serves it (spec 3.3 §12). */
  workflow?: WorkflowClient;
  /** The annotated viewer open first, with a correction under way (fixtures). */
  initialViewer?: { place: ViewerPlace; correction?: Correction };
  /** The live camera surface open first, a zone editor or a card open on it (fixtures). */
  initialCamera?: { place: CameraPlace; opened?: Opened };
}) {
  const [section, setSection] = useState<Section>(() => initialSection ?? sectionOf(window.location.pathname));
  const [theme, setTheme] = useState<Theme>(storedTheme);
  const [palette, setPalette] = useState(false);
  // ⌘K or Ctrl+K opens the command palette anywhere in the home.
  useEffect(() => {
    const onKey = (event: globalThis.KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && !event.altKey && event.key.toLowerCase() === "k") {
        event.preventDefault();
        setPalette((shown) => !shown);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
  const [problem, setProblem] = useState("");
  const [selected, setSelected] = useState<string | undefined>(initialAgent);
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
  // The annotated viewer of one image, opened from a thread, a card or a tool line (spec 3.7
  // §8.2, board choice V1): a view in place of the section, under Threads.
  const [viewing, setViewing] = useState<ViewerPlace | undefined>(
    () => initialViewer?.place ?? viewerOf(window.location),
  );
  // The live camera surface (spec 3.7 §8.2, board choice C1): a view under the agent, in place
  // of the section, under Threads.
  const [watching, setWatching] = useState<CameraPlace | undefined>(
    () => initialCamera?.place ?? cameraOf(window.location),
  );
  useEffect(() => {
    const back = () => {
      setSection(sectionOf(window.location.pathname));
      setAgentsPlace(agentsPlaceOf(window.location));
      setViewing(viewerOf(window.location));
      setWatching(cameraOf(window.location));
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
  // A part of the home the status line leaves out is never asked for, so a server built or
  // mounted without it answers no request for it (spec 3.3 §6.1).
  const serving = useCallback(
    async (part: Served) => {
      const served = await client.serves?.();
      return served === undefined || served.has(part);
    },
    [client],
  );
  // Whether the server serves a website: its tab shows only then (spec 3.5 §8.2).
  const site = usePoll(
    async () => (website && (await serving("website")) ? ((await website.home()) ?? null) : null),
    60_000,
  );
  // Whether the server takes screen actions, and what Coming up may be snoozed to (spec 3.3 §2 item 7).
  const screens = usePoll(
    async () => (verbs && (await serving("verbs")) ? ((await verbs.list()) ?? null) : null),
    10_000,
  );
  // Whether the server observes graph runs: the developer panel's Runs tab shows only then.
  const graphs = usePoll(
    async () => (workflow && (await serving("workflow")) ? ((await workflow.graphs()) ?? null) : null),
    60_000,
  );
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
  const { refresh: refreshScreens } = screens;
  const refresh = useCallback(() => {
    void refreshStatus();
    void refreshAgents();
    void refreshDecisions();
    void refreshKernel();
    void refreshAway();
    void refreshAreas();
    void refreshScreens();
  }, [refreshStatus, refreshAgents, refreshDecisions, refreshKernel, refreshAway, refreshAreas, refreshScreens]);

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
    setViewing(undefined);
    setWatching(undefined);
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
  // The agent shown while none is chosen. It applies to the latest choice, so a decision opened
  // between the roster's render and this call, such as a deep link's, keeps its thread.
  const showFirst = useCallback((agent: string) => setSelected((chosen) => chosen ?? agent), []);
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

  const viewerClient = useMemo(() => client.viewerClient?.(), [client]);
  const openViewer = useCallback((place: ViewerPlace) => {
    const path = viewerPath(place);
    if (`${window.location.pathname}${window.location.search}` !== path) window.history.pushState(null, "", path);
    setSection("threads");
    setWatching(undefined);
    setViewing(place);
    if (content.current) content.current.scrollTop = 0;
  }, []);
  const cameraClient = useMemo(() => client.cameraClient?.(), [client]);
  const openCamera = useCallback((place: CameraPlace) => {
    const path = cameraPath(place);
    if (`${window.location.pathname}${window.location.search}` !== path) window.history.pushState(null, "", path);
    setSection("threads");
    setViewing(undefined);
    setWatching(place);
    if (content.current) content.current.scrollTop = 0;
  }, []);
  // Every link to the viewer — a tool line's, a card's "Open the source image" — and to a camera
  // opens it in place, keeping the home's state; a modified click still opens a new tab.
  useEffect(() => {
    if (!viewerClient && !cameraClient) return;
    const onClick = (event: MouseEvent) => {
      if (event.defaultPrevented || event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
      const link = (event.target as Element | null)?.closest?.("a[href]");
      if (!(link instanceof HTMLAnchorElement) || link.target) return;
      const url = new URL(link.href, window.location.href);
      if (url.origin !== window.location.origin) return;
      const place = viewerClient ? viewerOf(url) : undefined;
      if (place) {
        event.preventDefault();
        openViewer(place);
        return;
      }
      const camera = cameraClient ? cameraOf(url) : undefined;
      if (!camera) return;
      event.preventDefault();
      openCamera(camera);
    };
    document.addEventListener("click", onClick);
    return () => document.removeEventListener("click", onClick);
  }, [viewerClient, cameraClient, openViewer, openCamera]);
  // Which agents watch a camera: their thread links to it (board choice C1). A server that serves
  // no cameras says so once and is not asked again.
  const noCameras = useRef(false);
  const cameras = usePoll(async () => {
    if (!cameraClient || noCameras.current || !(await serving("cameras"))) return null;
    try {
      return await cameraClient.cameras();
    } catch (error) {
      if (error instanceof ApiError && error.code === "surface.unsupported") noCameras.current = true;
      return null;
    }
  }, 30_000);
  const cameraLinks = useMemo(() => {
    const links: Record<string, { href: string; text: string }> = {};
    const view = cameras.value;
    const agent = view?.agent?.agent.name;
    if (view && agent && view.tiles.length > 0) {
      const first = view.tiles[0];
      links[agent] =
        view.tiles.length === 1
          ? { href: cameraPath({ camera: first.id }), text: first.name }
          : { href: cameraPath({}), text: `${view.title} · ${view.countText}` };
    }
    return links;
  }, [cameras.value]);
  const closeViewer = useCallback(
    (agent: string, session: string) => {
      if (agent) {
        setSelected(agent);
        setFocusDecision(undefined);
        setVisiting(followed[agent] === session ? undefined : { agent, session });
      }
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
  // A decision's images load as the signed-in viewer (spec 3.7 §8.2).
  const imageLoader = useMemo<ImageLoader>(
    () => (client.image ? (href) => client.image!(href) : sameOriginLoader),
    [client],
  );

  // A drawing in its own window: the canvas alone, no header (spec 3.3 §5.1).
  if (section === "canvas") {
    return (
      <ImageLoaderContext.Provider value={imageLoader}>
        <CanvasWindow client={client} send={send} />
      </ImageLoaderContext.Provider>
    );
  }
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
    return <Voice client={voice} onEnd={ended} screenActions={screens.value != null} />;
  }

  const themeName = (value: Theme) =>
    value === "system" ? labels.themeSystem : value === "light" ? labels.themeLight : labels.themeDark;
  const paletteItems: PaletteItem[] = [
    ...[
      { section: "threads" as const, label: labels.threads },
      { section: "decisions" as const, label: labels.decisions },
      { section: "today" as const, label: labels.today },
      { section: "memory" as const, label: labels.memoryAndRules },
      { section: "developer" as const, label: labels.developer },
      ...(site.value ? [{ section: "website" as const, label: websiteLabels.tab }] : []),
    ].map((tab) => ({ id: `go-${tab.section}`, group: labels.paletteGoTo, label: tab.label, run: () => navigate(tab.section) })),
    ...rows.map((row) => ({
      id: `agent-${row.agent.name}`,
      group: labels.agents,
      label: labels.paletteOpenThread(row.agent.displayName),
      hint: row.statusText,
      keywords: row.agent.name,
      run: () => {
        select(row.agent.name);
        navigate("threads");
      },
    })),
    // Opening a decision shows its card; nothing here answers it (spec 3.3 §7.4).
    ...(decisions.value ?? []).map((card) => ({
      id: `decision-${card.id}`,
      group: labels.paletteDecisions,
      label: card.heading,
      hint: `${card.agent.displayName} · ${card.riskText}`,
      run: () => openDecision(card),
    })),
    ...(status.value?.stopped
      ? []
      : [{ id: "stop-all", group: labels.paletteActions, label: labels.stopAll, run: () => void act({ type: "stopAll" }, { kind: "keyboard" }) }]),
    {
      id: "theme",
      group: labels.paletteActions,
      label: labels.paletteTheme(themeName(nextTheme(theme))),
      keywords: "dark light appearance",
      run: () => setTheme(nextTheme),
    },
    ...(voiceEntry ? [{ id: "voice", group: labels.paletteActions, label: voiceLabels.talk, keywords: "voice microphone", run: voiceEntry.open }] : []),
    ...(agentsSeam ? [{ id: "add-agent", group: labels.paletteActions, label: labels.paletteAddAgent, keywords: "new agent", run: () => navigate("agents") }] : []),
  ];

  const home = (
    <div className="app">
      <CommandPalette open={palette} items={paletteItems} onClose={() => setPalette(false)} />
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
        onPalette={() => setPalette(true)}
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
        {watching && cameraClient && (
          <Suspense fallback={<div className="vw-loading" aria-busy="true" />}>
            <Camera
              client={cameraClient}
              place={watching}
              onNavigate={openCamera}
              onOpenThread={closeViewer}
              onOpenDecision={openDecision}
              onMemory={() => navigate("memory")}
              initialOpened={watching === initialCamera?.place ? initialCamera.opened : undefined}
            />
          </Suspense>
        )}
        {!(watching && cameraClient) && viewing && viewerClient && (
          <Suspense fallback={<div className="vw-loading" aria-busy="true" />}>
            <Viewer
              client={viewerClient}
              place={viewing}
              onNavigate={openViewer}
              onOpenThread={closeViewer}
              initialCorrection={viewing === initialViewer?.place ? initialViewer.correction : undefined}
            />
          </Suspense>
        )}
        {!(viewing && viewerClient) && !(watching && cameraClient) && section === "threads" && (
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
            onShowFirst={showFirst}
            followed={followed}
            onFollow={follow}
            visiting={visiting}
            onBack={() => {
              setVisiting(undefined);
              setFocusDecision(undefined);
            }}
            onVisit={(agent, chosen) => {
              setFocusDecision(undefined);
              setVisiting(followed[agent] === chosen ? undefined : { agent, session: chosen });
            }}
            focusDecision={focusDecision}
            onOpenDecision={openDecision}
            canAddAgents={roster.value?.canAdd ?? false}
            removed={roster.value?.removed ?? []}
            verbs={screens.value ?? undefined}
            snooze={verbs ? (item, choice, origin) => verbs.snooze(item, choice, origin) : undefined}
            cameraLinks={cameraLinks}
          />
        )}
        {section === "decisions" && (
          <DecisionsInbox
            decisions={decisions.value ?? []}
            send={send}
            onSettled={() => refresh()}
            takeOver={takeOver}
            onOpenThread={openDecision}
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
            screens={screens.value?.verbs}
          />
        )}
        {section === "developer" && (
          <Suspense fallback={null}>
            <Developer
              client={client}
              sessionId={session}
              workflow={workflow && graphs.value ? { client: workflow, graphs: graphs.value } : undefined}
            />
          </Suspense>
        )}
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
        {section === "agents" && !(agentsSeam && agentsPlace) && (
          <section className="section-empty" aria-labelledby="agents-off-heading">
            <h2 id="agents-off-heading">{labels.agentsOffHeading}</h2>
            <p>{labels.agentsOffText}</p>
            <button type="button" className="button button-secondary" onClick={() => navigate("threads")}>
              {labels.backToThreads}
            </button>
          </section>
        )}
      </div>
    </div>
  );
  return (
    <ImageLoaderContext.Provider value={imageLoader}>
      <VoiceEntryContext.Provider value={voiceEntry}>{home}</VoiceEntryContext.Provider>
    </ImageLoaderContext.Provider>
  );
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
