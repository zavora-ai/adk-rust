import { useCallback, useEffect, useState } from "react";
import type { CameraView, CamerasView, DecisionCard, StageView } from "../generated/api";
import { problemText } from "../client";
import { CameraStage } from "./CameraStage";
import type { CameraClient } from "./client";
import { CameraOffIcon, ChevronIcon, FlagIcon, StopIcon, ZoneIcon } from "./icons";
import { cameraLabels as words } from "./labels";
import { BudgetRows, DecisionMini, Notice, StatePill, Swatch, Switch, Tracks } from "./parts";
import { RaiseCard } from "./RaiseCard";
import type { CameraPlace } from "./route";
import { cameraPath } from "./route";
import "./camera.css";

const STAGE_WIDTH = 358;
const READ_AGAIN_MS = 3_000;

/**
 * The live camera surface on a phone (spec 3.7 §8.2, board choice C14): the stream on top, "On
 * / Off" and "Raise a card"; Decisions, Watching for, the budget rows and the tracks; "Edit the
 * zone", which opens the zone in the web home, and "Stop the stream" below. `/ui/cameras` lists
 * every camera with its switch. Drawn to the signed-off boards `camera-*-phone`.
 */
export function PhoneCamera({
  client,
  place,
  onBack,
  onNavigate,
  onOpenDecision,
  initialCard,
}: {
  client: CameraClient;
  place: CameraPlace;
  onBack: () => void;
  onNavigate: (place: CameraPlace) => void;
  onOpenDecision: (card: DecisionCard) => void;
  /** A card being raised on arrival (fixtures). */
  initialCard?: StageView;
}) {
  return place.camera ? (
    <One key={place.camera} client={client} id={place.camera} onBack={onBack} onOpenDecision={onOpenDecision} initialCard={initialCard} />
  ) : (
    <Several client={client} onBack={onBack} onNavigate={onNavigate} />
  );
}

function useRead<T>(read: () => Promise<T>) {
  const [value, setValue] = useState<T>();
  const [problem, setProblem] = useState("");
  const again = useCallback(async () => {
    try {
      setValue(await read());
      setProblem("");
    } catch (error) {
      setProblem(problemText(error));
    }
  }, [read]);
  useEffect(() => {
    void again();
    const timer = window.setInterval(() => void again(), READ_AGAIN_MS);
    return () => window.clearInterval(timer);
  }, [again]);
  return { value, setValue, problem, setProblem, again };
}

function One({
  client,
  id,
  onBack,
  onOpenDecision,
  initialCard,
}: {
  client: CameraClient;
  id: string;
  onBack: () => void;
  onOpenDecision: (card: DecisionCard) => void;
  initialCard?: StageView;
}) {
  const read = useCallback(() => client.camera(id), [client, id]);
  const { value: view, setValue: setView, problem, setProblem, again } = useRead<CameraView>(read);
  const [frozen, setFrozen] = useState<StageView | undefined>(initialCard);
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!client.feed) return;
    const abort = new AbortController();
    void (async () => {
      try {
        for await (const message of client.feed!(id, abort.signal)) {
          if (message.type === "frame") setView((now) => (now ? { ...now, stage: message.stage } : now));
          else void again();
        }
      } catch {
        // Read again every few seconds whatever happens to the feed.
      }
    })();
    return () => abort.abort();
  }, [client, id, again, setView]);

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

  if (!view) {
    return (
      <main className="phone-main cam-phone" aria-busy={!problem}>
        {problem && (
          <p className="phone-problem" role="alert">
            {problem}
          </p>
        )}
      </main>
    );
  }

  if (frozen) {
    return (
      <main className="phone-main cam-phone cam-phone-card">
        <RaiseCard
          frozen={frozen}
          name={view.name}
          agent={view.agent.agent}
          width={STAGE_WIDTH}
          onCancel={() => setFrozen(undefined)}
          onRaise={async (text) => {
            const frame = frozen.frame;
            if (!frame) throw new Error(words.noFrame);
            const raised = await client.raiseCard(id, { frame: frame.digest, note: text });
            setFrozen(undefined);
            setNote(raised.text);
            void again();
            return raised;
          }}
        />
      </main>
    );
  }

  const watching = view.watches[0];
  const zone = view.zones[0];
  return (
    <main className="phone-main cam-phone">
      <header className="cam-phone-head">
        <button type="button" className="cam-phone-back" onClick={onBack}>
          <ChevronIcon />
          {view.agent.agent.displayName}
        </button>
        <span className="cam-phone-title">
          <h1>{view.name}</h1>
          <StatePill tone={view.tone} state={view.state} small>
            {view.stateText}
          </StatePill>
        </span>
      </header>
      <div className="cam-phone-pad">
        <CameraStage stage={view.stage} width={STAGE_WIDTH} name={words.picture(view.name)} size="small" />
      </div>
      <div className="cam-phone-controls">
        <Switch
          on={view.on}
          short
          disabled={busy || !view.actions.switch}
          label={words.cameraSwitch(view.name)}
          onSwitch={(on) => void act(() => client.switchTo(id, on))}
        />
        <button
          type="button"
          className="cam-btn cam-btn-sm cam-btn-surface"
          disabled={!view.actions.raiseCard}
          onClick={() => {
            const frame = view.stage.frame;
            if (!frame) return;
            client
              .freeze(id, frame.digest)
              .then(setFrozen)
              .catch((error: unknown) => setProblem(problemText(error)));
          }}
        >
          <FlagIcon />
          {words.raiseCardShort}
        </button>
      </div>
      {view.notice && (
        <div className="cam-phone-pad">
          <Notice notice={view.notice} short onRetry={() => void act(() => client.retry(id))} />
        </div>
      )}
      <div className="cam-phone-body">
        {(problem || note) && (
          <p className={problem ? "phone-problem" : "cam-note"} role={problem ? "alert" : "status"}>
            {problem || note}
          </p>
        )}
        {view.decisions.map((card) => (
          <DecisionMini key={card.id} card={card} onOpen={onOpenDecision} short />
        ))}
        {watching && (
          <div className="cam-phone-group">
            <span className="cam-label">{words.watchingFor}</span>
            <span className={`cam-phone-watch${watching.active ? "" : " cam-watch-idle"}`}>
              <Swatch label={watching.label} faded={!watching.active} />
              <span>
                {watching.shortBefore} {watching.zone && <b>{watching.zone}</b>}
                {watching.shortAfter}
              </span>
            </span>
          </div>
        )}
        <div className="cam-phone-group">
          <span className="cam-label">{words.frameBudget}</span>
          <BudgetRows budget={view.budget} />
        </div>
        {view.tracks.length > 0 && (
          <div className="cam-phone-group">
            <span className="cam-label">{words.tracksNow}</span>
            <Tracks tracks={view.tracks} short limit={3} />
          </div>
        )}
        {view.tracksShortNote && <span className="cam-small cam-muted">{view.tracksShortNote}</span>}
        <div className="cam-phone-more">
          {zone && view.actions.zone ? (
            <a className="cam-btn cam-btn-sm cam-btn-surface" href={cameraPath({ camera: id, zone: true })}>
              <ZoneIcon />
              {words.editZone}
            </a>
          ) : (
            <button type="button" className="cam-btn cam-btn-sm cam-btn-surface" disabled>
              <ZoneIcon />
              {words.editZone}
            </button>
          )}
          <button type="button" className="cam-btn cam-btn-sm cam-btn-surface" disabled={busy || !view.actions.stop} onClick={() => void act(() => client.stop(id))}>
            <StopIcon />
            {words.stopStream}
          </button>
        </div>
      </div>
    </main>
  );
}

function Several({
  client,
  onBack,
  onNavigate,
}: {
  client: CameraClient;
  onBack: () => void;
  onNavigate: (place: CameraPlace) => void;
}) {
  const read = useCallback(() => client.cameras(), [client]);
  const { value: view, problem, setProblem, again } = useRead<CamerasView>(read);
  if (!view) {
    return (
      <main className="phone-main cam-phone" aria-busy={!problem}>
        {problem && (
          <p className="phone-problem" role="alert">
            {problem}
          </p>
        )}
      </main>
    );
  }
  const first = view.tiles.find((tile) => tile.id === view.selected) ?? view.tiles[0];
  return (
    <main className="phone-main cam-phone">
      <header className="cam-phone-head">
        {view.agent && (
          <button type="button" className="cam-phone-back" onClick={onBack}>
            <ChevronIcon />
            {view.agent.agent.displayName}
          </button>
        )}
        <span className="cam-phone-title">
          <h1>{view.title}</h1>
          <span className="cam-muted">{view.countShort}</span>
        </span>
      </header>
      {first && (
        <>
          <div className="cam-phone-pad">
            <button type="button" className="cam-plain" onClick={() => onNavigate({ camera: first.id })}>
              <CameraStage stage={first.stage} width={STAGE_WIDTH} name={first.name} size="small" />
            </button>
          </div>
          {view.selectedLine && (
            <span className="cam-phone-line">
              {first.name} <span className={`cam-tone-${first.tone}`}>{view.selectedLine.slice(first.name.length)}</span>
            </span>
          )}
        </>
      )}
      <div className="cam-phone-list">
        {view.tiles.map((tile) => (
          <div className="cam-phone-row" key={tile.id}>
            <button type="button" className="cam-plain cam-phone-thumb" onClick={() => onNavigate({ camera: tile.id })} aria-label={tile.name}>
              {tile.stage.look === "dark" ? (
                <span className="cam-thumb-off">
                  <CameraOffIcon size={16} color="#94A3B8" />
                </span>
              ) : (
                <CameraStage stage={{ ...tile.stage, badge: undefined }} width={96} name={tile.name} size="strip" tags={false} />
              )}
            </button>
            <span className="cam-switch-words">
              <b>{tile.name}</b>
              <span className="cam-small">
                <span className={`cam-tone-${tile.tone}`}>{tile.stateText}</span>
                {tile.shortLine && <span className="cam-muted"> · {tile.shortLine}</span>}
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
                  .then(() => again())
                  .catch((error: unknown) => setProblem(problemText(error)))
              }
            />
          </div>
        ))}
      </div>
      <span className="cam-phone-note">{view.shortNote}</span>
      {problem && (
        <p className="phone-problem" role="alert">
          {problem}
        </p>
      )}
    </main>
  );
}
