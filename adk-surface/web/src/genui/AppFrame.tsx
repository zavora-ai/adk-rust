import { useEffect, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";
import type { AppOpen, AppPrompt, AppView } from "../generated/api";
import { OPENING_THE_APP } from "../generated/words";
import { visible } from "../safe";
import type { AppsSeam } from "./apps";
import { AppBridge, frameable, openable, pageTheme } from "./apps";
import "./genui.css";

/** The frame's height before the app says how tall it is, in CSS pixels. */
const FIRST_HEIGHT = 220;

type Shown = Extract<AppOpen, { state: "shown" }>;

/**
 * An app an MCP server's tool showed (spec 3.3 §6.1 `genui-apps`): on the normal card
 * background under "App from Weather · it can't approve anything", framed sandboxed on the
 * apps origin — scripts only, no same-origin access, no forms, popups or navigation of the
 * page — at the app's height up to a cap. What the app asks for shows under it, outside the
 * frame, and only the person's press does it: a tool call becomes the ordinary decision card
 * below, a message waits for Send it, a link for Open. Where it cannot be framed (no apps
 * origin, another device, the phone), it is the tool's text result.
 */
export function AppFrame({
  app,
  seam,
  session,
  below,
}: {
  app: AppView;
  /** The home's app routes; absent where the server was built without `genui-apps`. */
  seam?: AppsSeam;
  /** The session the app is in. */
  session?: string;
  /** What follows the app: the decisions of the calls it asked for. */
  below?: ReactNode;
}) {
  const elsewhere: AppOpen = useMemo(
    () => ({ state: "elsewhere", text: app.elsewhereText, resultText: app.resultText }),
    [app.elsewhereText, app.resultText],
  );
  const [open, setOpen] = useState<AppOpen | undefined>(seam && session ? undefined : elsewhere);
  // Each prompt keeps its own key, so one sent or dismissed never hands its state to the next.
  const [prompts, setPrompts] = useState<Array<{ key: number; prompt: AppPrompt }>>([]);
  const nextKey = useRef(0);
  useEffect(() => {
    if (!seam || !session) {
      setOpen(elsewhere);
      return;
    }
    let live = true;
    seam
      .open(session, app.callId)
      .then((opened) => {
        if (!live) return;
        const unframeable =
          opened?.state === "shown" && !frameable(opened.frameUrl, opened.frameOrigin, window.location.hostname);
        setOpen(opened && !unframeable ? opened : elsewhere);
      })
      .catch(() => {
        if (live) setOpen(elsewhere);
      });
    return () => {
      live = false;
    };
  }, [seam, session, app.callId, elsewhere]);

  const prompt = (next: AppPrompt) => {
    const key = nextKey.current++;
    setPrompts((all) => [
      ...all.filter(({ prompt: known }) => known.kind !== "notice" || known.text !== next.text),
      { key, prompt: next },
    ]);
  };
  const dismiss = (gone: number) => setPrompts((all) => all.filter(({ key }) => key !== gone));

  return (
    <div className="drawn-place app-place">
      <section className="drawn drawn-app" aria-label={visible(app.headerText)}>
        <header className="drawn-by">
          <span className="app-mark" aria-hidden="true">
            {visible(app.appName).slice(0, 1).toUpperCase()}
          </span>
          <span>{visible(app.headerText)}</span>
        </header>
        {open === undefined ? (
          <div className="app-opening" role="status">
            <span className="app-spinner" aria-hidden="true" />
            {OPENING_THE_APP}
          </div>
        ) : open.state === "shown" ? (
          <Framed shown={open} app={app} seam={seam} session={session} onPrompt={prompt} />
        ) : open.state === "blocked" ? (
          <p className="app-blocked">
            <strong>{visible(open.text)}</strong>
          </p>
        ) : (
          <div className="app-elsewhere">
            <p>{visible(open.text)}</p>
            {open.resultText.trim() && <p className="app-result">{visible(open.resultText)}</p>}
          </div>
        )}
      </section>
      {prompts.map(({ key, prompt: shown }) => (
        <Prompt key={key} prompt={shown} app={app} seam={seam} session={session} onDone={() => dismiss(key)} />
      ))}
      {below}
    </div>
  );
}

/** The sandboxed frame and its bridge to the home. */
function Framed({
  shown,
  app,
  seam,
  session,
  onPrompt,
}: {
  shown: Shown;
  app: AppView;
  seam?: AppsSeam;
  session?: string;
  onPrompt: (prompt: AppPrompt) => void;
}) {
  const frame = useRef<HTMLIFrameElement>(null);
  const [height, setHeight] = useState(Math.min(FIRST_HEIGHT, shown.maxHeight));
  const prompted = useRef(onPrompt);
  prompted.current = onPrompt;
  useEffect(() => {
    if (!seam || !session) return;
    const bridge = new AppBridge({
      post: (message) => frame.current?.contentWindow?.postMessage(message, "*"),
      ask: (request) => seam.request(session, app.callId, request),
      resize: (next) => setHeight(Math.min(Math.ceil(next), shown.maxHeight)),
      prompt: (next) => prompted.current(next),
      theme: pageTheme,
      toolInput: shown.toolInput,
      toolResult: shown.toolResult,
    });
    // A sandboxed frame's origin is opaque, so only its window identifies it.
    const onMessage = (event: MessageEvent) => {
      if (!frame.current || event.source !== frame.current.contentWindow) return;
      void bridge.receive(event.data);
    };
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, [seam, session, app.callId, shown]);
  return (
    <iframe
      ref={frame}
      className={`app-frame${shown.prefersBorder ? " app-frame-border" : ""}`}
      title={visible(app.headerText)}
      src={shown.frameUrl}
      sandbox="allow-scripts"
      referrerPolicy="no-referrer"
      allow=""
      style={{ height }}
    />
  );
}

/** What the app asked for, outside its frame: a notice, a message to send, or a link to open. */
function Prompt({
  prompt,
  app,
  seam,
  session,
  onDone,
}: {
  prompt: AppPrompt;
  app: AppView;
  seam?: AppsSeam;
  session?: string;
  onDone: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string>();
  switch (prompt.kind) {
    case "notice":
      return (
        <p className="drawn-notice app-notice" role="note">
          <svg className="icon" aria-hidden="true" viewBox="0 0 24 24" focusable="false">
            <circle cx="12" cy="12" r="9" fill="none" stroke="currentColor" strokeWidth="1.5" />
            <path d="M12 11v6M12 7.5v.01" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" />
          </svg>
          <span>{visible(prompt.text)}</span>
        </p>
      );
    case "message": {
      const send = async () => {
        if (!seam || !session || busy) return;
        setBusy(true);
        setProblem(undefined);
        try {
          await seam.message(session, app.callId, prompt.message);
          onDone();
        } catch (error) {
          setBusy(false);
          setProblem(error instanceof Error ? error.message : String(error));
        }
      };
      return (
        <div className="app-ask" role="group" aria-label={visible(prompt.text)}>
          <p className="app-ask-text">{visible(prompt.text)}</p>
          <div className="app-ask-actions">
            <button type="button" className="button button-primary" onClick={send} disabled={busy || !seam || !session}>
              {visible(prompt.sendLabel)}
            </button>
            <button type="button" className="button button-secondary" onClick={onDone} disabled={busy}>
              {visible(prompt.dismissLabel)}
            </button>
          </div>
          <p className="app-ask-note">{visible(prompt.note)}</p>
          {problem && (
            <p className="app-ask-note" role="alert">
              {visible(problem)}
            </p>
          )}
        </div>
      );
    }
    case "link": {
      const ok = openable(prompt.url);
      const go = () => {
        if (!ok) return;
        window.open(prompt.url, "_blank", "noopener,noreferrer");
        onDone();
      };
      return (
        <div className="app-ask" role="group" aria-label={visible(prompt.text)}>
          <p className="app-ask-text">
            <strong>{visible(prompt.text)}</strong>
          </p>
          <p className="app-ask-url mono">{visible(prompt.url)}</p>
          <div className="app-ask-actions">
            <button type="button" className="button button-primary" onClick={go} disabled={!ok}>
              {visible(prompt.openLabel)}
            </button>
            <button type="button" className="button button-secondary" onClick={onDone}>
              {visible(prompt.dismissLabel)}
            </button>
          </div>
        </div>
      );
    }
  }
}
