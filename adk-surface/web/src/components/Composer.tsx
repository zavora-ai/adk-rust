import { useId, useState } from "react";
import type { FormEvent } from "react";
import type { Origin } from "../generated/api";
import { labels } from "../labels";

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
