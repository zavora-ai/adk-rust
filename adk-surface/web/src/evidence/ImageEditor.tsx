import { useEffect, useId, useRef, useState } from "react";
import type { KeyboardEvent, PointerEvent } from "react";
import type { DecisionCard, EvidenceImage, ImageChange, ImageChanged } from "../generated/api";
import { visible } from "../safe";
import { CLASS_COLOR, CopyDigest } from "./EvidencePanel";
import { useEvidenceImage } from "./images";
import { evidenceLabels as labels } from "./labels";

type Box = [number, number, number, number];
type Edge = "left" | "top" | "right" | "bottom";

/** The smallest crop or box, in the image's pixels. */
const MIN = 8;

/** The short form of a digest, as the engine writes it. */
function short(digest: string) {
  const [scheme, hex = ""] = digest.split(":");
  return hex.length > 14 ? `${scheme}:${hex.slice(0, 6)}…${hex.slice(-6)}` : digest;
}

/**
 * "Change the image before it leaves" (spec 3.7 §8.2, board choices A7, A8): the crop can only
 * shrink and covering can only grow. The editor shows the new image, the digest the server
 * makes of it and the one it replaces; **Use this image** saves and records it, and the call
 * is held again with it, so only the new image can be sent (V-I5). Esc closes it; nothing is
 * decided here.
 */
export function ImageEditor({
  card,
  image,
  mode: initialMode,
  preview,
  use,
  onClose,
}: {
  card: DecisionCard;
  image: EvidenceImage;
  mode: "crop" | "cover";
  /** The digest and size the server makes of `change`, held nowhere. */
  preview: (change: ImageChange) => Promise<ImageChanged>;
  /** Saves and records `change` and holds the call again with it. */
  use: (change: ImageChange) => Promise<void>;
  onClose: () => void;
}) {
  const loaded = useEvidenceImage(image.href);
  const [mode, setMode] = useState(initialMode);
  const full: Box = [0, 0, image.width, image.height];
  const [crop, setCrop] = useState<Box>(full);
  const [cover, setCover] = useState<Box[]>([]);
  const [history, setHistory] = useState<{ crop: Box; cover: Box[] }[]>([]);
  const [drawing, setDrawing] = useState<Box | null>(null);
  const [made, setMade] = useState<ImageChanged | null>(null);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState("");
  const stage = useRef<HTMLDivElement>(null);
  const drag = useRef<{ edge?: Edge; start?: [number, number] } | null>(null);
  const heading = useId();
  const dialog = useRef<HTMLDivElement>(null);

  useEffect(() => dialog.current?.focus(), []);

  const cropped = crop.some((value, index) => value !== full[index]);
  const change: ImageChange | null =
    cropped || cover.length > 0
      ? { image: image.index, ...(cropped ? { crop: crop.map(Math.round) as Box } : {}), cover: cover.map((box) => box.map(Math.round) as Box) }
      : null;
  const key = JSON.stringify(change);

  // The server makes the new image as it will be sent, and says its digest.
  useEffect(() => {
    if (!change) {
      setMade(null);
      return;
    }
    let live = true;
    const timer = window.setTimeout(() => {
      preview(change)
        .then((next) => live && (setMade(next), setProblem("")))
        .catch((error: unknown) => live && setProblem(error instanceof Error ? error.message : String(error)));
    }, 250);
    return () => {
      live = false;
      window.clearTimeout(timer);
    };
    // `key` stands for `change`, which is made anew each render.
  }, [key, preview]);

  function remember() {
    setHistory((past) => [...past, { crop, cover }]);
  }

  function point(event: PointerEvent): [number, number] {
    const rect = stage.current?.getBoundingClientRect();
    if (!rect || rect.width === 0) return [0, 0];
    const x = ((event.clientX - rect.left) / rect.width) * image.width;
    const y = ((event.clientY - rect.top) / rect.height) * image.height;
    return [Math.min(Math.max(x, 0), image.width), Math.min(Math.max(y, 0), image.height)];
  }

  /** Moves one edge of the crop, never past the image or the other edge. */
  function moveEdge(edge: Edge, to: number) {
    setCrop(([x0, y0, x1, y1]) => {
      switch (edge) {
        case "left":
          return [Math.min(Math.max(to, 0), x1 - MIN), y0, x1, y1];
        case "right":
          return [x0, y0, Math.max(Math.min(to, image.width), x0 + MIN), y1];
        case "top":
          return [x0, Math.min(Math.max(to, 0), y1 - MIN), x1, y1];
        case "bottom":
          return [x0, y0, x1, Math.max(Math.min(to, image.height), y0 + MIN)];
      }
    });
  }

  function down(event: PointerEvent<HTMLDivElement>) {
    if (mode !== "cover") return;
    event.preventDefault();
    remember();
    const at = point(event);
    drag.current = { start: at };
    setDrawing([at[0], at[1], at[0], at[1]]);
    event.currentTarget.setPointerCapture?.(event.pointerId);
  }

  function moved(event: PointerEvent<HTMLDivElement>) {
    const now = drag.current;
    if (!now) return;
    const [x, y] = point(event);
    if (now.edge) {
      moveEdge(now.edge, now.edge === "left" || now.edge === "right" ? x : y);
    } else if (now.start) {
      const [sx, sy] = now.start;
      setDrawing([Math.min(sx, x), Math.min(sy, y), Math.max(sx, x), Math.max(sy, y)]);
    }
  }

  function up() {
    const now = drag.current;
    drag.current = null;
    if (now?.start && drawing) {
      // A box covers only what the crop keeps.
      const [x0, y0, x1, y1] = drawing;
      const box: Box = [Math.max(x0, crop[0]), Math.max(y0, crop[1]), Math.min(x1, crop[2]), Math.min(y1, crop[3])];
      if (box[2] - box[0] >= MIN / 2 && box[3] - box[1] >= MIN / 2) setCover((boxes) => [...boxes, box]);
      else setHistory((past) => past.slice(0, -1));
    }
    setDrawing(null);
  }

  function handleDown(edge: Edge) {
    return (event: PointerEvent<HTMLButtonElement>) => {
      event.preventDefault();
      event.stopPropagation();
      remember();
      drag.current = { edge };
      stage.current?.setPointerCapture?.(event.pointerId);
    };
  }

  function handleKey(edge: Edge) {
    return (event: KeyboardEvent<HTMLButtonElement>) => {
      const step = (edge === "left" || edge === "right" ? image.width : image.height) / 50;
      const inward = edge === "left" || edge === "top" ? step : -step;
      const value = { left: crop[0], top: crop[1], right: crop[2], bottom: crop[3] }[edge];
      if (["ArrowRight", "ArrowDown"].includes(event.key)) {
        event.preventDefault();
        remember();
        moveEdge(edge, value + Math.abs(inward));
      } else if (["ArrowLeft", "ArrowUp"].includes(event.key)) {
        event.preventDefault();
        remember();
        moveEdge(edge, value - Math.abs(inward));
      }
    };
  }

  function undo() {
    const last = history[history.length - 1];
    if (!last) return;
    setCrop(last.crop);
    setCover(last.cover);
    setHistory((past) => past.slice(0, -1));
  }

  async function useIt() {
    if (!change) return;
    setBusy(true);
    setProblem("");
    try {
      await use(change);
    } catch (error) {
      setProblem(error instanceof Error ? error.message : String(error));
    } finally {
      setBusy(false);
    }
  }

  const pct = (value: number, of: number) => `${(value / of) * 100}%`;
  const box = ([x0, y0, x1, y1]: Box) => ({
    left: pct(x0, image.width),
    top: pct(y0, image.height),
    width: pct(x1 - x0, image.width),
    height: pct(y1 - y0, image.height),
  });
  const [cx0, cy0, cx1, cy1] = crop;
  const outW = Math.round(cx1 - cx0);
  const outH = Math.round(cy1 - cy0);

  return (
    <div className="editor-backdrop" role="presentation">
      <div
        ref={dialog}
        className="editor"
        role="dialog"
        aria-modal="true"
        aria-labelledby={heading}
        tabIndex={-1}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.preventDefault();
            onClose();
          }
        }}
      >
        <header className="editor-head">
          <div>
            <h2 id={heading}>{labels.changeTheImage}</h2>
            <p className="evidence-quiet">
              {visible(card.heading)} · {visible(card.agent.displayName)}
            </p>
          </div>
          <span className="evidence-switch editor-modes" role="group" aria-label={labels.changeTheImage}>
            <button type="button" aria-pressed={mode === "crop"} onClick={() => setMode("crop")}>
              {labels.tightenTheCrop}
            </button>
            <button type="button" aria-pressed={mode === "cover"} onClick={() => setMode("cover")}>
              {labels.addARedaction}
            </button>
          </span>
          <button type="button" className="button button-secondary" onClick={undo} disabled={history.length === 0}>
            {labels.undo}
          </button>
        </header>
        <div className="editor-body">
          <div className="editor-left">
            <div
              ref={stage}
              className={`editor-stage editor-${mode}`}
              style={{ aspectRatio: `${image.width} / ${image.height}` }}
              onPointerDown={down}
              onPointerMove={moved}
              onPointerUp={up}
              onPointerCancel={up}
            >
              {loaded.state === "shown" && <img src={loaded.url} alt={visible(image.caption ?? labels.image)} draggable={false} />}
              {image.regions.map((region, index) => (
                <span
                  key={index}
                  className="evidence-region"
                  style={{ ...box([region.x0, region.y0, region.x1, region.y1]), borderColor: CLASS_COLOR[region.class] }}
                >
                  <span className="evidence-tag" style={{ background: CLASS_COLOR[region.class] }}>
                    {visible(region.tag)}
                  </span>
                </span>
              ))}
              {[...cover, ...(drawing ? [drawing] : [])].map((each, index) => (
                <span key={`c${index}`} className="evidence-region editor-cover" style={{ ...box(each), borderColor: CLASS_COLOR.marked }}>
                  {index === 0 && (
                    <span className="evidence-tag" style={{ background: CLASS_COLOR.marked }}>
                      your redaction
                    </span>
                  )}
                </span>
              ))}
              {/* What the crop cuts away is dimmed. */}
              <span className="editor-dim" style={{ left: 0, top: 0, width: "100%", height: pct(cy0, image.height) }} />
              <span className="editor-dim" style={{ left: 0, top: pct(cy1, image.height), width: "100%", bottom: 0 }} />
              <span className="editor-dim" style={{ left: 0, top: pct(cy0, image.height), width: pct(cx0, image.width), height: pct(cy1 - cy0, image.height) }} />
              <span className="editor-dim" style={{ left: pct(cx1, image.width), top: pct(cy0, image.height), right: 0, height: pct(cy1 - cy0, image.height) }} />
              <span className="editor-crop" style={box(crop)}>
                {mode === "crop" &&
                  (["left", "top", "right", "bottom"] as Edge[]).map((edge) => (
                    <button
                      key={edge}
                      type="button"
                      className={`editor-handle editor-handle-${edge}`}
                      aria-label={labels.cropHandle(edge)}
                      onPointerDown={handleDown(edge)}
                      onKeyDown={handleKey(edge)}
                    />
                  ))}
                {cropped && (
                  <span className="editor-size">
                    {outW} × {outH}
                  </span>
                )}
              </span>
            </div>
            <p className="evidence-quiet editor-hint">{labels.editorHint}</p>
          </div>
          <aside className="editor-side">
            <h3>{labels.yourChanges}</h3>
            <ul className="editor-changes">
              {!change && <li className="evidence-quiet">{labels.noChangesYet}</li>}
              {cropped && (
                <li>
                  <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false">
                    <path d="M6 2v14a2 2 0 0 0 2 2h14M18 22V8a2 2 0 0 0-2-2H2" />
                  </svg>
                  {labels.cutTo(outW, outH, `${image.width} × ${image.height}`)}
                </li>
              )}
              {cover.length > 0 && (
                <li>
                  <i style={{ background: CLASS_COLOR.marked }} aria-hidden="true" />
                  {labels.boxesCovered(cover.length)}
                </li>
              )}
            </ul>
            <h3>{labels.theNewImage}</h3>
            <div className="editor-preview" style={{ aspectRatio: `${Math.max(outW, 1)} / ${Math.max(outH, 1)}` }}>
              {loaded.state === "shown" && (
                <span
                  className="editor-preview-image"
                  style={{
                    backgroundImage: `url(${loaded.url})`,
                    backgroundSize: `${(image.width / Math.max(outW, 1)) * 100}% ${(image.height / Math.max(outH, 1)) * 100}%`,
                    backgroundPosition: `${outW === image.width ? 0 : (cx0 / (image.width - outW)) * 100}% ${outH === image.height ? 0 : (cy0 / (image.height - outH)) * 100}%`,
                  }}
                />
              )}
              {cover.map(([x0, y0, x1, y1], index) => (
                <span
                  key={index}
                  className="editor-preview-cover"
                  style={{
                    left: pct(x0 - cx0, outW),
                    top: pct(y0 - cy0, outH),
                    width: pct(x1 - x0, outW),
                    height: pct(y1 - y0, outH),
                  }}
                />
              ))}
            </div>
            <dl className="editor-digests">
              <div>
                <dt>{labels.image}</dt>
                <dd>{made ? <CopyDigest digest={made.to} short={short(made.to)} /> : <span className="evidence-quiet">{labels.newDigestPending}</span>}</dd>
              </div>
              <div>
                <dt>{labels.replaces}</dt>
                <dd>
                  <s>
                    <code>{image.digestShort}</code>
                  </s>
                </dd>
              </div>
            </dl>
            <p className="editor-note">
              <span aria-hidden="true">ⓘ</span> {labels.editorNote}
            </p>
            {problem && (
              <p className="decision-problem" role="alert">
                {problem}
              </p>
            )}
            <div className="decision-actions">
              <button type="button" className="button button-primary" disabled={!change || busy} onClick={() => void useIt()}>
                {labels.useThisImage}
              </button>
              <button type="button" className="button button-quiet" onClick={onClose}>
                {"Cancel"}
              </button>
            </div>
          </aside>
        </div>
      </div>
    </div>
  );
}
