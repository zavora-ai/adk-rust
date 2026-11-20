/** Pieces the web and phone viewers share (spec 3.7 §8.2). */
import type { ReactNode } from "react";
import type { AnswerView, Fact, FactPart, RegionView, TrackView, ViewerView } from "../generated/api";
import { ORDER_PATH, swatchOf } from "./colors";
import type { Swatch } from "./colors";
import type { Box, StageBox, StageMask, View } from "./Stage";
import { viewerLabels as words } from "./labels";

export function boxOf(region: Pick<RegionView, "x0" | "y0" | "x1" | "y1">): Box {
  return [region.x0, region.y0, region.x1, region.y1];
}

/** Parts of a fact, each in its own style. */
export function Parts({ parts }: { parts: FactPart[] }) {
  return (
    <>
      {parts.map((part, index) => {
        switch (part.style) {
          case "mono":
            return (
              <span key={index} className="vw-mono">
                {part.text}
              </span>
            );
          case "muted":
            return (
              <span key={index} className="vw-muted">
                {part.text}
              </span>
            );
          case "strong":
            return <strong key={index}>{part.text}</strong>;
          case "plain":
            return <span key={index}>{part.text}</span>;
        }
      })}
    </>
  );
}

/** Labelled lines (board choices V4–V6). */
export function Facts({ facts }: { facts: Fact[] }) {
  return (
    <dl className="vw-kv">
      {facts.map((fact) => (
        <div key={fact.label} className="vw-kv-row">
          <dt>{fact.label}</dt>
          <dd>
            <Parts parts={fact.parts} />
          </dd>
        </div>
      ))}
    </dl>
  );
}

/** The transform chain as steps (board choice V4). */
export function Chain({ steps }: { steps: string[] }) {
  return (
    <ol className="vw-chain">
      {steps.map((step) => (
        <li key={step}>
          <i aria-hidden="true" />
          <span>{step}</span>
        </li>
      ))}
    </ol>
  );
}

export function Dot({ swatch }: { swatch: Swatch }) {
  return <span className="vw-dot" style={{ background: swatch[0] }} aria-hidden="true" />;
}

/** What the filters let through (board choice V3). */
export interface Filters {
  hiddenLabels: Set<string>;
  hiddenTracks: Set<number>;
  minScore: number;
}

export function baseLabel(region: RegionView): string {
  return region.label.replace(/ · blurred$/, "");
}

export function passes(region: RegionView, filters: Filters): boolean {
  if (filters.hiddenLabels.has(baseLabel(region))) return false;
  if (region.track != null && filters.hiddenTracks.has(region.track)) return false;
  return region.score == null || region.score >= filters.minScore - 1e-9;
}

export function isPhoto(view: ViewerView): boolean {
  return view.kind === "photo";
}

/** The rectangle of `width` × `height` image pixels centred on `box`, kept inside the image. */
export function around(box: Box, width: number, height: number, imageWidth: number, imageHeight: number): View {
  const cx = (box[0] + box[2]) / 2;
  const cy = (box[1] + box[3]) / 2;
  const x = Math.min(Math.max(0, cx - width / 2), Math.max(0, imageWidth - width));
  const y = Math.min(Math.max(0, cy - height / 2), Math.max(0, imageHeight - height));
  return [x, y, width, height];
}

/** Where a box's tag goes: below when a neighbour sits just above it, inside at the picture's edge. */
function tagPlace(region: RegionView, others: RegionView[], selected: boolean, kind: ViewerView["kind"]): StageBox["tagAt"] {
  if (selected && kind !== "videoFrame") return "below";
  if (region.atEdge || region.detail.endsWith("calibration")) return "in";
  const crowded = others.some(
    (other) =>
      other !== region &&
      other.y1 <= region.y0 + 4 &&
      other.y1 >= region.y0 - 70 &&
      other.x0 < region.x1 &&
      other.x1 > region.x0,
  );
  return crowded ? "below" : "above";
}

/** The boxes and masks one answer draws, filtered and with the boards' tag rules. */
export function drawn(
  view: ViewerView,
  answer: AnswerView,
  filters: Filters,
  selected: number | undefined,
  layers: { regions: boolean; masks: boolean; order: boolean },
  onSelect?: (index: number) => void,
  small = false,
): { boxes: StageBox[]; masks: StageMask[] } {
  const photo = isPhoto(view);
  const shown = answer.regions.filter((region) => passes(region, filters));
  const boxes: StageBox[] = [];
  const masks: StageMask[] = [];
  for (const region of shown) {
    const swatch = swatchOf(region.label, photo);
    const isSelected = region.index === selected;
    if (region.mask && layers.masks) {
      masks.push({ key: `m${region.index}`, mask: region.mask, swatch, dashed: !isSelected });
    }
    // A mask's box is drawn when it is the region looked at (board choice V8).
    if (!layers.regions || (region.mask && !isSelected)) continue;
    let tag: string | undefined;
    if (view.kind === "document") tag = isSelected ? region.tag : undefined;
    else if (small) tag = isSelected ? [region.label, region.measurement].filter(Boolean).join(" · ") : undefined;
    else tag = region.tag;
    boxes.push({
      key: `r${region.index}`,
      box: boxOf(region),
      swatch,
      tag,
      tagAt: tagPlace(region, shown, isSelected, view.kind),
      order: layers.order && region.order != null ? region.order : undefined,
      selected: isSelected,
      label: `${region.heading}${region.scoreText ? `, ${region.scoreText}` : ""}`,
      onSelect: onSelect ? () => onSelect(region.index) : undefined,
    });
  }
  return { boxes, masks };
}

/** The dashed reading-order path through the centres of the shown blocks (board choice V7). */
export function ReadingPath({ regions }: { regions: RegionView[] }) {
  const points = [...regions]
    .filter((region) => region.order != null)
    .sort((a, b) => (a.order ?? 0) - (b.order ?? 0))
    .map((region) => `${(region.x0 + region.x1) / 2},${(region.y0 + region.y1) / 2}`)
    .join(" ");
  return (
    <polyline points={points} fill="none" stroke={ORDER_PATH} strokeWidth={5} strokeDasharray="18 12" strokeOpacity={0.8} />
  );
}

/** Each track's trail up to its box (board choice V10). */
export function Trails({ tracks, regions }: { tracks: TrackView[]; regions: RegionView[] }) {
  return (
    <>
      {tracks.map((track) => {
        const region = regions.find((r) => r.track === track.id);
        if (!region || track.trail.length === 0) return null;
        const [fill] = swatchOf(track.label);
        const end = `${(region.x0 + region.x1) / 2},${region.y1 - 10}`;
        const points = [...track.trail.map((p) => p.join(",")), end].join(" ");
        return (
          <g key={track.id}>
            <polyline points={points} fill="none" stroke={fill} strokeWidth={8} strokeDasharray="4 14" strokeLinecap="round" />
            {track.trail.map(([x, y]) => (
              <circle key={`${x}-${y}`} cx={x} cy={y} r={9} fill={fill} />
            ))}
          </g>
        );
      })}
    </>
  );
}

/** The longest length measured, with its end caps (board choice V9). */
export function MeasureLine({ line }: { line: number[][] }) {
  const [[x0, y0], [x1, y1]] = line;
  const length = Math.hypot(x1 - x0, y1 - y0) || 1;
  const [nx, ny] = [(-(y1 - y0) / length) * 12, ((x1 - x0) / length) * 12];
  return (
    <g stroke="#FFFFFF" strokeWidth={4}>
      <line x1={x0} y1={y0} x2={x1} y2={y1} />
      <line x1={x0 - nx} y1={y0 - ny} x2={x0 + nx} y2={y0 + ny} />
      <line x1={x1 - nx} y1={y1 - ny} x2={x1 + nx} y2={y1 + ny} />
    </g>
  );
}

/** A section of the panel. */
export function Section({ title, sub, children }: { title: ReactNode; sub?: string; children: ReactNode }) {
  return (
    <section className="vw-sec">
      <h3>
        {title}
        {sub && <span>{sub}</span>}
      </h3>
      {children}
    </section>
  );
}

/** A measurement tile (board choice V9). */
export function Tile({ label, value, uncertainty }: { label: string; value: string; uncertainty: string }) {
  return (
    <div className="vw-tile">
      <span className="vw-tile-label">{label}</span>
      <span className="vw-tile-value">{value}</span>
      <span className="vw-tile-note">{uncertainty}</span>
    </div>
  );
}

/** "10 shown of 11 · 1 below 0.50". */
export function shownText(answer: AnswerView, filters: Filters): string {
  const all = answer.regions.length;
  const shown = answer.regions.filter((region) => passes(region, filters)).length;
  const below = answer.regions.filter((region) => region.score != null && region.score < filters.minScore - 1e-9).length;
  const parts = [words.shownOf(shown, all)];
  if (below > 0) parts.push(words.belowScore(below, filters.minScore.toFixed(2)));
  return parts.join(" · ");
}
