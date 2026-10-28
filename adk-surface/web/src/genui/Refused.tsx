import type { RefusedDrawing } from "../generated/api";
import { visible } from "../safe";
import "./genui.css";

/**
 * A drawing that was not shown (spec 3.3 §7.4 rule 5): a neutral one-line notice in the thread,
 * in plain words. The technical reason stays in the developer panel and the terminal.
 */
export function RefusedNotice({ refusal }: { refusal: RefusedDrawing }) {
  return (
    <p className="drawn-notice" role="note">
      <svg className="icon" aria-hidden="true" viewBox="0 0 24 24" focusable="false">
        <circle cx="12" cy="12" r="9" fill="none" stroke="currentColor" strokeWidth="1.5" />
        <path d="M12 11v6M12 7.5v.01" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" />
      </svg>
      <span>{visible(refusal.text)}</span>
    </p>
  );
}
