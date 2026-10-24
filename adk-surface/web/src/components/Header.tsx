import type { MouseEvent } from "react";
import type { Origin, StatusLine } from "../generated/api";
import { labels } from "../labels";
import { originOf } from "../origin";
import type { Section } from "../route";
import { pathOf } from "../route";
import type { Theme } from "../theme";
import { Icon } from "./Icon";

const tabs: Array<{ section: Section; label: string }> = [
  { section: "threads", label: labels.threads },
  { section: "today", label: labels.today },
  { section: "memory", label: labels.memoryAndRules },
];

/**
 * The header every section shares (spec 3.3 §5.1): the brand, the sections, the status line
 * and Stop all — or, once stopped, Resume all, which only the web home offers (S-I10).
 */
export function Header({
  section,
  status,
  decisions,
  theme,
  onNavigate,
  onTheme,
  onStop,
  onResume,
}: {
  section: Section;
  status?: StatusLine;
  decisions: number;
  theme: Theme;
  onNavigate: (section: Section) => void;
  onTheme: () => void;
  onStop: (origin: Origin) => void;
  onResume: (origin: Origin) => void;
}) {
  const line =
    section === "memory"
      ? [labels.keptOnYourComputer]
      : [status?.mode?.status, status?.spend?.text, status?.quiet?.text].filter((part): part is string => !!part);
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
        {tabs.map((tab) => (
          <a
            key={tab.section}
            className="tab"
            href={pathOf(tab.section)}
            aria-current={section === tab.section ? "page" : undefined}
            onClick={go(tab.section)}
          >
            {tab.label}
            {tab.section === "threads" && decisions > 0 && (
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
