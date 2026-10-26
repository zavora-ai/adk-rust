import { useContext, useId, useState } from "react";
import type { FormEvent } from "react";
import type { Origin } from "../generated/api";
import { labels } from "../labels";
import { VoiceEntryContext } from "../voice/context";
import { voiceLabels } from "../voice/labels";

/**
 * The message box under a thread. While a decision of the session is open, what is typed is
 * queued as the next message, never taken as an answer (spec 3.3 §7.4 rule 2, S-I4); Enter
 * sends and never decides.
 */
export function Composer({
  placeholder,
  queued,
  disabled = false,
  onSend,
}: {
  placeholder: string;
  queued: string[];
  disabled?: boolean;
  onSend: (text: string, origin: Origin) => Promise<void>;
}) {
  const [text, setText] = useState("");
  const [sending, setSending] = useState(false);
  const field = useId();
  // The microphone, where the server offers voice and the page may use it (spec 3.3 §5.6).
  const voice = useContext(VoiceEntryContext);

  async function submit(event: FormEvent, origin: Origin) {
    event.preventDefault();
    const message = text.trim();
    if (!message || sending) return;
    setSending(true);
    try {
      await onSend(message, origin);
      setText("");
    } finally {
      setSending(false);
    }
  }

  return (
    <div className="composer-wrap">
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
      <form className="composer" onSubmit={(event) => void submit(event, { kind: "keyboard" })}>
        <label htmlFor={field} className="visually-hidden">
          {placeholder}
        </label>
        <input
          id={field}
          className="composer-input"
          placeholder={placeholder}
          value={text}
          autoComplete="off"
          disabled={disabled}
          onChange={(event) => setText(event.target.value)}
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
        <button
          type="button"
          className="button button-stop composer-send"
          disabled={disabled || sending}
          onClick={(event) => void submit(event, event.detail === 0 ? { kind: "keyboard" } : { kind: "pointer" })}
        >
          {labels.send}
        </button>
      </form>
    </div>
  );
}
