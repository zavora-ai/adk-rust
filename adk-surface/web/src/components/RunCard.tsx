import { useRef, useState } from "react";
import type { CommandRun, ImageData, ShellPlace, ShellView } from "../generated/api";
import { imageUrl } from "../images";
import { labels } from "../labels";
import { visible } from "../safe";

/** Commands run apart from this computer: the OS sandbox, a container or a remote one. */
export function isolated(place: ShellPlace): boolean {
  return place.kind === "sandbox" || place.kind === "container" || place.kind === "remote";
}

function ShieldIcon() {
  return (
    <svg className="shell-icon" viewBox="0 0 16 16" aria-hidden="true">
      <path d="M8 1.5 2.5 4v4c0 3 2.4 5.4 5.5 6.5 3.1-1.1 5.5-3.5 5.5-6.5V4z" />
    </svg>
  );
}

/**
 * Where an agent's commands run (spec 3.3 §5.7): green when apart from this computer, amber
 * when a sandbox was asked for and is unavailable, quiet on this computer by choice.
 */
export function ShellBadge({ shell }: { shell: ShellView }) {
  const tone = isolated(shell.place) ? "isolated" : shell.place.kind === "degraded" ? "degraded" : "host";
  return (
    <span className={`shell-badge shell-badge-${tone}`} title={visible(shell.text)}>
      {tone !== "host" && <ShieldIcon />}
      {visible(shell.text)}
    </span>
  );
}

/**
 * A command or code an agent ran (spec 3.3 §5.1): what ran, how it exited, for how long and
 * where, and its last lines, open when it failed. `what` is the call's own line when the
 * result does not repeat the command. Every string is data (S-I9).
 */
export function RunCard({
  tool,
  what,
  ok,
  run,
  shell,
}: {
  tool: string;
  what: string;
  ok: boolean;
  run: CommandRun;
  /** Where the agent's commands run; the sandbox's own tool always runs in the sandbox. */
  shell?: ShellView;
}) {
  const sandboxed = tool === "sandbox_exec" || (shell ? isolated(shell.place) : false);
  const place = tool === "sandbox_exec" || shell ? (sandboxed ? labels.sandbox : labels.onThisComputer) : undefined;
  const status = run.stopped
    ? labels.runStopped[run.stopped]
    : run.exitCode !== undefined && run.exitCode !== null
      ? labels.exitCode(run.exitCode)
      : undefined;
  const command = visible(run.command ?? what);
  return (
    <details className={`run${ok ? "" : " run-failed"}`} open={!ok && run.output.length > 0}>
      <summary className="run-head">
        <span className={`run-mark${ok ? " run-mark-ok" : ""}`} aria-hidden="true">
          <svg viewBox="0 0 16 16">{ok ? <path d="M3.5 8.5l3 3 6-7" /> : <path d="M4 4l8 8M12 4l-8 8" />}</svg>
        </span>
        <code className="run-command">
          {run.language && run.language !== "command" ? (
            <span className="run-prompt">{visible(run.language)}</span>
          ) : (
            <span className="run-prompt">$</span>
          )}{" "}
          {command}
        </code>
        {status && <span className={`run-status${ok ? "" : " run-status-failed"}`}>{status}</span>}
        {run.durationMs !== undefined && run.durationMs !== null && (
          <span className="run-time">{run.durationMs < 1000 ? `${run.durationMs} ms` : `${(run.durationMs / 1000).toFixed(1)} s`}</span>
        )}
        {place && (
          <span className={`run-place${sandboxed ? " run-place-sandbox" : ""}`} title={shell ? visible(shell.text) : undefined}>
            {sandboxed && <ShieldIcon />}
            {place}
          </span>
        )}
      </summary>
      {run.output.length > 0 && (
        <pre className="run-output" aria-label={labels.output}>
          {run.output.map((line, index) => (
            <span key={index} className={line.stderr ? "run-stderr" : undefined}>
              {visible(line.text)}
              {"\n"}
            </span>
          ))}
        </pre>
      )}
      {(run.lines > run.output.length || run.truncated) && (
        <p className="run-foot">
          {run.lines > run.output.length && <span>{labels.lastLines(run.output.length, run.lines)}</span>}
          {run.truncated && <span>{labels.agentCanReadAll}</span>}
        </p>
      )}
    </details>
  );
}

/** The images of a person's message: thumbnails that open large in a dialog. */
export function MessageImages({ images }: { images: ImageData[] }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const [open, setOpen] = useState<number>();
  const show = (index: number) => {
    setOpen(index);
    dialog.current?.showModal?.();
  };
  const shown = open === undefined ? undefined : images[open];
  return (
    <>
      <div className={`message-images message-images-${Math.min(images.length, 4)}`}>
        {images.map((image, index) => (
          <button key={index} type="button" className="message-image" aria-label={labels.openImage(index + 1, images.length)} onClick={() => show(index)}>
            <img src={imageUrl(image)} alt="" />
          </button>
        ))}
      </div>
      <dialog
        ref={dialog}
        className="image-viewer"
        aria-label={shown ? labels.openImage((open ?? 0) + 1, images.length) : undefined}
        onClose={() => setOpen(undefined)}
        onClick={(event) => {
          if (event.target === dialog.current) dialog.current?.close();
        }}
      >
        {shown && <img src={imageUrl(shown)} alt="" />}
        <form method="dialog">
          <button type="submit" className="button button-secondary image-viewer-close">
            {labels.closeImage}
          </button>
        </form>
      </dialog>
    </>
  );
}
