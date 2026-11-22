import { useEffect, useId, useMemo, useRef, useState } from "react";
import type { KeyboardEvent } from "react";
import { labels } from "../labels";
import { visible } from "../safe";

/** One thing the palette can do: go somewhere, open something, or act. */
export type PaletteItem = {
  id: string;
  /** "Go to", "Agents", "Decisions", "Actions". */
  group: string;
  label: string;
  /** A quieter word beside it: an agent's status, a decision's agent. */
  hint?: string;
  /** More words it is found by. */
  keywords?: string;
  run: () => void;
};

/**
 * How well `query` finds `text`: every word of the query starts a word of the text (best), or
 * appears in it, or its letters appear in order; 0 when it does not.
 */
export function score(query: string, text: string): number {
  const q = query.trim().toLowerCase();
  if (!q) return 1;
  const t = text.toLowerCase();
  const words = q.split(/\s+/);
  const starts = t.split(/[^\p{L}\p{N}]+/u);
  if (words.every((word) => starts.some((start) => start.startsWith(word)))) return 3;
  if (words.every((word) => t.includes(word))) return 2;
  let at = 0;
  for (const letter of q.replace(/\s+/g, "")) {
    at = t.indexOf(letter, at);
    if (at < 0) return 0;
    at += 1;
  }
  return 1;
}

/**
 * The command palette (⌘K or Ctrl+K): type to find a section, an agent's thread, an open
 * decision or an action, and Enter to go. It opens a decision's card and never answers one
 * (spec 3.3 §7.4).
 */
export function CommandPalette({ open, items, onClose }: { open: boolean; items: PaletteItem[]; onClose: () => void }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const input = useRef<HTMLInputElement>(null);
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const list = useId();

  useEffect(() => {
    const node = dialog.current;
    if (!node) return;
    if (open && !node.open) {
      setQuery("");
      setActive(0);
      node.showModal?.();
      if (!node.open) node.setAttribute("open", "");
      input.current?.focus();
    } else if (!open && node.open) {
      node.close?.();
      node.removeAttribute("open");
    }
  }, [open]);

  const found = useMemo(
    () =>
      items
        .map((item, index) => ({ item, index, rank: score(query, `${item.label} ${item.hint ?? ""} ${item.keywords ?? ""} ${item.group}`) }))
        .filter((entry) => entry.rank > 0)
        .sort((a, b) => b.rank - a.rank || a.index - b.index)
        .map((entry) => entry.item),
    [items, query],
  );
  const groups = useMemo(() => {
    const order: string[] = [];
    for (const item of found) if (!order.includes(item.group)) order.push(item.group);
    return order.map((group) => ({ group, items: found.filter((item) => item.group === group) }));
  }, [found]);
  // The list in the order it is shown, for the arrow keys.
  const shown = groups.flatMap((group) => group.items);
  const current = shown[Math.min(active, shown.length - 1)];

  const run = (item: PaletteItem) => {
    onClose();
    item.run();
  };
  const onKey = (event: KeyboardEvent) => {
    if (event.key === "ArrowDown") setActive((at) => Math.min(shown.length - 1, at + 1));
    else if (event.key === "ArrowUp") setActive((at) => Math.max(0, at - 1));
    else if (event.key === "Enter" && current) run(current);
    else if (event.key === "Escape") onClose();
    else return;
    event.preventDefault();
  };

  return (
    <dialog
      ref={dialog}
      className="palette"
      aria-label={labels.paletteTitle}
      onCancel={(event) => {
        event.preventDefault();
        onClose();
      }}
      onClick={(event) => {
        if (event.target === dialog.current) onClose();
      }}
    >
      <div className="palette-search">
        <svg className="icon" viewBox="0 0 24 24" aria-hidden="true">
          <circle cx="11" cy="11" r="6.5" />
          <path d="M16 16l4.5 4.5" />
        </svg>
        <input
          ref={input}
          role="combobox"
          aria-expanded="true"
          aria-controls={list}
          aria-activedescendant={current ? `${list}-${current.id}` : undefined}
          aria-label={labels.paletteTitle}
          placeholder={labels.palettePlaceholder}
          value={query}
          onChange={(event) => {
            setQuery(event.target.value);
            setActive(0);
          }}
          onKeyDown={onKey}
        />
        <kbd className="palette-kbd">esc</kbd>
      </div>
      <div id={list} role="listbox" aria-label={labels.paletteResults} className="palette-list">
        {shown.length === 0 && <p className="palette-empty">{labels.paletteNothing}</p>}
        {groups.map((group) => (
          <div key={group.group} role="group" aria-label={group.group} className="palette-group">
            <p className="palette-group-label" aria-hidden="true">
              {group.group}
            </p>
            {group.items.map((item) => (
              <div
                key={item.id}
                id={`${list}-${item.id}`}
                role="option"
                aria-selected={item === current}
                className="palette-option"
                onMouseMove={() => setActive(shown.indexOf(item))}
                onClick={() => run(item)}
              >
                <span className="palette-label">{visible(item.label)}</span>
                {item.hint && <span className="palette-hint">{visible(item.hint)}</span>}
              </div>
            ))}
          </div>
        ))}
      </div>
    </dialog>
  );
}
