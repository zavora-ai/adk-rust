import type { ReactNode } from "react";
import type { CameraRegion, StageView, Tone } from "../generated/api";
import { useEvidenceImage } from "../evidence/images";
import type { Swatch } from "../viewer/colors";
import { swatchOf } from "../viewer/colors";
import type { Box, StageBox } from "../viewer/Stage";
import { Stage } from "../viewer/Stage";
import { CameraOffIcon, Spinner } from "./icons";

/** The layers a person turns on and off over the picture (board choice C1). */
export interface Layers {
  boxes: boolean;
  masks: boolean;
  tracks: boolean;
  zones: boolean;
}

export const ALL_LAYERS: Layers = { boxes: true, masks: true, tracks: true, zones: true };

/** Overlay colours fixed per label in both themes (board choice S1): the zone is orange, a seal yellow. */
const ZONE: Swatch = ["#FB923C", "#1A0E00"];
const SEAL: Swatch = ["#FDE047", "#231C00"];
const TRAIL = "#22C55E";

export function swatchFor(label: string): Swatch {
  if (label === "zone") return ZONE;
  if (label === "seal") return SEAL;
  return swatchOf(label);
}

const DOT: Record<Tone, string> = {
  ok: "#22C55E",
  warn: "#F59E0B",
  fail: "#EF4444",
  muted: "#94A3B8",
  // A frozen frame's badge (board choice C11).
  plain: "#38BDF8",
};

/** "small": a tile or a phone draws short tags and no rate badge; "strip": no tags at all. */
export type StageSize = "main" | "small" | "strip";

/**
 * A camera's picture with what is drawn over it (spec 3.7 §8.2): the frame the server holds,
 * loaded with the person's own authorisation, its regions and tags in screen pixels, masks,
 * trails, zones and the measured length in the camera's pixels; the badges; and, when the
 * camera is not live, the last frame dimmed or grey under the words, or a black panel.
 */
export function CameraStage({
  stage,
  width,
  name,
  size = "main",
  layers = ALL_LAYERS,
  tags = true,
  overlay,
  children,
}: {
  stage: StageView;
  /** The picture's width on screen, in CSS pixels. */
  width: number;
  /** The camera's name, for the picture's label. */
  name: string;
  size?: StageSize;
  layers?: Layers;
  /** Tags on the boxes; a strip tile of another camera shows the picture alone. */
  tags?: boolean;
  /** Boxes drawn over the picture by an editor, in the camera's pixels. */
  overlay?: StageBox[];
  /** Anything else over the picture, in screen pixels: an editor's handles. */
  children?: ReactNode;
}) {
  const picture = useEvidenceImage(stage.look === "dark" ? undefined : stage.frame?.href);
  const height = Math.round((width * stage.height) / Math.max(1, stage.width));
  const small = size !== "main";

  if (stage.look === "dark") {
    return (
      <div
        className={`cam-stage cam-stage-dark cam-stage-${size}`}
        style={{ width: `${width}px`, height: `${height}px` }}
        role="img"
        aria-label={stage.cover?.text || name}
      >
        {stage.cover && size !== "strip" && (
          <div className="cam-dark-words">
            <CameraOffIcon size={size === "main" ? 40 : 28} />
            {stage.cover.title && <strong className="cam-cover-title">{stage.cover.title}</strong>}
            <span className="cam-cover-text">{stage.cover.text}</span>
          </div>
        )}
        {size === "strip" && <CameraOffIcon size={22} color="#94A3B8" />}
      </div>
    );
  }

  const live = stage.look === "live";
  const boxes: StageBox[] = [];
  if (live && layers.zones) {
    for (const zone of stage.zones) {
      boxes.push({
        key: `zone-${zone.id}`,
        box: zone.corners as Box,
        swatch: ZONE,
        dash: true,
        tag: size === "main" && tags ? zone.name : undefined,
        tagAt: "in",
      });
    }
  }
  if (live && layers.boxes) {
    stage.regions.forEach((region, index) => {
      if (!region.bbox) return;
      const tag = !tags ? undefined : small ? region.shortTag : region.tag;
      boxes.push({
        key: `region-${index}`,
        box: region.bbox as Box,
        swatch: swatchFor(region.label),
        tag: tag || undefined,
        tagAt: region.tagAt,
        selected: region.finding,
      });
    });
  }
  if (overlay) boxes.push(...overlay);

  const svg = live ? <Drawn stage={stage} layers={layers} /> : undefined;
  const filter = stage.look === "dimmed" ? "cam-stage-dimmed" : stage.look === "grey" ? "cam-stage-grey" : "";
  return (
    <div className={`cam-stage-wrap cam-stage-${size}`} style={{ width: `${width}px`, height: `${height}px` }}>
      <Stage
        src={picture.state === "shown" ? picture.url : undefined}
        width={stage.width}
        height={stage.height}
        displayWidth={width}
        boxes={boxes}
        svg={svg}
        label={name}
        className={`cam-stage ${filter}`}
      />
      {stage.badge && size !== "strip" && (
        <span className="cam-badge cam-badge-time">
          <i style={{ background: DOT[stage.badge.tone] }} aria-hidden="true" />
          {stage.badge.text}
        </span>
      )}
      {stage.rate && size === "main" && <span className="cam-badge cam-badge-rate">{stage.rate}</span>}
      {stage.cover && size !== "strip" && (
        <div className="cam-veil">
          {stage.cover.retrying && size === "main" && <Spinner />}
          <strong className="cam-cover-title">{stage.cover.title}</strong>
          <span className="cam-cover-text">{stage.cover.text}</span>
        </div>
      )}
      {children}
    </div>
  );
}

/** Masks, the measured length, trails and the zones' tint, in the camera's pixels. */
function Drawn({ stage, layers }: { stage: StageView; layers: Layers }) {
  const scale = stage.width / 1920;
  return (
    <>
      {layers.zones &&
        stage.zones.map((zone) => (
          <rect
            key={zone.id}
            x={zone.corners[0]}
            y={zone.corners[1]}
            width={zone.corners[2] - zone.corners[0]}
            height={zone.corners[3] - zone.corners[1]}
            fill="#FB923C"
            fillOpacity={0.06}
          />
        ))}
      {layers.tracks &&
        stage.trails.map((trail) =>
          trail.points.slice(0, 4).map(([x, y], index) => (
            <circle
              key={`${trail.track}-${index}`}
              cx={x}
              cy={y}
              r={(8 - index) * scale}
              fill={TRAIL}
              fillOpacity={0.72 - index * 0.18}
            />
          )),
        )}
      {layers.masks && stage.regions.map((region, index) => <Outline key={index} region={region} />)}
      {layers.boxes &&
        stage.regions.map((region, index) =>
          region.measurement ? <Measure key={`m-${index}`} region={region} /> : null,
        )}
    </>
  );
}

function Outline({ region }: { region: CameraRegion }) {
  const outline = region.outline ?? [];
  if (outline.length < 3) return null;
  const points = outline.map(([x, y]) => `${x},${y}`).join(" ");
  if (region.label === "seal") {
    return <polygon points={points} fill="#FDE047" fillOpacity={0.25} stroke="#FEF08A" strokeWidth={7} strokeDasharray="16 10" />;
  }
  const [fill] = swatchFor(region.label);
  return <polygon points={points} fill={fill} fillOpacity={0.55} stroke={fill} strokeWidth={3} />;
}

/** A measured length: a white line above the defect with its two ends (board choice C1). */
function Measure({ region }: { region: CameraRegion }) {
  const measured = region.measurement;
  if (!measured) return null;
  const [fx, fy] = measured.from;
  const [tx, ty] = measured.to;
  return (
    <g stroke="#FFFFFF" strokeWidth={4}>
      <line x1={fx} y1={fy - 26} x2={tx} y2={ty - 26} />
      <line x1={fx} y1={fy - 38} x2={fx} y2={fy - 14} />
      <line x1={tx} y1={ty - 38} x2={tx} y2={ty - 14} />
    </g>
  );
}
