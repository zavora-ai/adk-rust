import type { MouseEvent } from "react";
import type { Origin, StatusLine } from "../generated/api";
import type { AreaSettings } from "../kernel";
import { onThisDevice } from "../away";
import { areaStatus, quietText } from "../settings";
import { labels } from "../labels";
import { originOf } from "../origin";
import type { Section } from "../route";
import { pathOf } from "../route";
import type { Theme } from "../theme";
import { Icon } from "./Icon";

const tabs: Array<{ section: Section; label: string }> = [
  { section: "threads", label: labels.threads },
  { section: "decisions", label: labels.decisions },
  { section: "today", label: labels.today },
  { section: "memory", label: labels.memoryAndRules },
];

/** Each section's mark in the icon rail of a wide screen (spec 3.3 §5.1). */
const ICONS: Partial<Record<Section, string>> = {
  threads: "M4 5.5h16v10H10l-4 3.5v-3.5H4z",
  decisions: "M4 13.5 6.5 5h11l2.5 8.5v5H4zM4 13.5h4.5l1.5 2h4l1.5-2H20",
  today: "M4.5 6h15v13.5h-15zM4.5 10h15M9 3.5v4M15 3.5v4",
  memory: "M5 4.5h10.5a3 3 0 0 1 3 3v12H8a3 3 0 0 1-3-3zM5 16.5a3 3 0 0 1 3-3h10.5",
  developer: "M8.5 7 3.5 12l5 5M15.5 7l5 5-5 5",
  website: "M12 3.5a8.5 8.5 0 1 0 0 17 8.5 8.5 0 0 0 0-17zM3.5 12h17M12 3.5c2.4 2.3 3.5 5.2 3.5 8.5s-1.1 6.2-3.5 8.5c-2.4-2.3-3.5-5.2-3.5-8.5S9.6 5.8 12 3.5z",
};

/**
 * The header every section shares (spec 3.3 §5.1): the brand, the sections, the status line
 * and Stop all — or, once stopped, Resume all, which only the web home offers (S-I10).
 */
export function Header({
  section,
  status,
  areas,
  conversationMode,
  decisions,
  website,
  theme,
  onNavigate,
  onTheme,
  onStop,
  onResume,
  onPalette,
}: {
  section: Section;
  status?: StatusLine;
  /** Each area's settings in force (spec 3.3 §10 K3): with one area, the status line reads them. */
  areas?: AreaSettings[];
  /**
   * The mode the conversation on Threads runs in (`status.mode` read with its session): a
   * session's own mode moves on from its area's with an approved plan exit (§8.1). Elsewhere no
   * conversation is shown, and the line reads the area's settings in force.
   */
  conversationMode?: string;
  decisions: number;
  /** The website section's tab, when the server serves a website (spec 3.5 §8.2). */
  website?: string;
  theme: Theme;
  onNavigate: (section: Section) => void;
  onTheme: () => void;
  onStop: (origin: Origin) => void;
  onResume: (origin: Origin) => void;
  /** Opens the command palette (⌘K). */
  onPalette?: () => void;
}) {
  // One area's settings change while the kernel runs; the engine's line holds the ones it
  // started with, so the kernel's are shown once read. Quiet time reads on this device's clock.
  const host = status?.hostUtcOffsetMinutes ?? undefined;
  const now = new Date();
  const quiet = status?.quiet && quietText(onThisDevice({ start: status.quiet.start, end: status.quiet.end }, host), now);
  const line =
    section === "memory"
      ? [labels.keptOnYourComputer]
      : areas?.length === 1
        ? areaStatus(areas[0], now, host, conversationMode)
        : [status?.mode?.status, status?.spend?.text, quiet].filter((part): part is string => !!part);
  const themeLabel =
    theme === "system" ? labels.themeSystem : theme === "light" ? labels.themeLight : labels.themeDark;
  const go = (target: Section) => (event: MouseEvent<HTMLAnchorElement>) => {
    if (event.metaKey || event.ctrlKey || event.shiftKey || event.button !== 0) return;
    event.preventDefault();
    onNavigate(target);
  };
  return (
    <header className="header">
      <a className="brand" href={pathOf("threads")} onClick={go("threads")}>
        <i className="brand-dot" aria-hidden="true" />
        {labels.brand}
      </a>
      <nav className="tabs" aria-label={labels.sections}>
        {[
          ...tabs,
          ...(website ? [{ section: "website" as const, label: website }] : []),
          { section: "developer" as const, label: labels.developerShort },
        ].map((tab) => (
          <a
            key={tab.section}
            className={`tab tab-${tab.section}`}
            href={pathOf(tab.section)}
            aria-current={section === tab.section ? "page" : undefined}
            title={tab.label}
            onClick={go(tab.section)}
          >
            <svg className="tab-icon" viewBox="0 0 24 24" aria-hidden="true">
              <path d={ICONS[tab.section]} />
            </svg>
            <span className="tab-label">{tab.label}</span>
            {tab.section === "decisions" && decisions > 0 && (
              <span className="badge badge-tab" aria-label={`${labels.decisions}: ${decisions}`}>
                {decisions}
              </span>
            )}
          </a>
        ))}
      </nav>
      {status?.stopped ? (
        <p className="status-line status-stopped" role="status">
          {status.stoppedText ?? labels.stopped}
        </p>
      ) : (
        <p className="status-line">
          {line.map((part) => (
            <span key={part}>{part}</span>
          ))}
        </p>
      )}
      <div className="header-end">
        {onPalette && (
          <button
            type="button"
            className="palette-open"
            title={labels.paletteOpen}
            aria-keyshortcuts="Meta+K Control+K"
            onClick={onPalette}
          >
            <svg className="icon" viewBox="0 0 24 24" aria-hidden="true">
              <circle cx="11" cy="11" r="6.5" />
              <path d="M16 16l4.5 4.5" />
            </svg>
            <span className="palette-open-text">{labels.search}</span>
            <kbd aria-hidden="true">⌘K</kbd>
          </button>
        )}
        <button
          type="button"
          className="icon-button"
          aria-label={`${labels.theme}: ${themeLabel}`}
          title={`${labels.theme}: ${themeLabel}`}
          onClick={onTheme}
        >
          <Icon name={theme === "light" ? "sun" : theme === "dark" ? "moon" : "half"} />
        </button>
        {status?.stopped ? (
          <button type="button" className="button button-primary" onClick={(event) => onResume(originOf(event))}>
            {labels.resumeAll}
          </button>
        ) : (
          <button type="button" className="button button-stop" onClick={(event) => onStop(originOf(event))}>
            {labels.stopAll}
          </button>
        )}
      </div>
    </header>
  );
}
