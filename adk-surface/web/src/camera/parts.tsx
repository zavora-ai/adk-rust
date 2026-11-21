import { useState } from "react";
import type { MouseEvent } from "react";
import type {
  BudgetView,
  CameraAgent,
  CameraState,
  Command,
  CommandOutcome,
  DecisionCard,
  NoticeView,
  Origin,
  SourceTile,
  Tone,
  TrackRow,
  WatchView,
} from "../generated/api";
import { Avatar } from "../components/Avatar";
import { problemText } from "../client";
import { EvidenceThumb } from "../evidence/EvidencePanel";
import { enterNeverDecides, originOf } from "../origin";
import { CameraStage, swatchFor } from "./CameraStage";
import { CameraIcon, InfoIcon, LockIcon, WarnIcon } from "./icons";
import { cameraLabels as words } from "./labels";

/** A camera's state as a pill: a word and a colour (spec 3.3 §9). */
export function StatePill({ tone, state, children, small = false }: { tone: Tone; state?: CameraState; children: string; small?: boolean }) {
  const dot = state !== "off" && state !== "stopped";
  return (
    <span className={`cam-pill cam-pill-${tone}${small ? " cam-pill-sm" : ""}`}>
      {dot && <i aria-hidden="true" />}
      {children}
    </span>
  );
}

/** A label's colour as a small square, the boards' swatch. */
export function Swatch({ label, faded = false }: { label: string; faded?: boolean }) {
  const [fill] = swatchFor(label === "zone" ? "zone" : label);
  return <i className={`cam-swatch${faded ? " cam-swatch-faded" : ""}`} style={{ background: fill }} aria-hidden="true" />;
}

/** "Camera on / Off", as the voice screen's switches (board choice C2). */
export function Switch({
  on,
  disabled,
  short = false,
  icon = !short,
  label,
  onSwitch,
}: {
  on: boolean;
  disabled?: boolean;
  /** "On" in place of "Camera on": the phone and the switch list. */
  short?: boolean;
  /** The camera beside "On": the phone's own camera screen. */
  icon?: boolean;
  label: string;
  onSwitch: (on: boolean, origin: Origin) => void;
}) {
  return (
    <span className="cam-seg" role="group" aria-label={label}>
      <button
        type="button"
        className={on ? "cam-seg-on" : ""}
        aria-pressed={on}
        disabled={disabled}
        onClick={(event) => !on && onSwitch(true, originOf(event))}
      >
        {icon && <CameraIcon />}
        {short ? words.on : words.cameraOn}
      </button>
      <button
        type="button"
        className={on ? "" : "cam-seg-off"}
        aria-pressed={!on}
        disabled={disabled}
        onClick={(event) => on && onSwitch(false, originOf(event))}
      >
        {words.off}
      </button>
    </span>
  );
}

/** The banner over the picture (board choices C4–C6). */
export function Notice({
  notice,
  short = false,
  onRetry,
  onLimit,
}: {
  notice: NoticeView;
  short?: boolean;
  onRetry?: () => void;
  onLimit?: (href: string) => void;
}) {
  const action = notice.action;
  const actionText = action ? (short ? action.shortText : action.text) : undefined;
  return (
    <div className={`cam-notice cam-notice-${notice.tone}`} role="status">
      <WarnIcon />
      <span>
        <b>{short ? notice.shortTitle : notice.title}</b>
        {(short ? notice.shortText : notice.text) && <> {short ? notice.shortText : notice.text}</>}
        {!short && notice.detail && <code className="cam-mono"> {notice.detail}</code>}
        {action?.kind === "limit" && (
          <>
            {" "}
            <a className="cam-link" href={action.href} onClick={(event) => {
              if (!onLimit) return;
              event.preventDefault();
              onLimit(action.href);
            }}>
              {actionText}
            </a>
          </>
        )}
        {action?.kind === "retry" && short && (
          <>
            {" "}
            <button type="button" className="cam-link" onClick={onRetry}>
              {actionText}
            </button>
          </>
        )}
      </span>
      {action?.kind === "retry" && !short && (
        <button type="button" className="cam-btn cam-btn-xs" onClick={onRetry}>
          {actionText}
        </button>
      )}
    </div>
  );
}

/** The agent that watches, above its decisions (board choice C1). */
export function AgentHead({ agent, onOpenThread }: { agent: CameraAgent; onOpenThread?: () => void }) {
  return (
    <div className="cam-agent">
      <Avatar agent={agent.agent} />
      <span className="cam-agent-words">
        <span className="cam-agent-name">{agent.agent.displayName}</span>
        <span className={`cam-tone-${agent.tone}`}>{agent.line}</span>
      </span>
      {onOpenThread && (
        <button type="button" className="cam-link cam-agent-open" onClick={onOpenThread}>
          {words.openThread}
        </button>
      )}
    </div>
  );
}

/**
 * A decision beside the camera: the held line stop with its frame (board choice C13). Its first
 * answer that lets the call run, and Open, which shows the whole card in its thread.
 */
export function DecisionMini({
  card,
  onOpen,
  decide,
  short = false,
}: {
  card: DecisionCard;
  onOpen: (card: DecisionCard) => void;
  decide?: (command: Command, origin: Origin) => Promise<CommandOutcome>;
  short?: boolean;
}) {
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState("");
  const allow = card.allowed.options.find((option) => option.answer !== "deny" && option.answer !== "edit");
  const image = card.media?.images[0];
  const text = card.summary ?? (image?.caption ? `${image.caption} The frame is on the card.` : undefined);
  async function answer(event: MouseEvent<HTMLButtonElement>) {
    if (!allow || !decide) return;
    const origin = originOf(event);
    setBusy(true);
    setProblem("");
    try {
      await decide(
        {
          type: "decide",
          decisionId: card.id,
          version: card.version,
          answer: allow.answer,
          expanded: false,
          ...(card.media ? { evidence: card.media.images.filter((one) => !one.purged).map((one) => one.digest) } : {}),
        },
        origin,
      );
    } catch (error) {
      setProblem(problemText(error));
    } finally {
      setBusy(false);
    }
  }
  if (short) {
    return (
      <button type="button" className="cam-decision cam-decision-short" onClick={() => onOpen(card)}>
        {card.media && <EvidenceThumb media={card.media} />}
        <span className="cam-decision-words">
          <span className="cam-decision-heading">{card.heading}</span>
          <span className="cam-decision-meta">
            {card.riskText}
            {image?.caption ? ` · ${image.caption.charAt(0).toLowerCase()}${image.caption.slice(1).replace(/\.$/, "")}` : ""}
          </span>
        </span>
        <span className="cam-chevron" aria-hidden="true">›</span>
      </button>
    );
  }
  return (
    <article className="cam-decision" aria-label={card.heading}>
      <div className="cam-decision-top">
        {card.media && <EvidenceThumb media={card.media} />}
        <span className="cam-decision-words">
          <span className="cam-decision-heading">{card.heading}</span>
          <span className="cam-decision-meta">
            {card.agent.displayName} · {card.riskText} · {card.sinceText}
          </span>
          {text && <span className="cam-decision-text">{text}</span>}
        </span>
      </div>
      <div className="cam-decision-actions">
        {allow && decide && (
          <button type="button" className="cam-btn cam-btn-primary cam-btn-sm" disabled={busy} onClick={(event) => void answer(event)} onKeyDown={enterNeverDecides}>
            {allow.needsPresence && <LockIcon />}
            {allow.label}
          </button>
        )}
        <button type="button" className="cam-btn cam-btn-text cam-btn-sm" onClick={() => onOpen(card)}>
          {words.open}
        </button>
      </div>
      {problem && (
        <p className="cam-problem" role="alert">
          {problem}
        </p>
      )}
    </article>
  );
}

/** One watch condition (board choice C1 "Watching for"). */
export function Watch({ watch, onEditZone }: { watch: WatchView; onEditZone?: () => void }) {
  return (
    <div className={`cam-watch${watch.active ? "" : " cam-watch-idle"}`}>
      <span className="cam-watch-head">
        <Swatch label={watch.label} />
        {watch.name}
        <span className="cam-spacer" />
        <span className={`cam-pill cam-pill-sm ${watch.active ? "cam-pill-ok" : "cam-pill-neutral"}`}>{watch.pill}</span>
      </span>
      <span className="cam-watch-rule">
        {watch.before}
        {watch.zone && (
          <>
            {" "}
            <Swatch label="zone" />
            <b>{watch.zone}</b>
          </>
        )}
        {watch.after}
      </span>
      <span className="cam-watch-foot">
        {watch.zone && (
          <button type="button" className="cam-link" onClick={onEditZone} disabled={!onEditZone}>
            {words.editZone}
          </button>
        )}
        <span className="cam-muted">{watch.woke}</span>
      </span>
    </div>
  );
}

/** "Frame budget" (board choice C3): three meters, the order it steps down, what it did. */
export function Budget({ budget, onLimit }: { budget: BudgetView; onLimit?: () => void }) {
  return (
    <>
      {budget.meters.map((meter) => (
        <div className="cam-meter" key={meter.label}>
          <span className="cam-meter-top">
            <span>{meter.label}</span>
            <b>{meter.value}</b>
          </span>
          <div className="cam-bar" role="meter" aria-label={meter.label} aria-valuenow={meter.percent} aria-valuemin={0} aria-valuemax={100}>
            <i className={`cam-bar-${meter.tone}`} style={{ width: `${meter.percent}%` }} />
          </div>
          {meter.note && <span className="cam-meter-note">{meter.note}</span>}
        </div>
      ))}
      {budget.order && <span className="cam-small cam-muted">{budget.order}</span>}
      {budget.steps.length > 0 && (
        <>
          <span className="cam-label">{words.whatBudgetDid}</span>
          <div className="cam-steps">
            {budget.steps.map((step) => (
              <span className="cam-step" key={`${step.at}-${step.text}`}>
                <span className="cam-mono cam-muted">{step.at}</span>
                <span className={step.next ? "cam-muted" : ""}>{step.text}</span>
              </span>
            ))}
          </div>
        </>
      )}
      {budget.limit && (
        <button type="button" className="cam-link" onClick={onLimit}>
          {budget.limit}
        </button>
      )}
    </>
  );
}

/** The phone's budget rows (board choice C14). */
export function BudgetRows({ budget }: { budget: BudgetView }) {
  return (
    <div className="cam-rows">
      {budget.rows.map((row) => (
        <span className="cam-row" key={row.label}>
          <span className="cam-muted">{row.label}</span>
          <span className={`cam-tone-${row.tone}`}>{row.value}</span>
        </span>
      ))}
    </div>
  );
}

/** "Tracks now". */
export function Tracks({ tracks, short = false, limit }: { tracks: TrackRow[]; short?: boolean; limit?: number }) {
  return (
    <div className="cam-tracks">
      {tracks.slice(0, limit ?? tracks.length).map((track) => (
        <div className={`cam-track${track.finding && !short ? " cam-track-finding" : ""}`} key={track.id}>
          <Swatch label={track.swatch} />
          <span className="cam-mono">{track.id}</span>
          <b>{track.label}</b>
          <span className="cam-muted">{short ? track.shortNote : track.note}</span>
        </div>
      ))}
    </div>
  );
}

/** "Sources": every camera, the one shown large marked (board choice C15). */
export function Sources({ sources, onSelect }: { sources: SourceTile[]; onSelect: (id: string) => void }) {
  return (
    <div className="cam-sources">
      <span className="cam-sources-head">
        <b>{words.sources}</b>
        <span className="cam-muted">{words.selectOne}</span>
      </span>
      <div className="cam-sources-row">
        {sources.map((tile) => (
          <button
            type="button"
            key={tile.id}
            className={`cam-source${tile.selected ? " cam-source-selected" : ""}`}
            aria-current={tile.selected ? "true" : undefined}
            onClick={() => onSelect(tile.id)}
          >
            <CameraStage stage={tile.stage} width={176} name={tile.name} size="strip" tags={tile.selected} />
            <span className="cam-source-foot">
              <b>{tile.name}</b>
              <span className="cam-spacer" />
              <span className={`cam-tone-${tile.tone}`}>{tile.stateText}</span>
            </span>
          </button>
        ))}
      </div>
    </div>
  );
}

export { InfoIcon };
