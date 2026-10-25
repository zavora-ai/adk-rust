import { useEffect, useMemo, useRef, useState } from "react";
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
import type { VoiceDevices } from "../voice/devices";
import { browserDevices } from "../voice/devices";
import { allowingIn, heard, voiceCantApprove, voiceLabels, wantsTo } from "../voice/labels";
import { VoiceSession, initialState } from "../voice/session";
import type { VoiceState } from "../voice/session";
import "../voice/voice.css";

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
 * answer would apply to, a spoken "allow once" counting down with Cancel, the decisions voice
 * cannot approve with "Decide on screen", and what the model can see while its switches are on.
 * Everything a model, a tool or a person said is rendered as text (S-I9).
 */
export function Voice({
  client,
  devices: given,
  onEnd,
}: {
  client: VoiceClient;
  devices?: VoiceDevices;
  onEnd: () => void;
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
    onEnd();
  }

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
        <button type="button" className="button button-stop voice-end" onClick={end}>
          {voiceLabels.end}
        </button>
      </header>
      <main className="voice-main" id="content">
        <section className="voice-talk" aria-live="polite">
          <h1 className="voice-state">
            <Waveform />
            {state.phase === "speaking"
              ? voiceLabels.speaking
              : state.phase === "connecting"
                ? voiceLabels.connecting
                : state.phase === "ended"
                  ? voiceLabels.ended
                  : voiceLabels.listening}
          </h1>
          {stopped ? (
            <p className="voice-prompt">{voiceLabels.stopped}</p>
          ) : (
            prompted && <Prompt card={prompted} />
          )}
          {state.heard && <p className="voice-said">{state.heard}</p>}
          {state.pending && (
            <article className="voice-pending" aria-label={pending?.heading ?? state.pending.text}>
              <div className="voice-pending-top">
                <span className="voice-heard">{heard(state.heard ?? "")}</span>
                <span className="voice-pill">
                  <span className="voice-pill-dot" aria-hidden="true" />
                  {allowingIn(seconds)}
                </span>
              </div>
              <h2 className="voice-pending-heading">{pending?.heading}</h2>
              <div className="voice-pending-bottom">
                <span className="voice-pending-meta">
                  {pending ? `${pending.agent.displayName} · ${pending.riskText}` : ""}
                </span>
                <button type="button" className="button voice-cancel" onClick={() => void cancel(state.pending!.id)}>
                  {voiceLabels.cancel}
                </button>
              </div>
            </article>
          )}
          {state.outcome?.text && !state.pending && <p className="voice-outcome">{state.outcome.text}</p>}
          {!stopped &&
            onScreen.map((high) => (
              <p className="voice-screen" key={high.id}>
                {voiceCantApprove(high.heading, high.money)}{" "}
                <a href={decisionPath(high.id)}>{voiceLabels.decideOnScreen}</a>
              </p>
            ))}
          {(problem || state.problem) && (
            <p className="banner" role="alert">
              {state.problem ?? problem}
            </p>
          )}
          {state.phase === "ended" && (
            <button type="button" className="button" onClick={() => setAttempt((count) => count + 1)}>
              {voiceLabels.startAgain}
            </button>
          )}
        </section>
        <aside className="voice-side">
          <section className="voice-see">
            <h2>{voiceLabels.whatItCanSee}</h2>
            <Preview stream={state.preview} />
            <div className="voice-switches" role="group" aria-label={voiceLabels.inputs}>
              {(["mic", "camera", "screen"] as InputSource[]).map((source) => {
                const on = state.inputs[source];
                const label = voiceLabels[source](on);
                return (
                  <button
                    key={source}
                    type="button"
                    className={`voice-switch${on ? " voice-switch-on" : ""}`}
                    aria-pressed={on}
                    disabled={state.phase === "connecting" || state.phase === "ended"}
                    onClick={() => void session.current?.switch(source, !on)}
                  >
                    {label}
                  </button>
                );
              })}
            </div>
          </section>
          <p className="voice-note">{voiceLabels.privacyNote}</p>
        </aside>
      </main>
    </div>
  );
}

/**
 * What a spoken answer would apply to now, and the words that answer it. A call voice cannot
 * approve says so, and where to decide it.
 */
function Prompt({ card }: { card: DecisionCard }) {
  if (card.allowed.options.some((option) => option.answer === "allowOnce")) {
    return (
      <p className="voice-prompt">
        {`${wantsTo(card.agent.displayName, card.heading)} ${voiceLabels.sayAllowOnceOrDeny}`}
      </p>
    );
  }
  const [before, after] = voiceLabels.sayDenyOrDecideOnScreen.split(DECIDE);
  return (
    <p className="voice-prompt">
      {`${voiceCantApprove(card.heading, card.money)} ${before}`}
      <a href={decisionPath(card.id)}>{DECIDE}</a>
      {after}
    </p>
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

function Waveform() {
  const bars = [10, 22, 30, 16, 26, 12, 20];
  return (
    <svg className="voice-wave" viewBox="0 0 50 36" aria-hidden="true">
      {bars.map((height, index) => (
        <rect key={index} x={index * 7 + 1} y={(36 - height) / 2} width="4" height={height} rx="2" />
      ))}
    </svg>
  );
}
