import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties, PointerEvent as ReactPointerEvent, ReactNode, RefObject } from "react";
import type {
  AnswerView,
  CompareView,
  Difference,
  ExampleScope,
  LabelledExample,
  RegionView,
  SavedExample,
  ViewerView,
} from "../generated/api";
import { ApiError, problemText } from "../client";
import { useEvidenceImage } from "../evidence/images";
import { CORRECTION, DIFFER, ONLY_HERE, WAS, swatchOf } from "./colors";
import type { Swatch } from "./colors";
import { CheckIcon, ChevronIcon, CompareIcon, CopyIcon, DownloadIcon, InfoIcon, PenIcon, PlayIcon, WarnIcon } from "./icons";
import { viewerLabels as words } from "./labels";
import type { Filters } from "./parts";
import {
  Chain,
  Dot,
  Facts,
  MeasureLine,
  Parts,
  ReadingPath,
  Section,
  Tile,
  Trails,
  around,
  baseLabel,
  boxOf,
  drawn,
  isPhoto,
  passes,
  shownText,
} from "./parts";
import type { ViewerPlace } from "./route";
import { Stage } from "./Stage";
import type { Box, StageBox, View } from "./Stage";
import "./viewer.css";

/** The routes the viewer reads and writes (spec 3.7 §8.3). */
export interface ViewerClient {
  viewer(place: ViewerPlace): Promise<ViewerView>;
  /** One answer's RegionSet, or both answers' when `other` is given (board choice V14). */
  regions(session: string, answer: string, other?: string): Promise<unknown>;
  saveExample(session: string, example: LabelledExample): Promise<SavedExample>;
}

/** A region being corrected or drawn (board choice V13). */
export interface Correction {
  index?: number;
  label: string;
  text: string;
  box: Box;
  /** The box the model gave, drawn dashed and tagged "was". */
  was?: Box;
  wasText?: string;
}

type Mode = "select" | "edit" | "draw";

interface SaveForm {
  scope: ExampleScope;
  /** The answer it is kept from, for a comparison's "Keep". */
  answer?: string;
  /** The difference it belongs to, for a comparison's "Keep". */
  difference?: number;
}

/** The fit of the picture: portrait pages at 846 px tall at most, landscape pictures to the column. */
const MAX_PAGE_HEIGHT = 846;
const PANE_WIDTH = 420;

function pct(scale: number): string {
  return `${Math.round(scale * 100)}%`;
}

function saveFile(name: string, value: unknown) {
  const blob = new Blob([JSON.stringify(value, null, 2)], { type: "application/json" });
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = name;
  document.body.append(link);
  link.click();
  link.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1_000);
}

function useWidth(fallback: number): [RefObject<HTMLDivElement>, number] {
  const ref = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(fallback);
  useEffect(() => {
    const element = ref.current;
    if (!element || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(([entry]) => {
      if (entry.contentRect.width > 0) setWidth(entry.contentRect.width);
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  return [ref, width];
}

/** A select drawn as the boards draw it: the value with a quieter note, over a native picker. */
function Picker({
  label,
  value,
  note,
  options,
  onChange,
  mono,
}: {
  label: string;
  value: string;
  note?: string;
  options: Array<{ value: string; text: string }>;
  onChange: (value: string) => void;
  mono?: boolean;
}) {
  const current = options.find((option) => option.value === value);
  return (
    <span className="vw-select">
      <span className={mono ? "vw-mono" : undefined}>
        {current?.text ?? value}
        {note && <span className="vw-muted"> · {note}</span>}
      </span>
      <ChevronIcon />
      <select aria-label={label} value={value} onChange={(event) => onChange(event.target.value)}>
        {options.map((option) => (
          <option key={option.value} value={option.value}>
            {option.text}
          </option>
        ))}
      </select>
    </span>
  );
}

function Digest({ digest, short }: { digest: string; short: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <span className="vw-digest">
      <span className="vw-mono" title={digest}>
        {short}
      </span>
      <button
        type="button"
        className="vw-icon-button"
        aria-label={copied ? words.copied : words.copyDigest}
        title={copied ? words.copied : words.copyDigest}
        onClick={() => {
          void navigator.clipboard?.writeText(digest).then(() => setCopied(true));
        }}
      >
        <CopyIcon />
      </button>
    </span>
  );
}

function Layers({ layers, on, toggle }: { layers: string[]; on: Set<string>; toggle: (layer: string) => void }) {
  return (
    <span className="vw-seg" role="group" aria-label="Layers">
      {layers.map((layer) => (
        <button key={layer} type="button" className={on.has(layer) ? "vw-on" : undefined} aria-pressed={on.has(layer)} onClick={() => toggle(layer)}>
          {on.has(layer) && <span className="vw-check"><CheckIcon /></span>}
          {layer}
        </button>
      ))}
    </span>
  );
}

function Zoom({ scale, onZoom, onFit }: { scale: number; onZoom: (factor: number) => void; onFit: () => void }) {
  return (
    <span className="vw-zoom">
      <button type="button" className="vw-btn vw-btn-xs" aria-label={words.zoomOut} onClick={() => onZoom(1 / 1.25)}>
        −
      </button>
      <span className="vw-mono vw-zoom-value" aria-live="polite">
        {pct(scale)}
      </span>
      <button type="button" className="vw-btn vw-btn-xs" aria-label={words.zoomIn} onClick={() => onZoom(1.25)}>
        +
      </button>
      <button type="button" className="vw-btn vw-btn-xs" onClick={onFit}>
        {words.fit}
      </button>
    </span>
  );
}

function Slider({ value, onChange }: { value: number; onChange: (value: number) => void }) {
  return (
    <div className="vw-slider-field">
      <span className="vw-slider-head">
        <span>{words.scoreAtLeast}</span>
        <span className="vw-mono">{value.toFixed(2)}</span>
      </span>
      <span className="vw-slider" style={{ "--at": `${value * 100}%` } as CSSProperties}>
        <input
          type="range"
          min={0}
          max={1}
          step={0.01}
          value={value}
          aria-label={words.scoreAtLeast}
          onChange={(event) => onChange(Number(event.target.value))}
        />
      </span>
    </div>
  );
}

function Chip({ swatch, label, count, off, onClick }: { swatch: Swatch; label: string; count: string; off: boolean; onClick: () => void }) {
  return (
    <button type="button" className={`vw-chip${off ? " vw-chip-off" : ""}`} aria-pressed={!off} onClick={onClick}>
      <Dot swatch={swatch} />
      {label}
      <span className="vw-chip-n">{count}</span>
    </button>
  );
}

function RegionRow({ region, photo, selected, dim, onSelect, numbered }: { region: RegionView; photo: boolean; selected: boolean; dim: boolean; onSelect: () => void; numbered: boolean }) {
  const name = region.trackName ? `${region.trackName} ${region.label}` : region.label;
  return (
    <button type="button" className={`vw-row${selected ? " vw-row-selected" : ""}${dim ? " vw-row-dim" : ""}`} aria-pressed={selected} onClick={onSelect}>
      {numbered && <span className="vw-row-n vw-mono">{region.order}</span>}
      <Dot swatch={swatchOf(region.label, photo)} />
      <span className="vw-row-label">{name}</span>
      <span className="vw-row-detail">{region.detail}</span>
      <span className="vw-row-score vw-mono">{region.scoreText}</span>
    </button>
  );
}

/** The video's timeline: when each track is seen (board choice V10). */
function Lanes({ answer, width }: { answer: AnswerView; width: number }) {
  const video = answer.video;
  if (!video) return null;
  const laneWidth = Math.max(100, width - 28 - 150);
  const x = (t: number) => (t / (video.lengthS || 1)) * laneWidth;
  return (
    <div className="vw-lanes" style={{ width: `${width}px` }}>
      <div className="vw-lanes-head">
        <button type="button" className="vw-btn vw-btn-xs" aria-label="Play" disabled>
          <PlayIcon />
        </button>
        <span className="vw-mono">
          {video.atText} / {video.lengthText}
        </span>
        <span className="vw-muted">{video.frameText}</span>
        <span className="vw-spacer" />
        <span className="vw-muted">When each track is seen</span>
      </div>
      <div className="vw-lanes-body">
        {video.tracks.map((track) => (
          <div key={track.id} className="vw-lane">
            <span className="vw-lane-name">
              <span className="vw-mono">{track.name}</span> {track.label}
            </span>
            <span className="vw-lane-bar" style={{ width: `${laneWidth}px` }}>
              {track.spans.map(([a, b]) => (
                <i key={`${a}-${b}`} style={{ left: `${x(a).toFixed(1)}px`, width: `${(x(b) - x(a)).toFixed(1)}px`, background: swatchOf(track.label)[0] }} />
              ))}
            </span>
          </div>
        ))}
        <span className="vw-lane-now" style={{ left: `${(150 + x(video.atS)).toFixed(1)}px` }} />
      </div>
    </div>
  );
}

/**
 * The annotated viewer (spec 3.7 §8.2, X12): the picture an agent saw with the regions it
 * found, each region's backend, model, frame and transform chain, measurements, and the run
 * and call that produced them; two backends side by side; a region corrected and saved as a
 * labelled example; the answer exported as its RegionSet. Built to the signed-off boards
 * (`viewer-*-web`, choices V1–V14).
 */
export function Viewer({
  client,
  place,
  onNavigate,
  onOpenThread,
  initialCorrection,
  initialMode,
  initialSave,
}: {
  client: ViewerClient;
  place: ViewerPlace;
  onNavigate: (place: ViewerPlace) => void;
  onOpenThread: (agent: string, session: string) => void;
  /** The fixture's: a correction under way. */
  initialCorrection?: Correction;
  initialMode?: Mode;
  initialSave?: SaveForm;
}) {
  const [view, setView] = useState<ViewerView>();
  const [problem, setProblem] = useState("");
  const [layersOn, setLayersOn] = useState<Set<string>>(new Set());
  const [hiddenLabels, setHiddenLabels] = useState<Set<string>>(new Set());
  const [hiddenTracks, setHiddenTracks] = useState<Set<number>>(new Set());
  const [minScore, setMinScore] = useState(0.5);
  const [selected, setSelected] = useState<number>();
  const [zoom, setZoom] = useState<number>();
  const [correction, setCorrection] = useState<Correction | undefined>(initialCorrection);
  const [mode, setMode] = useState<Mode>(initialMode ?? (initialCorrection ? "edit" : "select"));
  const [save, setSave] = useState<SaveForm | undefined>(initialSave ?? (initialCorrection ? { scope: "region" } : undefined));
  const [file, setFile] = useState("");
  const [newFile, setNewFile] = useState("");
  const [caseName, setCaseName] = useState("");
  const [saved, setSaved] = useState("");
  const [busy, setBusy] = useState(false);
  const [overlaid, setOverlaid] = useState(false);
  const [noOther, setNoOther] = useState(false);
  const [canvasRef, canvasWidth] = useWidth(952);
  const drag = useRef<{ kind: "move" | "resize" | "draw"; handle?: [number, number]; from: [number, number]; box: Box } | undefined>(undefined);

  useEffect(() => {
    let live = true;
    setProblem("");
    client
      .viewer(place)
      .then((next) => {
        if (!live) return;
        setView(next);
        const kind = next.kind;
        setLayersOn(
          new Set(
            initialCorrection
              ? [words.regions]
              : kind === "document"
              ? [words.regions, words.readingOrder]
              : kind === "videoFrame"
                ? [words.regions, words.trails]
                : [words.regions, words.masks, words.measurements],
          ),
        );
        setMinScore(next.minScore);
        setSelected(next.answer?.selected ?? undefined);
        setFile((current) => current || next.examples.file);
        setCaseName((current) => current || next.examples.caseName);
      })
      .catch((error: unknown) => live && setProblem(error instanceof ApiError || error instanceof Error ? problemText(error) : words.couldNotLoad));
    return () => {
      live = false;
    };
  }, [client, place]);

  const picture = useEvidenceImage(view?.image.href);
  const answer = view?.answer ?? undefined;
  const compare = view?.compare ?? undefined;
  const filters: Filters = useMemo(() => ({ hiddenLabels, hiddenTracks, minScore }), [hiddenLabels, hiddenTracks, minScore]);
  const toggle = useCallback((layer: string) => {
    setLayersOn((current) => {
      const next = new Set(current);
      if (next.has(layer)) next.delete(layer);
      else next.add(layer);
      return next;
    });
  }, []);

  if (problem) {
    return (
      <div className="vw vw-problem" role="alert">
        {problem}
      </div>
    );
  }
  if (!view) return <div className="vw vw-loading" aria-busy="true" />;

  const photo = isPhoto(view);
  const document = view.kind === "document";
  const correcting = correction !== undefined;
  const regionOf = (index: number | undefined) => answer?.regions.find((region) => region.index === index);
  const current = regionOf(selected);
  const otherAnswers = view.answers.filter((choice) => choice.id !== answer?.id);

  // ---------------------------------------------------------------- the picture's fit
  // The canvas column's content width: its padding is already left out.
  const available = Math.max(320, canvasWidth);
  const fit = compare
    ? Math.min(PANE_WIDTH, (available - 60) / 2) / view.image.width
    : view.image.height > view.image.width
      ? Math.min((available - 32) / view.image.width, MAX_PAGE_HEIGHT / view.image.height)
      : Math.min((available - 32) / view.image.width, 1);
  const scale = correcting ? (zoom ?? 1.5) : (zoom ?? fit);
  let stageView: View | undefined;
  let displayWidth = Math.round(view.image.width * scale);
  if (correcting && correction) {
    const width = Math.min(view.image.width, (available - 40) / scale);
    const height = Math.min(view.image.height, (MAX_PAGE_HEIGHT * 0.85) / scale);
    const [x0, y0] = correction.box;
    stageView = [
      Math.min(Math.max(0, x0 - width * 0.45), Math.max(0, view.image.width - width)),
      Math.min(Math.max(0, y0 - height * 0.41), Math.max(0, view.image.height - height)),
      width,
      height,
    ];
    displayWidth = Math.round(width * scale);
  } else if (displayWidth > available) {
    // Zoomed past the column: show the part around the selected region.
    const width = available / scale;
    const height = Math.min(view.image.height, 860 / scale);
    stageView = current ? around(boxOf(current), width, height, view.image.width, view.image.height) : [0, 0, width, height];
    displayWidth = Math.round(available);
  }

  // ---------------------------------------------------------------- actions
  const choose = (id: string) => onNavigate({ session: place.session, image: place.image, answer: id });
  const startCompare = () => {
    const other = otherAnswers[0];
    if (!other || !answer) {
      setNoOther(true);
      return;
    }
    onNavigate({ session: place.session, image: place.image, answer: answer.id, compare: other.id });
  };
  const exportSets = async () => {
    if (!answer) return;
    try {
      const sets = await client.regions(view.sessionId, answer.id, compare?.b.answer.id);
      const name = compare ? `${answer.id}-${compare.b.answer.id}.regions.json` : `${answer.id}.regions.json`;
      saveFile(name, sets);
    } catch (error) {
      setProblem(problemText(error));
    }
  };
  const correct = (region: RegionView) => {
    // The page around the region, its other blocks quiet (board choice V13).
    setLayersOn(new Set([words.regions]));
    setCorrection({
      index: region.index,
      label: baseLabel(region),
      text: region.text ?? "",
      box: boxOf(region),
      was: boxOf(region),
      wasText: region.escalation?.readAs ?? region.text ?? undefined,
    });
    setMode("edit");
    setSave({ scope: "region" });
    setSaved("");
  };
  const drawMissing = () => {
    const w = Math.round(view.image.width / 6);
    const h = Math.round(view.image.height / 6);
    const x = Math.round(view.image.width / 2 - w / 2);
    const y = Math.round(view.image.height / 2 - h / 2);
    const asked = answer?.empty?.text.find((part) => part.style === "strong")?.text.replace(/[“”]/g, "");
    setCorrection({ label: asked ?? "region", text: "", box: [x, y, x + w, y + h] });
    setMode("draw");
    setSave({ scope: "region" });
    setSaved("");
  };
  const cancel = () => {
    setCorrection(undefined);
    setSave(undefined);
    setMode("select");
    setZoom(undefined);
  };
  const submit = async () => {
    if (!answer || !save) return;
    const target = file === "__new__" ? newFile.trim() : file;
    let region: LabelledExample["region"];
    if (save.scope !== "nothing") {
      if (correction) {
        region = {
          index: correction.index ?? null,
          label: correction.label,
          text: correction.text || null,
          box: correction.box.map((v) => Math.round(v * 10) / 10),
        };
      } else if (save.difference !== undefined && compare) {
        const difference = compare.differences[save.difference];
        const kept = save.answer === compare.b.answer.id ? compare.b.answer : compare.a.answer;
        const index = save.answer === compare.b.answer.id ? difference.bRegion : difference.aRegion;
        const keptRegion = kept.regions.find((r) => r.index === index);
        if (keptRegion) region = { index: keptRegion.index, label: baseLabel(keptRegion), text: keptRegion.text ?? null, box: boxOf(keptRegion) };
      } else if (current) {
        region = { index: current.index, label: baseLabel(current), text: current.text ?? null, box: boxOf(current) };
      }
    }
    setBusy(true);
    try {
      const result = await client.saveExample(view.sessionId, {
        answer: save.answer ?? answer.id,
        file: target,
        caseName: caseName.trim(),
        scope: save.scope,
        region,
      });
      setSaved(result.text);
      setCorrection(undefined);
      setSave(undefined);
      setMode("select");
      setZoom(undefined);
    } catch (error) {
      setSaved(problemText(error));
    } finally {
      setBusy(false);
    }
  };

  // ---------------------------------------------------------------- editing the box
  const onPointerDown = (point: [number, number], event: ReactPointerEvent<HTMLDivElement>) => {
    if (!correction || mode === "select") return;
    const handle = (event.target as HTMLElement).dataset?.handle;
    const [x0, y0, x1, y1] = correction.box;
    if (handle) {
      const [a, b] = handle.split(",").map(Number) as [number, number];
      drag.current = { kind: "resize", handle: [a, b], from: point, box: correction.box };
    } else if (mode === "draw") {
      drag.current = { kind: "draw", from: point, box: [point[0], point[1], point[0], point[1]] };
    } else if (point[0] >= x0 && point[0] <= x1 && point[1] >= y0 && point[1] <= y1) {
      drag.current = { kind: "move", from: point, box: correction.box };
    } else {
      return;
    }
    event.currentTarget.setPointerCapture?.(event.pointerId);
    event.preventDefault();
  };
  const onPointerMove = (point: [number, number]) => {
    const active = drag.current;
    if (!active || !correction) return;
    const clampX = (v: number) => Math.min(Math.max(0, v), view.image.width);
    const clampY = (v: number) => Math.min(Math.max(0, v), view.image.height);
    const dx = point[0] - active.from[0];
    const dy = point[1] - active.from[1];
    let [x0, y0, x1, y1] = active.box;
    if (active.kind === "move") {
      const w = x1 - x0;
      const h = y1 - y0;
      x0 = Math.min(Math.max(0, x0 + dx), view.image.width - w);
      y0 = Math.min(Math.max(0, y0 + dy), view.image.height - h);
      x1 = x0 + w;
      y1 = y0 + h;
    } else if (active.kind === "resize" && active.handle) {
      const [a, b] = active.handle;
      if (a === 0) x0 = clampX(x0 + dx);
      if (a === 1) x1 = clampX(x1 + dx);
      if (b === 0) y0 = clampY(y0 + dy);
      if (b === 1) y1 = clampY(y1 + dy);
    } else {
      [x0, x1] = [Math.min(active.from[0], clampX(point[0])), Math.max(active.from[0], clampX(point[0]))];
      [y0, y1] = [Math.min(active.from[1], clampY(point[1])), Math.max(active.from[1], clampY(point[1]))];
    }
    if (x1 - x0 >= 2 && y1 - y0 >= 2) {
      setCorrection({ ...correction, box: [Math.round(x0), Math.round(y0), Math.round(x1), Math.round(y1)] });
    }
  };
  const onPointerUp = () => {
    drag.current = undefined;
  };

  // ---------------------------------------------------------------- the picture
  const layerRegions = layersOn.has(words.regions);
  const layerMasks = layersOn.has(words.masks);
  const layerOrder = layersOn.has(words.readingOrder);
  let stage: ReactNode;
  let below: ReactNode = null;
  const failed = picture.state === "failed";
  const src = picture.state === "shown" ? picture.url : undefined;

  if (compare && answer) {
    const pane = (side: "a" | "b") => {
      const one = compare[side];
      const width = Math.round(view.image.width * scale);
      const marks = new Map(one.marks.map((mark) => [mark.index, mark]));
      const boxes: StageBox[] = one.answer.regions.map((region) => {
        const mark = marks.get(region.index);
        const swatch = mark?.agreement === "differ" ? DIFFER : mark?.agreement === "onlyHere" ? ONLY_HERE : swatchOf(region.label);
        return {
          key: `${side}${region.index}`,
          box: boxOf(region),
          swatch,
          dash: mark?.agreement !== "agree",
          tag: mark?.tag ?? undefined,
          tagAt: mark?.agreement === "onlyHere" ? "above" : "below",
          order: region.order ?? undefined,
        };
      });
      if (overlaid && side === "a") {
        for (const region of compare.b.answer.regions) {
          const mark = compare.b.marks.find((m) => m.index === region.index);
          if (mark?.agreement === "agree") continue;
          boxes.push({ key: `b${region.index}`, box: boxOf(region), swatch: mark?.agreement === "onlyHere" ? ONLY_HERE : DIFFER, dash: true, tag: mark?.tag ?? undefined, tagAt: "below" });
        }
      }
      return (
        <div className="vw-pane" style={{ width: `${width}px` }}>
          <div className="vw-pane-head">
            <span className="vw-pane-name">
              {one.answer.name}
              <span className="vw-muted"> · {one.answer.place}</span>
            </span>
            <span className="vw-pane-stats">
              {one.answer.stats.map((stat) => (
                <span key={stat}>{stat}</span>
              ))}
            </span>
          </div>
          <Stage
            src={src}
            failed={failed}
            failedText={words.imageFailed}
            width={view.image.width}
            height={view.image.height}
            displayWidth={width}
            boxes={boxes}
            label={`${view.title}, ${one.answer.name}`}
          />
        </div>
      );
    };
    stage = (
      <div className="vw-panes">
        {pane("a")}
        {!overlaid && pane("b")}
      </div>
    );
  } else {
    const { boxes, masks } = answer
      ? drawn(view, answer, filters, correcting ? -1 : selected, { regions: layerRegions, masks: layerMasks, order: layerOrder || correcting }, correcting ? undefined : setSelected)
      : { boxes: [], masks: [] };
    let shownBoxes = boxes;
    if (correcting && correction) {
      // The page around the region, the other blocks quiet; the person's box with handles.
      shownBoxes = boxes
        .filter((entry) => entry.key !== `r${correction.index}`)
        .map((entry) => ({ ...entry, tag: undefined, onSelect: undefined }));
      shownBoxes.push({
        key: "correction",
        box: correction.box,
        swatch: CORRECTION,
        tag: words.yourCorrection(correction.label),
        tagAt: "above",
        selected: true,
        handles: mode !== "select",
      });
    }
    const shown = answer?.regions.filter((region) => passes(region, filters)) ?? [];
    const svg = (
      <>
        {answer && document && layerOrder && !correcting && <ReadingPath regions={shown} />}
        {answer?.video && layersOn.has(words.trails) && <Trails tracks={answer.video.tracks} regions={shown} />}
        {answer?.measurements?.line && layersOn.has(words.measurements) && <MeasureLine line={answer.measurements.line} />}
      </>
    );
    stage = (
      <Stage
        src={src}
        failed={failed}
        failedText={words.imageFailed}
        width={view.image.width}
        height={view.image.height}
        view={stageView}
        displayWidth={displayWidth}
        boxes={shownBoxes}
        masks={correcting ? [] : masks}
        svg={svg}
        label={view.title}
        className={correcting ? `vw-stage-edit vw-stage-${mode}` : undefined}
        onPointerDown={correcting ? onPointerDown : undefined}
        onPointerMove={correcting ? onPointerMove : undefined}
        onPointerUp={correcting ? onPointerUp : undefined}
        extra={(s, v) =>
          correction?.was ? (
            <div
              className="vw-box vw-box-dash vw-box-was"
              style={{
                left: `${(correction.was[0] - v[0]) * s}px`,
                top: `${(correction.was[1] - v[1]) * s}px`,
                width: `${(correction.was[2] - correction.was[0]) * s}px`,
                height: `${(correction.was[3] - correction.was[1]) * s}px`,
                borderColor: WAS[0],
              }}
            >
              <span className="vw-tag vw-tag-was" style={{ background: WAS[0], color: WAS[1] }}>
                {words.wasTag}
              </span>
            </div>
          ) : null
        }
      />
    );
    if (correcting && correction) {
      below = (
        <p className="vw-caption">
          {mode === "draw" ? words.drawingHint : words.correctingHint(correction.index !== undefined ? String(regionOf(correction.index)?.order ?? correction.index + 1) : "")}
        </p>
      );
    } else if (answer?.video) {
      below = <Lanes answer={answer} width={displayWidth} />;
    } else if (photo && current && answer?.measurements && layersOn.has(words.measurements)) {
      const inset = around(boxOf(current), 240, 150, view.image.width, view.image.height);
      const insetBoxes: StageBox[] = [{ key: "inset", box: boxOf(current), swatch: swatchOf(current.label, true), tag: current.measurement ?? current.label, tagAt: "below", selected: true }];
      const insetMasks = current.mask ? [{ key: "inset-mask", mask: current.mask, swatch: swatchOf(current.label, true) }] : [];
      below = (
        <div className="vw-below">
          <figure className="vw-inset">
            <Stage
              src={src}
              width={view.image.width}
              height={view.image.height}
              view={inset}
              displayWidth={360}
              boxes={insetBoxes}
              masks={layerMasks ? [...(answer.regions.filter((r) => r.mask && r.index !== current.index).map((r) => ({ key: `i${r.index}`, mask: r.mask!, swatch: swatchOf(r.label, true), dashed: true }))), ...insetMasks] : []}
              svg={answer.measurements.line && answer.measurements.region === current.index ? <MeasureLine line={answer.measurements.line} /> : undefined}
              label={words.zoomedAround}
            />
            <figcaption>{words.zoomedAround}</figcaption>
          </figure>
          {view.caption && <p className="vw-caption vw-caption-side">{view.caption}</p>}
        </div>
      );
    } else if (view.caption) {
      below = <p className="vw-caption">{view.caption}</p>;
    }
  }

  // ---------------------------------------------------------------- the canvas toolbar
  const layerNames = document
    ? [words.regions, words.readingOrder, words.masks]
    : view.kind === "videoFrame"
      ? [words.regions, words.trails, words.masks]
      : answer?.measurements
        ? [words.regions, words.masks, words.measurements]
        : [words.regions, words.masks];
  const toolbar = compare ? (
    <>
      <span className="vw-legend">
        <span>
          <i style={{ borderTop: `2px dashed ${DIFFER[0]}` }} />
          {words.readDifferently}
        </span>
        <span>
          <i style={{ borderTop: `2px dashed ${ONLY_HERE[0]}` }} />
          {words.onlyOneFound}
        </span>
        <span>
          <i style={{ borderTop: `2px solid ${swatchOf("text")[0]}` }} />
          {words.agree}
        </span>
      </span>
      <span className="vw-spacer" />
      <span className="vw-seg" role="group" aria-label="Arrangement">
        <button type="button" className={!overlaid ? "vw-on" : undefined} aria-pressed={!overlaid} onClick={() => setOverlaid(false)}>
          {words.sideBySide}
        </button>
        <button type="button" className={overlaid ? "vw-on" : undefined} aria-pressed={overlaid} onClick={() => setOverlaid(true)}>
          {words.overlaid}
        </button>
      </span>
      <Zoom scale={scale} onZoom={(f) => setZoom(scale * f)} onFit={() => setZoom(undefined)} />
    </>
  ) : (
    <>
      <Layers layers={layerNames} on={layersOn} toggle={toggle} />
      {correcting && (
        <span className="vw-seg" role="group" aria-label="Tool">
          <button type="button" className={mode === "select" ? "vw-on" : undefined} aria-pressed={mode === "select"} onClick={() => setMode("select")}>
            {words.select}
          </button>
          <button type="button" className={mode === "edit" ? "vw-on" : undefined} aria-pressed={mode === "edit"} onClick={() => setMode("edit")}>
            <PenIcon /> {words.editBox}
          </button>
          <button type="button" className={mode === "draw" ? "vw-on" : undefined} aria-pressed={mode === "draw"} onClick={() => setMode("draw")}>
            {words.drawRegion}
          </button>
        </span>
      )}
      <span className="vw-spacer" />
      <Zoom scale={scale} onZoom={(f) => setZoom(scale * f)} onFit={() => setZoom(undefined)} />
    </>
  );

  // ---------------------------------------------------------------- the panel
  const saveForm = (
    <SaveFields
      view={view}
      save={save}
      file={file}
      newFile={newFile}
      caseName={caseName}
      busy={busy}
      onFile={setFile}
      onNewFile={setNewFile}
      onCaseName={setCaseName}
      onScope={(scope) => save && setSave({ ...save, scope })}
      onSave={() => void submit()}
      onCancel={cancel}
    />
  );
  let panel: ReactNode;
  if (compare) {
    panel = <ComparePanel compare={compare} answers={view.answers} save={save} saveForm={saveForm} onPick={(a, b) => onNavigate({ ...place, answer: a, compare: b })} onKeep={(difference, answerId) => {
      setSave({ scope: "region", answer: answerId, difference });
      setCaseName(`${view.examples.caseName.split("-").slice(0, 3).join("-")}-${compare.differences[difference].number}`);
      setSaved("");
    }} saved={saved} />;
  } else if (correcting && correction) {
    const was = correction.index !== undefined ? regionOf(correction.index) : undefined;
    const labelsKnown = [...new Set([correction.label, ...(answer?.labels.map((l) => l.label) ?? [])])];
    panel = (
      <>
        <Section title={was ? words.correctRegion(String(was.order ?? was.index + 1)) : words.newRegion}>
          <label className="vw-field">
            <span className="vw-lbl">{words.label}</span>
            <Picker label={words.label} value={correction.label} options={labelsKnown.map((l) => ({ value: l, text: l }))} onChange={(label) => setCorrection({ ...correction, label })} />
          </label>
          {(document || correction.text) && (
            <label className="vw-field">
              <span className="vw-lbl">{words.text}</span>
              <input className="vw-input vw-mono" value={correction.text} onChange={(event) => setCorrection({ ...correction, text: event.target.value })} />
              {correction.wasText && correction.wasText !== correction.text && <span className="vw-note">{words.was("")}<span className="vw-mono">{correction.wasText}</span></span>}
            </label>
          )}
          <div className="vw-field">
            <span className="vw-lbl">{words.box}</span>
            <span className="vw-mono vw-box-text">{boxText(correction.box)}</span>
            {correction.was && boxText(correction.was) !== boxText(correction.box) && (
              <span className="vw-note">
                {words.was("")}
                <span className="vw-mono">{boxText(correction.was)}</span>
              </span>
            )}
          </div>
        </Section>
        <Section title={words.saveAsExample}>{saveForm}</Section>
      </>
    );
  } else if (answer) {
    panel = (
      <AnswerPanel
        view={view}
        answer={answer}
        filters={filters}
        selected={current}
        onSelect={setSelected}
        onChoose={choose}
        onCompare={startCompare}
        noOther={noOther}
        onLabel={(label) => setHiddenLabels((set) => toggled(set, label))}
        onTrack={(track) => setHiddenTracks((set) => toggled(set, track))}
        onScore={setMinScore}
        onCorrect={correct}
        onSave={(scope) => {
          setSave({ scope });
          setSaved("");
        }}
        onDraw={drawMissing}
        onClearFilters={() => {
          setHiddenLabels(new Set());
          setHiddenTracks(new Set());
          setMinScore(0);
        }}
        onOpenThread={() => onOpenThread(answer.run.agent, view.sessionId)}
        save={save}
        saveForm={saveForm}
        saved={saved}
      />
    );
  } else {
    panel = (
      <Section title={words.notAnsweredYet}>
        <p className="vw-text">{words.notAnsweredText}</p>
      </Section>
    );
  }

  const title = compare ? `${view.title} · two answers` : view.title;
  const meta = correcting && correction
    ? `${view.image.width} × ${view.image.height} · ${words.correctingMeta(correction.index !== undefined ? String(regionOf(correction.index)?.order ?? correction.index + 1) : "new")}`
    : compare
      ? view.meta.replace(/ at \d+ dpi$/, "")
      : view.meta;
  return (
    <div className="vw">
      <div className="vw-subbar">
        <button type="button" className="vw-back" onClick={() => onOpenThread(view.agent || answer?.run.agent || "", view.sessionId)}>
          <ChevronIcon dir="left" />
          {view.agentName}
        </button>
        <span className="vw-rule" aria-hidden="true" />
        <h1 className="vw-title">{title}</h1>
        <span className="vw-meta">{meta}</span>
        <Digest digest={view.image.digest} short={view.image.digestShort} />
        <span className="vw-spacer" />
        {compare ? (
          <>
            <button type="button" className="vw-btn vw-btn-sm" onClick={() => void exportSets()}>
              <DownloadIcon />
              {words.exportBoth}
            </button>
            <button type="button" className="vw-btn vw-btn-sm" onClick={() => onNavigate({ ...place, compare: undefined })}>
              {words.close}
            </button>
          </>
        ) : (
          <>
            <button type="button" className="vw-btn vw-btn-sm" onClick={startCompare} disabled={!answer}>
              <CompareIcon />
              {words.compareBackends}
            </button>
            <button type="button" className="vw-btn vw-btn-sm" onClick={() => void exportSets()} disabled={!answer}>
              <DownloadIcon />
              {words.exportRegionSet}
            </button>
          </>
        )}
      </div>
      <div className="vw-body">
        <div className="vw-canvas" ref={canvasRef}>
          <div className="vw-tools">{toolbar}</div>
          {stage}
          {below}
        </div>
        <aside className="vw-panel" aria-label="Answer">
          {panel}
        </aside>
      </div>
    </div>
  );
}

function toggled<T>(set: Set<T>, value: T): Set<T> {
  const next = new Set(set);
  if (next.has(value)) next.delete(value);
  else next.add(value);
  return next;
}

function boxText(box: Box): string {
  const r = (v: number) => Math.round(v);
  return `${r(box[0])}, ${r(box[1])} → ${r(box[2])}, ${r(box[3])}`;
}

/** Where a labelled example goes (board choice V13). */
function SaveFields({
  view,
  save,
  file,
  newFile,
  caseName,
  busy,
  onFile,
  onNewFile,
  onCaseName,
  onScope,
  onSave,
  onCancel,
}: {
  view: ViewerView;
  save: SaveForm | undefined;
  file: string;
  newFile: string;
  caseName: string;
  busy: boolean;
  onFile: (file: string) => void;
  onNewFile: (file: string) => void;
  onCaseName: (name: string) => void;
  onScope: (scope: ExampleScope) => void;
  onSave: () => void;
  onCancel: () => void;
}) {
  if (!view.examples.canSave) return <p className="vw-note">{words.cantSave}</p>;
  const files = [...new Set([...view.examples.files, view.examples.file].filter(Boolean))];
  const options = [...files.map((f) => ({ value: f, text: f })), { value: "__new__", text: words.newFile }];
  return (
    <div className="vw-save">
      <label className="vw-field">
        <span className="vw-lbl">{words.evalFixtures}</span>
        <Picker label={words.evalFixtures} value={file || files[0] || "__new__"} options={options} onChange={onFile} mono />
      </label>
      {file === "__new__" && (
        <label className="vw-field">
          <span className="vw-lbl">{words.newFileName}</span>
          <input className="vw-input vw-mono" value={newFile} placeholder="evals/vision/examples.test.json" onChange={(event) => onNewFile(event.target.value)} />
        </label>
      )}
      <label className="vw-field">
        <span className="vw-lbl">{words.caseName}</span>
        <input className="vw-input" value={caseName} onChange={(event) => onCaseName(event.target.value)} />
      </label>
      {save?.scope !== "nothing" && save?.difference === undefined && (
        <div className="vw-radios" role="radiogroup" aria-label="What the example holds">
          {(["region", "page"] as const).map((scope) => (
            <label key={scope} className="vw-radio">
              <input type="radio" name="scope" checked={save?.scope === scope} onChange={() => onScope(scope)} />
              <span className="vw-rad" aria-hidden="true" />
              {scope === "region" ? words.thisRegionOnly : words.everyRegion}
            </label>
          ))}
        </div>
      )}
      <div className="vw-notice">
        <InfoIcon />
        <span>{(save?.scope === "nothing" ? words.nothingNote : words.digestNote)(view.image.digestShort)}</span>
      </div>
      <div className="vw-actions">
        <button type="button" className="vw-btn vw-btn-sm vw-btn-primary" disabled={busy || !caseName.trim()} onClick={onSave}>
          {words.saveExample}
        </button>
        <button type="button" className="vw-btn vw-btn-sm vw-btn-text" onClick={onCancel}>
          {words.cancel}
        </button>
      </div>
    </div>
  );
}

/** The panel of one answer: where it came from, the filters, the regions and the one looked at (board choice V2). */
function AnswerPanel({
  view,
  answer,
  filters,
  selected,
  onSelect,
  onChoose,
  onCompare,
  noOther,
  onLabel,
  onTrack,
  onScore,
  onCorrect,
  onSave,
  onDraw,
  onClearFilters,
  onOpenThread,
  save,
  saveForm,
  saved,
}: {
  view: ViewerView;
  answer: AnswerView;
  filters: Filters;
  selected: RegionView | undefined;
  onSelect: (index: number) => void;
  onChoose: (id: string) => void;
  onCompare: () => void;
  noOther: boolean;
  onLabel: (label: string) => void;
  onTrack: (track: number) => void;
  onScore: (score: number) => void;
  onCorrect: (region: RegionView) => void;
  onSave: (scope: ExampleScope) => void;
  onDraw: () => void;
  onClearFilters: () => void;
  onOpenThread: () => void;
  save: SaveForm | undefined;
  saveForm: ReactNode;
  saved: string;
}) {
  const photo = isPhoto(view);
  const document = view.kind === "document";
  const runCall = (
    <>
      <dl className="vw-kv">
        <div className="vw-kv-row">
          <dt>{words.run}</dt>
          <dd>{answer.run.text}</dd>
        </div>
        <div className="vw-kv-row">
          <dt>{words.call}</dt>
          <dd>
            <Parts parts={answer.run.call} />
          </dd>
        </div>
      </dl>
      <button type="button" className="vw-link" onClick={onOpenThread}>
        {words.openInThread}
      </button>
    </>
  );
  const from = (
    <Section title={words.answerFrom}>
      <Picker
        label={words.answerFrom}
        value={answer.id}
        note={answer.place}
        options={view.answers.map((choice) => ({ value: choice.id, text: choice.name }))}
        onChange={onChoose}
      />
      {view.answers.length > 1 && (
        <span className="vw-small vw-muted">
          {document ? words.answersForPage(view.answers.length) : words.answersForImage(view.answers.length)} ·{" "}
          <button type="button" className="vw-link" onClick={onCompare}>
            {words.compare}
          </button>
        </span>
      )}
      {noOther && <p className="vw-note">{words.nothingToCompare}</p>}
    </Section>
  );
  if (answer.empty) {
    const empty = answer.empty;
    return (
      <>
        {from}
        <div className="vw-empty">
          <h2>{empty.title}</h2>
          <p className="vw-empty-text">
            <Parts parts={empty.text} />
          </p>
          <Facts facts={empty.facts} />
          <div className="vw-actions vw-actions-wrap">
            <button type="button" className="vw-btn vw-btn-sm" onClick={onCompare}>
              <CompareIcon />
              {words.compareWithAnother}
            </button>
            <button type="button" className="vw-btn vw-btn-sm" onClick={onDraw}>
              <PenIcon />
              {words.drawMissing}
            </button>
          </div>
          <span>
            <button type="button" className="vw-btn vw-btn-sm" onClick={() => onSave("nothing")}>
              {words.saveNothing}
            </button>
          </span>
          {save?.scope === "nothing" && saveForm}
          {saved && <p className="vw-note" role="status">{saved}</p>}
        </div>
        <Section title={words.notTheSameAs}>
          <div className="vw-notice">
            <InfoIcon />
            <span>
              <Parts parts={empty.hidden} />
              <button type="button" className="vw-link" onClick={onClearFilters}>
                {words.clearFilters}
              </button>
            </span>
          </div>
          <div className="vw-notice vw-notice-failed">
            <WarnIcon />
            <span>
              <Parts parts={empty.failed} />
            </span>
          </div>
        </Section>
      </>
    );
  }
  const video = answer.video;
  const listed = answer.regions;
  return (
    <>
      {from}
      <Section title={words.show}>
        {video && <span className="vw-lbl">{words.tracks}</span>}
        {video && (
          <div className="vw-chips">
            {video.tracks.map((track) => (
              <Chip key={track.id} swatch={swatchOf(track.label)} label={`${track.name} ${track.label.replace(/ · blurred$/, "")}`} count={track.framesText} off={filters.hiddenTracks.has(track.id)} onClick={() => onTrack(track.id)} />
            ))}
          </div>
        )}
        {video && <span className="vw-lbl">{words.labelsHeading}</span>}
        <div className="vw-chips">
          {answer.labels.map((entry) => (
            <Chip key={entry.label} swatch={swatchOf(entry.label, photo)} label={entry.label} count={String(entry.count)} off={filters.hiddenLabels.has(entry.label)} onClick={() => onLabel(entry.label)} />
          ))}
        </div>
        <Slider value={filters.minScore} onChange={onScore} />
        {!video && <span className="vw-small vw-muted">{shownText(answer, filters)}</span>}
      </Section>
      <Section title={video ? words.inThisFrame : words.regions} sub={document ? words.inReadingOrder : undefined}>
        <div className="vw-rows">
          {listed.map((region) => (
            <RegionRow
              key={region.index}
              region={region}
              photo={photo}
              numbered={document}
              selected={region.index === selected?.index}
              dim={region.score != null && region.score < filters.minScore - 1e-9}
              onSelect={() => onSelect(region.index)}
            />
          ))}
        </div>
      </Section>
      {answer.measurements && (
        <Section title={words.measurements}>
          <div className="vw-tiles">
            {answer.measurements.tiles.map((tile) => (
              <Tile key={tile.label} label={tile.label} value={tile.value} uncertainty={tile.uncertainty} />
            ))}
          </div>
          <p className="vw-text vw-small2">{answer.measurements.calibration}</p>
        </Section>
      )}
      {selected && (
        <Section title={selected.heading}>
          <Facts facts={selected.facts} />
          {document && <span className="vw-lbl">{words.howTheBoxGotHere}</span>}
          <Chain steps={selected.chain} />
          {video && <p className="vw-small vw-muted">{video.note}</p>}
          {runCall}
          {!video && (
            <div className="vw-actions">
              <button type="button" className="vw-btn vw-btn-sm" onClick={() => onCorrect(selected)}>
                <PenIcon />
                {words.correctThisRegion}
              </button>
              <button type="button" className="vw-btn vw-btn-sm" onClick={() => onSave("region")}>
                {words.saveAsExample}
              </button>
            </div>
          )}
          {save && save.difference === undefined && save.scope !== "nothing" && saveForm}
          {saved && <p className="vw-note" role="status">{saved}</p>}
        </Section>
      )}
    </>
  );
}

/** Two answers for one image (board choice V11). */
function ComparePanel({
  compare,
  answers,
  save,
  saveForm,
  saved,
  onPick,
  onKeep,
}: {
  compare: CompareView;
  answers: ViewerView["answers"];
  save: SaveForm | undefined;
  saveForm: ReactNode;
  saved: string;
  onPick: (a: string, b: string) => void;
  onKeep: (difference: number, answer: string) => void;
}) {
  const options = answers.map((choice) => ({ value: choice.id, text: choice.name }));
  const named = (prefix: string, id: string) => options.map((o) => ({ ...o, text: o.value === id ? `${prefix} · ${o.text}` : o.text }));
  return (
    <>
      <Section title={words.comparing}>
        <Picker label="A" value={compare.a.answer.id} options={named("A", compare.a.answer.id)} onChange={(id) => onPick(id, compare.b.answer.id)} />
        <Picker label="B" value={compare.b.answer.id} options={named("B", compare.b.answer.id)} onChange={(id) => onPick(compare.a.answer.id, id)} />
        <span className="vw-small vw-muted">{words.bothForImage}</span>
      </Section>
      <Section title={words.howTheyAgree}>
        <div className="vw-tiles">
          {(
            [
              [compare.agree, words.agree],
              [compare.differ, words.readDifferently],
              [compare.onlyOne, words.onlyOneFound],
            ] as const
          ).map(([n, label]) => (
            <div key={label} className="vw-tile">
              <span className="vw-tile-value">{n}</span>
              <span className="vw-tile-label vw-tile-label-below">{label}</span>
            </div>
          ))}
        </div>
        <p className="vw-text vw-small2">{compare.overlapText}</p>
      </Section>
      <Section title={words.differences}>
        {compare.differences.map((difference: Difference, at) => (
          <div key={`${difference.number}-${difference.title}`} className="vw-diff">
            <span className="vw-diff-head">
              <span className="vw-mono vw-muted">{difference.number}</span>
              <Dot swatch={difference.aRegion == null || difference.bRegion == null ? ONLY_HERE : swatchOf(difference.label)} />
              <span className="vw-diff-title">{difference.title}</span>
            </span>
            <div className="vw-diff-grid">
              <span className="vw-muted">{compare.a.shortName}</span>
              <span className="vw-mono">{difference.a}</span>
              <span className="vw-muted">{compare.b.shortName}</span>
              <span className="vw-mono">{difference.b}</span>
            </div>
            {difference.aRegion != null && difference.bRegion != null && (
              <div className="vw-actions">
                <button type="button" className="vw-btn vw-btn-xs" onClick={() => onKeep(at, compare.a.answer.id)}>
                  {words.keep(compare.a.shortName)}
                </button>
                <button type="button" className="vw-btn vw-btn-xs" onClick={() => onKeep(at, compare.b.answer.id)}>
                  {words.keep(compare.b.shortName)}
                </button>
              </div>
            )}
            {save?.difference === at && saveForm}
          </div>
        ))}
        <p className="vw-text vw-small2">{words.keepingSaves}</p>
        {saved && <p className="vw-note" role="status">{saved}</p>}
      </Section>
    </>
  );
}
