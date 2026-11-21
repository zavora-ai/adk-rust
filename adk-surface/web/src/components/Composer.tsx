import { useContext, useEffect, useId, useRef, useState } from "react";
import type { ClipboardEvent, DragEvent, FormEvent } from "react";
import type { ImageData, Origin } from "../generated/api";
import { MAX_IMAGES, readImages } from "../images";
import type { Attached, Refusal } from "../images";
import { labels } from "../labels";
import { VoiceEntryContext } from "../voice/context";
import { voiceLabels } from "../voice/labels";

/**
 * The message box under a thread. While a decision of the session is open, what is typed is
 * queued as the next message, never taken as an answer (spec 3.3 §7.4 rule 2, S-I4); Enter
 * sends and never decides. With `attach`, it takes images too — picked, pasted or dropped —
 * and sends them with the text (spec 3.3 §5.1).
 */
export function Composer({
  placeholder,
  queued,
  disabled = false,
  attach = false,
  askDrawing = false,
  onStopTurn,
  draft,
  onSend,
}: {
  placeholder: string;
  queued: string[];
  disabled?: boolean;
  /** The message box takes images. */
  attach?: boolean;
  /** Offers "Ask for a drawing": the request goes in the box, sent only with Send. */
  askDrawing?: boolean;
  /** While the agent works on this thread: stops its turn (spec 3.3 §5.1). */
  onStopTurn?: (origin: Origin) => void;
  /** Text put in the box from outside, such as a suggested reply; `n` changes with each. */
  draft?: { text: string; n: number };
  onSend: (text: string, origin: Origin, images?: ImageData[]) => Promise<void>;
}) {
  const [text, setText] = useState("");
  const [images, setImages] = useState<Attached[]>([]);
  const [refused, setRefused] = useState<Refusal | undefined>();
  const [over, setOver] = useState(false);
  const [sending, setSending] = useState(false);
  const field = useId();
  const picker = useRef<HTMLInputElement>(null);
  const input = useRef<HTMLInputElement>(null);
  // A suggested reply fills the box and takes the focus; nothing is sent until Send.
  useEffect(() => {
    if (!draft) return;
    setText(draft.text);
    input.current?.focus();
  }, [draft]);
  // The microphone, where the server offers voice and the page may use it (spec 3.3 §5.6).
  const voice = useContext(VoiceEntryContext);

  async function add(files: File[]) {
    if (!attach || files.length === 0) return;
    const read = await readImages(files, images.length);
    setImages((have) => [...have, ...read.images].slice(0, MAX_IMAGES));
    setRefused(read.refused[0]);
  }

  async function submit(event: FormEvent, origin: Origin) {
    event.preventDefault();
    const message = text.trim();
    if ((!message && images.length === 0) || sending) return;
    setSending(true);
    try {
      const sent = images.map(({ mimeType, data }) => ({ mimeType, data }));
      await (sent.length > 0 ? onSend(message, origin, sent) : onSend(message, origin));
      setText("");
      setImages([]);
      setRefused(undefined);
    } finally {
      setSending(false);
    }
  }

  const onPaste = (event: ClipboardEvent) => {
    const files = [...event.clipboardData.files].filter((file) => file.type.startsWith("image/"));
    if (!attach || files.length === 0) return;
    event.preventDefault();
    void add(files);
  };
  const onDrop = (event: DragEvent) => {
    if (!attach) return;
    event.preventDefault();
    setOver(false);
    void add([...event.dataTransfer.files]);
  };

  return (
    <div className="composer-wrap">
      {/* Outside the form, so Enter in the message still sends it. */}
      {attach && (
        <input
          ref={picker}
          type="file"
          accept="image/png,image/jpeg,image/webp,image/gif"
          multiple
          hidden
          onChange={(event) => {
            void add([...(event.target.files ?? [])]);
            event.target.value = "";
          }}
        />
      )}
      {queued.length > 0 && (
        <ul className="queued" aria-label={labels.queued}>
          {queued.map((message, index) => (
            <li key={index}>
              <span className="queued-text">{message}</span>
              <span className="queued-note">{labels.queued}</span>
            </li>
          ))}
        </ul>
      )}
      <form
        className={`composer${images.length > 0 ? " composer-with-images" : ""}${over ? " composer-over" : ""}`}
        onSubmit={(event) => void submit(event, { kind: "keyboard" })}
        onDragOver={(event) => {
          if (!attach || !event.dataTransfer.types.includes("Files")) return;
          event.preventDefault();
          setOver(true);
        }}
        onDragLeave={() => setOver(false)}
        onDrop={onDrop}
      >
        {images.length > 0 && (
          <ul className="composer-images" aria-label={labels.imagesToSend}>
            {images.map((image, index) => (
              <li key={image.url}>
                <img src={image.url} alt="" />
                <button
                  type="button"
                  className="composer-image-remove"
                  aria-label={labels.removeImage(index + 1)}
                  onClick={() => setImages((have) => have.filter((_, at) => at !== index))}
                >
                  <svg viewBox="0 0 16 16" aria-hidden="true">
                    <path d="M4 4l8 8M12 4l-8 8" />
                  </svg>
                </button>
              </li>
            ))}
            <li className="composer-images-hint">{labels.imagesHint(images.length, MAX_IMAGES)}</li>
          </ul>
        )}
        <div className="composer-row">
          {attach && (
            <button
                type="button"
                className="icon-button composer-attach"
                aria-label={labels.attachImages}
                disabled={disabled || images.length >= MAX_IMAGES}
                onClick={() => picker.current?.click()}
              >
                <svg className="icon" viewBox="0 0 24 24" aria-hidden="true">
                  <rect x="3" y="4" width="18" height="16" rx="3" />
                  <circle cx="9" cy="10" r="2" />
                <path d="M21 16l-5-5-9 9" />
              </svg>
            </button>
          )}
          {askDrawing && (
            <button
              type="button"
              className="icon-button composer-ask-drawing"
              aria-label={labels.askDrawing}
              title={labels.askDrawing}
              disabled={disabled}
              onClick={() => {
                setText((current) => (current.startsWith(labels.askDrawingText) ? current : `${labels.askDrawingText}${current}`));
                input.current?.focus();
              }}
            >
              <svg className="icon" viewBox="0 0 24 24" aria-hidden="true">
                <path d="M4 19.5h16M6.5 16V11M11 16V6.5M15.5 16v-6M20 16V8.5" />
              </svg>
            </button>
          )}
          <label htmlFor={field} className="visually-hidden">
            {placeholder}
          </label>
          <input
            ref={input}
            id={field}
            className="composer-input"
            placeholder={placeholder}
            value={text}
            autoComplete="off"
            disabled={disabled}
            onChange={(event) => setText(event.target.value)}
            onPaste={onPaste}
          />
          {voice && (
            <button type="button" className="button composer-mic" aria-label={voiceLabels.talk} onClick={voice.open}>
              <svg viewBox="0 0 24 24" width="20" height="20" aria-hidden="true">
                <path
                  d="M12 15a3 3 0 0 0 3-3V6a3 3 0 0 0-6 0v6a3 3 0 0 0 3 3Zm5-3a5 5 0 0 1-10 0H5a7 7 0 0 0 6 6.93V21h2v-2.07A7 7 0 0 0 19 12h-2Z"
                  fill="currentColor"
                />
              </svg>
            </button>
          )}
          {onStopTurn && (
            <button
              type="button"
              className="button button-secondary composer-stop"
              onClick={(event) => onStopTurn(event.detail === 0 ? { kind: "keyboard" } : { kind: "pointer" })}
            >
              <svg className="icon" viewBox="0 0 24 24" aria-hidden="true">
                <rect x="7" y="7" width="10" height="10" rx="2" />
              </svg>
              {labels.stopTurn}
            </button>
          )}
          <button
            type="button"
            className="button button-stop composer-send"
            disabled={disabled || sending}
            onClick={(event) => void submit(event, event.detail === 0 ? { kind: "keyboard" } : { kind: "pointer" })}
          >
            {labels.send}
          </button>
        </div>
        {refused && (
          <p className="composer-refused" role="alert">
            {labels.imageRefused[refused]}
          </p>
        )}
      </form>
    </div>
  );
}
