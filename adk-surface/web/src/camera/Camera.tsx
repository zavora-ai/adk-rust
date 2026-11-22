import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import type { CameraView, CamerasView, DecisionCard, Origin, StageView } from "../generated/api";
import { problemText } from "../client";
import type { Box } from "../viewer/Stage";
import { CameraStage } from "./CameraStage";
import type { Layers } from "./CameraStage";
import type { CameraClient } from "./client";
import { CheckIcon, ChevronIcon, FlagIcon, StopIcon, ZoneIcon } from "./icons";
import { cameraLabels as words } from "./labels";
import { AgentHead, Budget, DecisionMini, Notice, Sources, StatePill, Switch, Tracks, Watch } from "./parts";
import { RaiseCard } from "./RaiseCard";
import type { CameraPlace } from "./route";
import { ZoneEditor } from "./ZoneEditor";
import "./camera.css";

/** How often the whole view is read again, for tracks, watches and the budget. */
const READ_AGAIN_MS = 3_000;
/** The largest the picture is drawn (board choice C1). */
const STAGE_MAX = 940;
/** The gap between the grid's tiles, as `.cam-grid` sets it. */
const TILE_GAP = 14;

/** What opens over the camera. */
export type Opened =
  | { kind: "zone"; zone: string; corners?: Box }
  | { kind: "card"; frozen: StageView; note?: string };

/**
 * The live camera surface on the web home (spec 3.7 §8.2, board choice C1): a view under the
 * agent — the stream large with its overlays, the source switch and Stop the stream in the bar,
 * and beside it the agent, its Decisions, "Watching for", "Frame budget" and "Tracks now"; every
 * source in a strip below. `/ui/cameras` shows every camera as a grid (board choice C8). Drawn
 * to the signed-off boards `camera-*-web`.
 */
export function Camera({
  client,
  place,
  onNavigate,
  onOpenThread,
  onOpenDecision,
  onMemory,
  initialOpened,
}: {
  client: CameraClient;
  place: CameraPlace;
  onNavigate: (place: CameraPlace) => void;
  onOpenThread: (agent: string, session: string) => void;
  onOpenDecision: (card: DecisionCard) => void;
  /** "Change the daily frame limit": Memory & rules. */
  onMemory: () => void;
  /** A zone editor or a card open on arrival (fixtures). */
  initialOpened?: Opened;
}) {
  if (!place.camera) {
    return <Grid client={client} onNavigate={onNavigate} onOpenThread={onOpenThread} onOpenDecision={onOpenDecision} />;
  }
  return (
    <One
      key={place.camera}
      client={client}
      id={place.camera}
      editZone={place.zone === true}
      onNavigate={onNavigate}
      onOpenThread={onOpenThread}
      onOpenDecision={onOpenDecision}
      onMemory={onMemory}
      initialOpened={initialOpened}
    />
  );
}

/** The width the picture may take in its column, measured once `shown` puts the column on screen. */
function useStageWidth(max: number, shown: boolean) {
  const ref = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(max);
  useLayoutEffect(() => {
    const element = ref.current;
    if (!element || typeof ResizeObserver === "undefined") return;
    const measure = () => setWidth(Math.max(280, Math.min(max, element.clientWidth - 48)));
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, [max, shown]);
  return { ref, width };
}

function One({
  client,
  id,
  editZone,
  onNavigate,
  onOpenThread,
  onOpenDecision,
  onMemory,
  initialOpened,
}: {
  client: CameraClient;
  id: string;
  editZone: boolean;
  onNavigate: (place: CameraPlace) => void;
  onOpenThread: (agent: string, session: string) => void;
  onOpenDecision: (card: DecisionCard) => void;
  onMemory: () => void;
  initialOpened?: Opened;
}) {
  const [view, setView] = useState<CameraView>();
  const [problem, setProblem] = useState("");
  const [note, setNote] = useState("");
  const [layers, setLayers] = useState<Layers>({ boxes: true, masks: true, tracks: true, zones: true });
  const [opened, setOpened] = useState<Opened | undefined>(initialOpened);
  const [busy, setBusy] = useState(false);
  const { ref, width } = useStageWidth(STAGE_MAX, view !== undefined);

  const read = useCallback(async () => {
    try {
      const next = await client.camera(id);
      setView(next);
      setProblem("");
      return next;
    } catch (error) {
      setProblem(problemText(error));
      return undefined;
    }
  }, [client, id]);

  useEffect(() => {
    void read().then((first) => {
      const zone = first?.zones[0];
      if (editZone && zone && first?.actions.zone) setOpened({ kind: "zone", zone: zone.id });
    });
    const timer = window.setInterval(() => void read(), READ_AGAIN_MS);
    return () => window.clearInterval(timer);
  }, [read, editZone]);

  // Each frame that passes the server's gate redraws the picture; anything else reads it all again.
  useEffect(() => {
    if (!client.feed) return;
    const abort = new AbortController();
    void (async () => {
      try {
        for await (const message of client.feed!(id, abort.signal)) {
          if (message.type === "frame") setView((now) => (now ? { ...now, stage: message.stage } : now));
          else void read();
        }
      } catch {
        // The view is read again every few seconds whatever happens to the feed.
      }
    })();
    return () => abort.abort();
  }, [client, id, read]);

  async function act(run: () => Promise<CameraView>) {
    setBusy(true);
    setProblem("");
    try {
      setView(await run());
    } catch (error) {
      setProblem(problemText(error));
    } finally {
      setBusy(false);
    }
  }

  async function freeze() {
    const frame = view?.stage.frame;
    if (!frame) return;
    try {
      setOpened({ kind: "card", frozen: await client.freeze(id, frame.digest) });
    } catch (error) {
      setProblem(problemText(error));
    }
  }

  if (!view) {
    return problem ? (
      <p className="cam-problem cam-page-problem" role="alert">
        {problem}
      </p>
    ) : (
      <div className="cam-loading" aria-busy="true" />
    );
  }

  const zone = view.zones[0];
  const thread = () => onOpenThread(view.agent.agent.name, view.agent.sessionId);
  const editingZone = opened?.kind === "zone" ? view.zones.find((one) => one.id === opened.zone) : undefined;
  return (
    <div className="cam">
      <div className="cam-subbar">
        <button type="button" className="cam-back" onClick={thread}>
          <ChevronIcon />
          {view.agent.agent.displayName}
        </button>
        <span className="cam-rule" aria-hidden="true" />
        <h1 className="cam-title">{view.name}</h1>
        <StatePill tone={view.tone} state={view.state}>
          {view.stateText}
        </StatePill>
        <span className="cam-meta">{view.ingest}</span>
        <span className="cam-spacer" />
        <Switch
          on={view.on}
          disabled={busy || !view.actions.switch}
          label={words.cameraSwitch(view.name)}
          onSwitch={(on) => void act(() => client.switchTo(id, on))}
        />
        <button type="button" className="cam-btn cam-btn-sm" disabled={busy || !view.actions.stop} onClick={() => void act(() => client.stop(id))}>
          <StopIcon />
          {words.stopStream}
        </button>
      </div>
      <div className="cam-body">
        <div className="cam-canvas" ref={ref}>
          {view.notice && (
            <Notice notice={view.notice} onRetry={() => void act(() => client.retry(id))} onLimit={() => onMemory()} />
          )}
          <CameraStage stage={view.stage} width={width} name={words.picture(view.name)} layers={layers} />
          <div className="cam-tools" style={{ width: `${width}px` }}>
            <button
              type="button"
              className="cam-btn cam-btn-sm cam-btn-surface"
              disabled={!view.actions.zone || !zone}
              onClick={() => zone && setOpened({ kind: "zone", zone: zone.id })}
            >
              <ZoneIcon />
              {words.drawZone}
            </button>
            <button type="button" className="cam-btn cam-btn-sm cam-btn-surface" disabled={!view.actions.raiseCard} onClick={() => void freeze()}>
              <FlagIcon />
              {words.raiseCard}
            </button>
            <span className="cam-spacer" />
            <span className="cam-seg cam-layers" role="group" aria-label="Layers">
              {(
                [
                  ["boxes", words.boxes],
                  ["masks", words.masks],
                  ["tracks", words.tracks],
                  ["zones", words.zones],
                ] as const
              ).map(([key, label]) => (
                <button
                  type="button"
                  key={key}
                  className={layers[key] ? "cam-seg-on" : ""}
                  aria-pressed={layers[key]}
                  onClick={() => setLayers((now) => ({ ...now, [key]: !now[key] }))}
                >
                  {layers[key] && <CheckIcon />} {label}
                </button>
              ))}
            </span>
          </div>
          {(problem || note) && (
            <p className={problem ? "cam-problem" : "cam-note"} role={problem ? "alert" : "status"} style={{ width: `${width}px` }}>
              {problem || note}
            </p>
          )}
          <Sources sources={view.sources} onSelect={(camera) => onNavigate({ camera })} />
        </div>
        <aside className="cam-panel" aria-label={view.agent.agent.displayName}>
          <AgentHead agent={view.agent} onOpenThread={thread} />
          <section className="cam-sec">
            <h3>{words.decisions}</h3>
            {view.decisions.length === 0 ? (
              <span className="cam-empty">{view.nothingWaiting}</span>
            ) : (
              view.decisions.map((card) => (
                <DecisionMini
                  key={card.id}
                  card={card}
                  onOpen={onOpenDecision}
                  decide={async (command, origin: Origin) => {
                    const outcome = await client.decide(command, origin);
                    void read();
                    return outcome;
                  }}
                />
              ))
            )}
          </section>
          {view.watches.length > 0 && (
            <section className="cam-sec">
              <h3>{words.watchingFor}</h3>
              {view.watches.map((watch) => (
                <Watch
                  key={watch.id}
                  watch={watch}
                  onEditZone={view.actions.zone && zone ? () => setOpened({ kind: "zone", zone: zone.id }) : undefined}
                />
              ))}
            </section>
          )}
          <section className="cam-sec">
            <h3>
              {words.frameBudget}
              <span>{view.budget.scope}</span>
            </h3>
            <Budget budget={view.budget} onLimit={onMemory} />
          </section>
          <section className="cam-sec">
            <h3>{words.tracksNow}</h3>
            {view.tracks.length > 0 && <Tracks tracks={view.tracks} />}
            <span className={view.tracks.length > 0 ? "cam-small cam-muted" : "cam-empty"}>{view.tracksNote}</span>
          </section>
        </aside>
      </div>
      {editingZone && (
        <div className="cam-modal" role="dialog" aria-modal="true" aria-label={words.zoneTitle}>
          <ZoneEditor
            view={view}
            zone={editingZone}
            initialCorners={opened?.kind === "zone" ? opened.corners : undefined}
            onCancel={() => setOpened(undefined)}
            onSave={async (name, corners) => {
              if (!client.saveZone) throw new Error(words.zoneTitle);
              const saved = await client.saveZone(id, editingZone.id, { name, corners, was: editingZone.corners });
              setOpened(undefined);
              setNote(saved.text);
              void read();
              return saved;
            }}
          />
        </div>
      )}
      {opened?.kind === "card" && (
        <div className="cam-modal" role="dialog" aria-modal="true" aria-label={words.cardTitle}>
          <RaiseCard
            frozen={opened.frozen}
            name={view.name}
            agent={view.agent.agent}
            initialNote={opened.note}
            onCancel={() => setOpened(undefined)}
            onRaise={async (text) => {
              const frame = opened.frozen.frame;
              if (!frame) throw new Error(words.noFrame);
              const raised = await client.raiseCard(id, { frame: frame.digest, note: text });
              setOpened(undefined);
              setNote(raised.text);
              void read();
              return raised;
            }}
          />
        </div>
      )}
    </div>
  );
}

/** Every camera as a grid (board choice C8): state pills, a switch list and a budget per source. */
function Grid({
  client,
  onNavigate,
  onOpenThread,
  onOpenDecision,
}: {
  client: CameraClient;
  onNavigate: (place: CameraPlace) => void;
  onOpenThread: (agent: string, session: string) => void;
  onOpenDecision: (card: DecisionCard) => void;
}) {
  const [view, setView] = useState<CamerasView>();
  const [problem, setProblem] = useState("");
  // Two tiles a row, each as wide as its half of the canvas allows.
  const { ref, width } = useStageWidth(STAGE_MAX, view !== undefined);
  const tileWidth = Math.max(220, Math.min(452, Math.floor((width - TILE_GAP) / 2) - 16));
  const read = useCallback(async () => {
    try {
      setView(await client.cameras());
      setProblem("");
    } catch (error) {
      setProblem(problemText(error));
    }
  }, [client]);
  useEffect(() => {
    void read();
    const timer = window.setInterval(() => void read(), READ_AGAIN_MS);
    return () => window.clearInterval(timer);
  }, [read]);

  if (!view) {
    return problem ? (
      <p className="cam-problem cam-page-problem" role="alert">
        {problem}
      </p>
    ) : (
      <div className="cam-loading" aria-busy="true" />
    );
  }
  const agent = view.agent;
  return (
    <div className="cam">
      <div className="cam-subbar">
        {agent && (
          <>
            <button type="button" className="cam-back" onClick={() => onOpenThread(agent.agent.name, agent.sessionId)}>
              <ChevronIcon />
              {agent.agent.displayName}
            </button>
            <span className="cam-rule" aria-hidden="true" />
          </>
        )}
        <h1 className="cam-title">{view.title}</h1>
        <span className="cam-meta">{view.countText}</span>
        <span className="cam-spacer" />
        <button type="button" className="cam-btn cam-btn-sm" disabled title="Cameras are added by the agent that watches them">
          {words.addCamera}
        </button>
      </div>
      <div className="cam-body">
        <div className="cam-canvas cam-canvas-grid" ref={ref}>
          <div className="cam-grid">
            {view.tiles.map((tile) => (
              <button
                type="button"
                key={tile.id}
                className={`cam-tile${tile.id === view.selected ? " cam-tile-selected" : ""}`}
                onClick={() => onNavigate({ camera: tile.id })}
              >
                <CameraStage stage={tile.stage} width={tileWidth} name={tile.name} size="small" />
                <span className="cam-tile-foot">
                  <b>{tile.name}</b>
                  <StatePill tone={tile.tone} state={tile.state} small>
                    {tile.stateText}
                  </StatePill>
                  <span className="cam-spacer" />
                  <span className="cam-small cam-muted">{tile.caption}</span>
                </span>
              </button>
            ))}
          </div>
          <span className="cam-grid-foot">{view.footer}</span>
          {problem && (
            <p className="cam-problem" role="alert">
              {problem}
            </p>
          )}
        </div>
        <aside className="cam-panel" aria-label={view.title}>
          {agent && <AgentHead agent={agent} />}
          <section className="cam-sec">
            <h3>{words.sources}</h3>
            <div className="cam-switches">
              {view.tiles.map((tile) => (
                <div className="cam-switch-row" key={tile.id}>
                  <span className="cam-switch-words">
                    <b>{tile.name}</b>
                    <span className="cam-small">
                      <span className={`cam-tone-${tile.tone}`}>{tile.stateText}</span>
                      <span className="cam-muted"> · {tile.line}</span>
                    </span>
                  </span>
                  <Switch
                    on={tile.on}
                    short
                    disabled={!tile.canSwitch}
                    label={words.cameraSwitch(tile.name)}
                    onSwitch={(on) =>
                      void client
                        .switchTo(tile.id, on)
                        .then(() => read())
                        .catch((error: unknown) => setProblem(problemText(error)))
                    }
                  />
                </div>
              ))}
            </div>
            <span className="cam-small cam-muted">{view.note}</span>
          </section>
          <section className="cam-sec">
            <h3>{words.decisions}</h3>
            {view.decisions.length === 0 ? (
              <span className="cam-empty">{view.nothingWaiting}</span>
            ) : (
              view.decisions.map((card) => (
                <DecisionMini
                  key={card.id}
                  card={card}
                  onOpen={onOpenDecision}
                  decide={async (command, origin) => {
                    const outcome = await client.decide(command, origin);
                    void read();
                    return outcome;
                  }}
                />
              ))
            )}
          </section>
          <section className="cam-sec">
            <h3>{words.frameBudget}</h3>
            {view.budget.map((meter) => (
              <div className="cam-meter" key={meter.label}>
                <span className="cam-meter-top">
                  <span>{meter.label}</span>
                  <b>{meter.value}</b>
                </span>
                <div className="cam-bar" role="meter" aria-label={meter.label} aria-valuenow={meter.percent} aria-valuemin={0} aria-valuemax={100}>
                  <i className={`cam-bar-${meter.tone}`} style={{ width: `${meter.percent}%` }} />
                </div>
                {meter.note && <span className="cam-meter-note">{meter.note}</span>}
              </div>
            ))}
          </section>
        </aside>
      </div>
    </div>
  );
}
