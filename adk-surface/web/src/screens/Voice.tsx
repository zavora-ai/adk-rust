import { useEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties } from "react";
import type {
  Command,
  CommandOutcome,
  DecisionCard,
  InputSource,
  Origin,
  StatusLine,
  VoiceModels,
  VoiceTicket,
} from "../generated/api";
import { problemText } from "../client";
import { decisionPath } from "../route";
import { visible } from "../safe";
import type { VoiceDevices } from "../voice/devices";
import { browserDevices } from "../voice/devices";
import { allowingIn, heard, voiceCantApprove, voiceLabels, wantsTo } from "../voice/labels";
import { VoiceSession, initialState } from "../voice/session";
import type { VoiceState } from "../voice/session";
import "../voice/voice.css";
import { verbLabels } from "../verbs/labels";
import { VoiceAction } from "../verbs/VoiceAction";

/** What the voice view reads and does (spec 3.3 §5.6). */
export interface VoiceClient {
  voiceModels(): Promise<VoiceModels | undefined>;
  voiceTicket(model: string, sessionId?: string): Promise<VoiceTicket>;
  /** The open decisions, with the answers voice may give them. */
  voiceDecisions(): Promise<DecisionCard[]>;
  /** A command given on the voice view: surface voice. */
  voiceCommand(command: Command, origin: Origin): Promise<CommandOutcome>;
  status(): Promise<StatusLine>;
}

/** How often the view reads the open decisions and the status line. */
const READ_EVERY_MS = 1000;


/**
 * The voice view (spec 3.3 §5.6): hands-free turns with the agent, the decision a spoken
 * answer would apply to with its exact call in full (§7.1), a spoken "allow once" counting
 * down with Cancel, the decisions voice cannot approve with "Decide on screen", and what the
 * model can see while its switches are on. Everything a model, a tool or a person said is
 * rendered as text (S-I9). End gives back the conversation's session, which then opens in
 * Threads.
 */
export function Voice({
  client,
  devices: given,
  onEnd,
  screenActions = false,
}: {
  client: VoiceClient;
  devices?: VoiceDevices;
  onEnd: (sessionId?: string) => void;
  /** The home takes screen actions by voice: "snooze news digest" (spec 3.3 §5.6). */
  screenActions?: boolean;
}) {
  const devices = useMemo(() => given ?? browserDevices(), [given]);
  const [models, setModels] = useState<VoiceModels>();
  const [model, setModel] = useState<string>();
  const [state, setState] = useState<VoiceState>(initialState);
  const [cards, setCards] = useState<DecisionCard[]>([]);
  const [status, setStatus] = useState<StatusLine>();
  const [problem, setProblem] = useState("");
  const [now, setNow] = useState(() => Date.now());
  const [attempt, setAttempt] = useState(0);
  const session = useRef<VoiceSession | undefined>(undefined);
  const conversation = useRef<string | undefined>(undefined);
  const seen = useRef<Set<string> | undefined>(undefined);
  conversation.current = state.sessionId ?? conversation.current;

  useEffect(() => {
    client.voiceModels().then(
      (offered) => {
        setModels(offered);
        setModel((current) => current ?? offered?.models[0]?.id);
      },
      (error: unknown) => setProblem(problemText(error)),
    );
  }, [client]);

  // One conversation per model; another model continues the same voice session.
  useEffect(() => {
    if (!model) return;
    const voice = new VoiceSession((id, sessionId) => client.voiceTicket(id, sessionId), devices, setState);
    session.current = voice;
    void voice.start(model, conversation.current);
    return () => voice.end();
  }, [client, devices, model, attempt]);

  useEffect(() => {
    let live = true;
    const read = async () => {
      try {
        const [open, line] = await Promise.all([client.voiceDecisions(), client.status()]);
        if (!live) return;
        // Another agent's decision is shown with a chime here; the model is never told.
        const known = seen.current;
        if (known && open.some((card) => !known.has(card.id) && card.sessionId !== conversation.current)) {
          devices.chime();
        }
        seen.current = new Set(open.map((card) => card.id));
        setCards(open);
        setStatus(line);
      } catch (error) {
        if (live) setProblem(problemText(error));
      }
    };
    void read();
    const timer = window.setInterval(() => void read(), READ_EVERY_MS);
    return () => {
      live = false;
      window.clearInterval(timer);
    };
  }, [client, devices]);

  // The countdown of a spoken "allow once".
  useEffect(() => {
    if (!state.pending) return;
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), 250);
    return () => window.clearInterval(timer);
  }, [state.pending]);

  const card = (id: string | undefined) => cards.find((candidate) => candidate.id === id);
  const prompted = card(state.prompted?.id);
  const pending = state.pending && card(state.pending.id);
  const stopped = status?.stopped === true;
  const onScreen = state.needsScreen.filter((id) => id !== state.prompted?.id).flatMap((id) => card(id) ?? []);
  const seconds = state.pending ? Math.max(0, Math.ceil((state.pending.cancelUntil - now) / 1000)) : 0;
  const agent = models?.agent ?? "";

  async function cancel(id: string) {
    setProblem("");
    try {
      await client.voiceCommand({ type: "cancelAnswer", decisionId: id }, { kind: "pointer" });
    } catch (error) {
      setProblem(problemText(error));
    }
  }

  function end() {
    session.current?.end();
    onEnd(conversation.current);
  }

  const live = state.phase === "listening" || state.phase === "speaking";
  const sharing = state.inputs.screen ? "screen" : state.inputs.camera ? "camera" : undefined;
  const captions = state.captions;
  return (
    <div className="voice">
      <header className="voice-header">
        <span className="voice-brand">
          <span className="voice-dot" aria-hidden="true" />
          {voiceLabels.brand}
        </span>
        <span className="voice-with">
          {voiceLabels.talkingWith} <strong>{agent}</strong>
        </span>
        <span className={`voice-link voice-link-${live ? "on" : state.phase}`}>
          <span className="voice-link-dot" aria-hidden="true" />
          {live ? voiceLabels.connected : state.phase === "connecting" ? voiceLabels.connecting : voiceLabels.endedShort}
        </span>
        <span className="voice-spacer" />
        <label className="voice-model">
          <span>{voiceLabels.voiceModel}</span>
          <select value={model ?? ""} onChange={(event) => setModel(event.target.value)}>
            {models?.models.map((offered) => (
              <option key={offered.id} value={offered.id}>
                {offered.name}
              </option>
            ))}
          </select>
        </label>
      </header>
      <main className="voice-main" id="content">
        <section className="voice-talk" aria-live="polite">
          <Presence phase={state.phase} session={session.current} />
          <h1 className="voice-state">
            {state.phase === "speaking"
              ? voiceLabels.speaking
              : state.phase === "connecting"
                ? voiceLabels.connecting
                : state.phase === "ended"
                  ? voiceLabels.ended
                  : voiceLabels.listening}
          </h1>
          <p className="voice-hint">
            {state.phase === "speaking" ? voiceLabels.speakingHint : state.phase === "listening" ? voiceLabels.listeningHint : ""}
          </p>
          {captions.length > 0 && (
            <ol className="voice-captions" aria-label={voiceLabels.captions}>
              {captions.map((line, index) => (
                <li key={`${line.at}-${index}`} className={index === captions.length - 1 ? "voice-caption-latest" : undefined}>
                  <time className="voice-caption-time" dateTime={new Date(line.at).toISOString()}>
                    {clock(line.at)}
                  </time>
                  {line.kind === "heard" ? (
                    <span>
                      <strong>{voiceLabels.you}</strong> · <span>{line.text}</span>
                    </span>
                  ) : (
                    <span className="voice-caption-note">{voiceLabels.spokeOver}</span>
                  )}
                </li>
              ))}
            </ol>
          )}
          {stopped ? (
            <p className="voice-prompt">{voiceLabels.stopped}</p>
          ) : (
            prompted && <Prompt card={prompted} />
          )}
          {state.pending && (
            <article className="voice-pending" aria-label={pending?.heading ?? state.pending.text}>
              <div className="voice-pending-top">
                <Countdown until={state.pending.cancelUntil} now={now} id={state.pending.id} />
                <div className="voice-pending-text">
                  <span className="voice-pill">{allowingIn(seconds)}</span>
                  <h2 className="voice-pending-heading">{pending?.heading}</h2>
                  <span className="voice-heard">{heard(state.heard ?? "")}</span>
                </div>
                <button type="button" className="button button-secondary voice-cancel" onClick={() => void cancel(state.pending!.id)}>
                  {voiceLabels.cancel}
                </button>
              </div>
              {pending && <Call card={pending} />}
              <span className="voice-pending-meta">{pending ? `${pending.agent.displayName} · ${pending.riskText}` : ""}</span>
            </article>
          )}
          {state.outcome?.text && !state.pending && <p className="voice-outcome">{state.outcome.text}</p>}
          {state.actions.map((action) => (
            <VoiceAction
              key={action.kind === "pending" ? action.actionId : `ended-${action.actionId ?? ""}`}
              action={action}
              onCancel={(id) => session.current?.cancelAction(id)}
            />
          ))}
          {!stopped &&
            onScreen.map((high) => (
              <p className="voice-screen" key={high.id}>
                {cantApprove(high)}{" "}
                <a href={decisionPath(high.id)}>{voiceLabels.decideOnScreen}</a>
              </p>
            ))}
          {(problem || state.problem) && (
            <p className="banner" role="alert">
              {state.problem ?? problem}
            </p>
          )}
          {state.phase === "ended" && (
            <button type="button" className="button button-secondary" onClick={() => setAttempt((count) => count + 1)}>
              {voiceLabels.startAgain}
            </button>
          )}
        </section>
        <aside className="voice-side">
          <section className="voice-see" aria-labelledby="voice-see-title">
            <div className="voice-see-head">
              <h2 id="voice-see-title">{voiceLabels.whatItCanSee}</h2>
              {sharing && (
                <span className="voice-live">
                  <span className="voice-live-dot" aria-hidden="true" />
                  {voiceLabels.sharingLive(sharing)}
                </span>
              )}
            </div>
            <Preview stream={state.preview} />
            {sharing && state.sent.length > 0 && (
              <div className="voice-sent">
                <div className="voice-sent-head">
                  <h3>{voiceLabels.sentToModel}</h3>
                  <span>{voiceLabels.lastFrames(state.sent.length)}</span>
                </div>
                <ol aria-label={voiceLabels.sentToModel}>
                  {state.sent.map((frame) => (
                    <li key={frame.url}>
                      <img src={frame.url} alt={voiceLabels.frameAlt(frame.source, clock(frame.at, true))} />
                      <time dateTime={new Date(frame.at).toISOString()}>{clock(frame.at, true)}</time>
                    </li>
                  ))}
                </ol>
              </div>
            )}
          </section>
          <p className="voice-note">{screenActions ? verbLabels.voicePrivacyNote : voiceLabels.privacyNote}</p>
        </aside>
      </main>
      <nav className="voice-dock" aria-label={voiceLabels.inputs}>
        <div className="voice-dock-bar">
          {(["mic", "camera", "screen"] as InputSource[]).map((source) => {
            const on = state.inputs[source];
            return (
              <button
                key={source}
                type="button"
                className={`voice-switch${on ? " voice-switch-on" : ""}`}
                aria-pressed={on}
                disabled={state.phase === "connecting" || state.phase === "ended"}
                onClick={() => void session.current?.switch(source, !on)}
              >
                <SourceIcon source={source} on={on} />
                <span className="voice-switch-text">{voiceLabels[source](on)}</span>
                {source === "mic" && on && <span className="voice-meter" aria-hidden="true"><i /><i /><i /></span>}
              </button>
            );
          })}
          <span className="voice-dock-rule" aria-hidden="true" />
          <button type="button" className="button button-stop voice-end" onClick={end}>
            {voiceLabels.end}
          </button>
        </div>
      </nav>
    </div>
  );
}

/** A time for captions and frames: 10:04, or 10:04:14 to the second. */
function clock(at: number, seconds = false): string {
  return new Date(at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", ...(seconds ? { second: "2-digit" } : {}), hour12: false });
}

/**
 * The conversation's presence: rings that follow the person's voice while it listens, bars that
 * follow the model's voice while it speaks. The loudness arrives outside React's renders and
 * moves CSS variables, so the page does not re-render with the audio.
 */
function Presence({ phase, session }: { phase: VoiceState["phase"]; session?: VoiceSession }) {
  const orb = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!session) return;
    // On the view itself, so the dock's mic meter follows the same voice.
    session.onLevels = (levels) => {
      const host = orb.current?.closest<HTMLElement>(".voice") ?? orb.current;
      host?.style.setProperty("--in", levels.input.toFixed(3));
      host?.style.setProperty("--out", levels.output.toFixed(3));
    };
    return () => {
      if (session.onLevels) session.onLevels = undefined;
    };
  }, [session]);
  return (
    <div ref={orb} className="voice-orb" data-phase={phase} aria-hidden="true">
      <span className="voice-orb-glow" />
      <span className="voice-orb-ring" />
      <span className="voice-orb-ring voice-orb-ring-late" />
      <span className="voice-orb-core">
        {[0.35, 0.6, 0.85, 1, 0.85, 0.6, 0.35].map((k, index) => (
          <i key={index} style={{ "--k": k, "--d": `${index * 90}ms` } as CSSProperties} />
        ))}
      </span>
    </div>
  );
}

/** The ring of a spoken "allow once" running out, from when it was heard. */
function Countdown({ until, now, id }: { until: number; now: number; id: string }) {
  const [start, setStart] = useState(() => ({ id, at: Date.now() }));
  if (start.id !== id) setStart({ id, at: Date.now() });
  const span = Math.max(1, until - start.at);
  const left = Math.min(1, Math.max(0, (until - now) / span));
  const circumference = 2 * Math.PI * 18;
  return (
    <svg className="voice-count" viewBox="0 0 40 40" aria-hidden="true">
      <circle cx="20" cy="20" r="18" className="voice-count-track" />
      <circle cx="20" cy="20" r="18" className="voice-count-left" strokeDasharray={circumference} strokeDashoffset={circumference * (1 - left)} />
    </svg>
  );
}

function SourceIcon({ source, on }: { source: InputSource; on: boolean }) {
  const path = {
    mic: "M12 3a3 3 0 0 0-3 3v6a3 3 0 0 0 6 0V6a3 3 0 0 0-3-3zM5 11a7 7 0 0 0 14 0M12 18v3",
    camera: "M3 7h4l2-2h6l2 2h4v12H3zM12 17a4 4 0 1 0 0-8 4 4 0 0 0 0 8z",
    screen: "M3 4h18v12H3zM8 20h8M12 16v4",
  }[source];
  return (
    <svg className="icon" viewBox="0 0 24 24" aria-hidden="true">
      <path d={path} />
      {!on && <path d="M4 4l16 16" />}
    </svg>
  );
}

/** Whether voice may allow `card` once: an everyday call shown in full. */
function voiceAllows(card: DecisionCard): boolean {
  return card.allowed.options.some((option) => option.answer === "allowOnce" && !option.needsExpanded);
}

/** Why voice cannot allow `card` once: high risk, money, or a call shown cut short. */
function cantApprove(card: DecisionCard): string {
  return card.money || card.risk === "high"
    ? voiceCantApprove(card.heading, card.money)
    : wantsTo(card.agent.displayName, card.heading);
}

/**
 * What a spoken answer would apply to now, the words that answer it, and the exact call in
 * full (§7.1). A call voice cannot approve says so, and where to decide it.
 */
function Prompt({ card }: { card: DecisionCard }) {
  if (voiceAllows(card)) {
    return (
      <>
        <p className="voice-prompt">
          {`${wantsTo(card.agent.displayName, card.heading)} ${voiceLabels.sayAllowOnceOrDeny}`}
        </p>
        <Call card={card} />
      </>
    );
  }
  const [before, after] = voiceLabels.sayDenyOrDecideOnScreen.split(DECIDE);
  return (
    <>
      <p className="voice-prompt">
        {`${cantApprove(card)} ${before}`}
        <a href={decisionPath(card.id)}>{DECIDE}</a>
        {after}
      </p>
      <Call card={card} />
    </>
  );
}

/** The exact call, in full, and whether it can be taken back (§7.1). */
function Call({ card }: { card: DecisionCard }) {
  return (
    <div className="voice-call-block">
      <code className="voice-call" aria-label={voiceLabels.exactCall}>
        {visible(card.call.text)}
      </code>
      <p className="voice-call-details">{card.reversibilityText}</p>
    </div>
  );
}

/** The words of "Say "deny", or decide on screen." that link to the decision. */
const DECIDE = "decide on screen";

function Preview({ stream }: { stream?: MediaStream }) {
  const video = useRef<HTMLVideoElement>(null);
  useEffect(() => {
    if (!video.current) return;
    video.current.srcObject = stream ?? null;
    if (stream) void video.current.play?.()?.catch(() => undefined);
  }, [stream]);
  return (
    <div className="voice-preview">
      {stream ? (
        <video ref={video} muted playsInline aria-label={voiceLabels.whatItCanSee} />
      ) : (
        <span className="voice-preview-empty">{voiceLabels.nothingShared}</span>
      )}
    </div>
  );
}
