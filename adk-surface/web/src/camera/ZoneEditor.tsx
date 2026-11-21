import { useEffect, useId, useRef, useState } from "react";
import type { PointerEvent as ReactPointerEvent } from "react";
import type { CameraView, ZoneSaved, ZoneView } from "../generated/api";
import { problemText } from "../client";
import type { Box } from "../viewer/Stage";
import { WAS } from "../viewer/colors";
import { ALL_LAYERS, CameraStage, swatchFor } from "./CameraStage";
import { InfoIcon, ZoneIcon } from "./icons";
import { cameraLabels as words } from "./labels";
import { Swatch } from "./parts";

type Mode = "move" | "draw";

function cornersText(box: Box): string {
  const r = (value: number) => Math.round(value);
  return `${r(box[0])}, ${r(box[1])} → ${r(box[2])}, ${r(box[3])}`;
}

/**
 * Draw or edit a zone (board choice C12): corner handles over the picture, the old outline
 * dashed, the watch that uses the zone, its corners in the camera's pixels. Saving changes the
 * watch from its next frame and is recorded.
 */
export function ZoneEditor({
  view,
  zone,
  width = 760,
  initialCorners,
  onSave,
  onCancel,
}: {
  view: CameraView;
  zone: ZoneView;
  width?: number;
  /** Corners already moved (fixtures). */
  initialCorners?: Box;
  onSave: (name: string, corners: Box) => Promise<ZoneSaved>;
  onCancel: () => void;
}) {
  const [mode, setMode] = useState<Mode>("move");
  const [corners, setCorners] = useState<Box>(initialCorners ?? (zone.corners as Box));
  const [name, setName] = useState(zone.name);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState("");
  const dragging = useRef<{ corner?: [number, number]; from?: [number, number] }>();
  const field = useId();
  const stage = view.stage;
  const scale = width / stage.width;
  const watch = view.watches.find((one) => one.zone === zone.name);
  const place = view.budget.scope.replace(/^for /, "");
  const changed = corners.some((value, index) => Math.round(value) !== Math.round(zone.corners[index]));

  useEffect(() => {
    const keys = (event: KeyboardEvent) => event.key === "Escape" && onCancel();
    window.addEventListener("keydown", keys);
    return () => window.removeEventListener("keydown", keys);
  }, [onCancel]);

  const clamp = (x: number, y: number): [number, number] => [
    Math.min(stage.width, Math.max(0, x)),
    Math.min(stage.height, Math.max(0, y)),
  ];
  const point = (event: ReactPointerEvent<HTMLDivElement>): [number, number] => {
    const rect = event.currentTarget.getBoundingClientRect();
    return clamp((event.clientX - rect.left) / scale, (event.clientY - rect.top) / scale);
  };
  const ordered = ([x0, y0, x1, y1]: Box): Box => [Math.min(x0, x1), Math.min(y0, y1), Math.max(x0, x1), Math.max(y0, y1)];

  function down(event: ReactPointerEvent<HTMLDivElement>) {
    const handle = (event.target as Element).closest?.("[data-corner]")?.getAttribute("data-corner");
    const at = point(event);
    if (handle) {
      const [a, b] = handle.split(",").map(Number) as [number, number];
      dragging.current = { corner: [a, b] };
    } else if (mode === "draw") {
      dragging.current = { from: at };
      setCorners([at[0], at[1], at[0], at[1]]);
    } else {
      return;
    }
    event.currentTarget.setPointerCapture?.(event.pointerId);
  }
  function move(event: ReactPointerEvent<HTMLDivElement>) {
    const drag = dragging.current;
    if (!drag) return;
    const [x, y] = point(event);
    if (drag.corner) {
      const [a, b] = drag.corner;
      setCorners((now) => {
        const next: Box = [...now];
        next[a === 0 ? 0 : 2] = x;
        next[b === 0 ? 1 : 3] = y;
        return next;
      });
    } else if (drag.from) {
      setCorners([drag.from[0], drag.from[1], x, y]);
    }
  }
  function up() {
    dragging.current = undefined;
    setCorners((now) => ordered(now));
  }

  async function save() {
    setBusy(true);
    setProblem("");
    try {
      await onSave(name, ordered(corners));
    } catch (error) {
      setProblem(problemText(error));
    } finally {
      setBusy(false);
    }
  }

  const [zx0, zy0, zx1, zy1] = corners;
  const handles: Array<[number, number]> = [
    [0, 0],
    [1, 0],
    [0, 1],
    [1, 1],
  ];
  return (
    <section className="cam-panel-card" aria-labelledby={`${field}-title`}>
      <header className="cam-panel-head">
        <h2 id={`${field}-title`}>{words.zoneTitle}</h2>
        <span className="cam-muted">{view.name}</span>
        <span className="cam-spacer" />
        <span className="cam-seg" role="group" aria-label={words.zoneTitle}>
          <button type="button" className={mode === "move" ? "cam-seg-on" : ""} aria-pressed={mode === "move"} onClick={() => setMode("move")}>
            <ZoneIcon />
            {words.moveCorners}
          </button>
          <button type="button" className={mode === "draw" ? "cam-seg-on" : ""} aria-pressed={mode === "draw"} onClick={() => setMode("draw")}>
            {words.drawNewZone}
          </button>
        </span>
      </header>
      <div className="cam-panel-body">
        <div className="cam-panel-picture">
          <CameraStage
            // The parts alone under the zone being edited, as the board draws them.
            stage={{
              ...stage,
              regions: stage.regions.filter((region) => region.track != null && !region.finding),
              zones: [],
              trails: [],
              badge: undefined,
              rate: undefined,
            }}
            width={width}
            name={view.name}
            layers={{ ...ALL_LAYERS, masks: false }}
            tags={false}
            overlay={[
              { key: "was", box: zone.corners as Box, swatch: WAS, dash: true },
              {
                key: "editing",
                box: ordered(corners),
                swatch: swatchFor("zone"),
                tag: `${name || zone.name} · ${words.editing}`,
                tagAt: "in",
                selected: true,
              },
            ]}
          >
            <div
              className={`cam-editor-layer${mode === "draw" ? " cam-editor-draw" : ""}`}
              onPointerDown={down}
              onPointerMove={move}
              onPointerUp={up}
              onPointerCancel={up}
            >
              {handles.map(([a, b]) => (
                <span
                  key={`${a},${b}`}
                  className="cam-handle"
                  data-corner={`${a},${b}`}
                  style={{
                    left: `${(a === 0 ? Math.min(zx0, zx1) : Math.max(zx0, zx1)) * scale - 7}px`,
                    top: `${(b === 0 ? Math.min(zy0, zy1) : Math.max(zy0, zy1)) * scale - 7}px`,
                  }}
                />
              ))}
            </div>
          </CameraStage>
        </div>
        <div className="cam-panel-fields">
          <label className="cam-field">
            <span className="cam-label">{words.name}</span>
            <input className="cam-input" value={name} maxLength={60} onChange={(event) => setName(event.target.value)} />
          </label>
          <div className="cam-field">
            <span className="cam-label">{words.usedBy}</span>
            {zone.usedBy.map((use) => (
              <span className="cam-used-by" key={use}>
                <Swatch label={watch?.label ?? "zone"} />
                {use} <span className="cam-muted">· {view.agent.agent.displayName}</span>
              </span>
            ))}
          </div>
          <div className="cam-field cam-field-tight">
            <span className="cam-label">{words.corners}</span>
            <span className="cam-mono">{cornersText(ordered(corners))}</span>
            <span className="cam-small cam-muted">
              {words.was} <span className="cam-mono">{zone.cornersText}</span>
            </span>
          </div>
          <div className="cam-info">
            <InfoIcon />
            <span>{words.zoneNote(place, view.agent.agent.displayName)}</span>
          </div>
          {problem && (
            <p className="cam-problem" role="alert">
              {problem}
            </p>
          )}
          <div className="cam-panel-actions">
            <button type="button" className="cam-btn cam-btn-primary cam-btn-sm" disabled={busy || !name.trim() || (!changed && name === zone.name)} onClick={() => void save()}>
              {words.saveZone}
            </button>
            <button type="button" className="cam-btn cam-btn-text cam-btn-sm" onClick={onCancel}>
              {words.cancel}
            </button>
          </div>
        </div>
      </div>
    </section>
  );
}
