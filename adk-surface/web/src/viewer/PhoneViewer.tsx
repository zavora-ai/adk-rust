import { useEffect, useMemo, useState } from "react";
import type { ViewerView } from "../generated/api";
import { problemText } from "../client";
import { useEvidenceImage } from "../evidence/images";
import { swatchOf } from "./colors";
import { ChevronIcon, InfoIcon } from "./icons";
import { viewerLabels as words } from "./labels";
import type { Filters } from "./parts";
import { Chain, Dot, Facts, MeasureLine, Parts, Tile, drawn, isPhoto, passes } from "./parts";
import type { ViewerPlace } from "./route";
import { Stage } from "./Stage";
import "./viewer.css";

/** The phone reads the viewer with its own token; it saves and exports nothing (board choice V15). */
export interface PhoneViewerClient {
  viewer(place: ViewerPlace): Promise<ViewerView>;
}

const STAGE_WIDTH = 358;

/**
 * The annotated viewer on a phone (spec 3.7 §8.2, board choice V15): the picture, the label
 * chips and the regions, read-only; a tapped region opens a sheet with its backend, frame,
 * transform chain and measurement. Drawn to the signed-off board `viewer-photo-phone`.
 */
export function PhoneViewer({
  client,
  place,
  onBack,
  onOpenThread,
}: {
  client: PhoneViewerClient;
  place: ViewerPlace;
  onBack: () => void;
  onOpenThread: () => void;
}) {
  const [view, setView] = useState<ViewerView>();
  const [problem, setProblem] = useState("");
  const [hiddenLabels, setHiddenLabels] = useState<Set<string>>(new Set());
  const [selected, setSelected] = useState<number>();
  const [sheet, setSheet] = useState(false);

  useEffect(() => {
    let live = true;
    client
      .viewer(place)
      .then((next) => {
        if (!live) return;
        setView(next);
        setSelected(next.answer?.selected ?? undefined);
      })
      .catch((error: unknown) => live && setProblem(problemText(error)));
    return () => {
      live = false;
    };
  }, [client, place]);

  const picture = useEvidenceImage(view?.image.href);
  const filters: Filters = useMemo(
    () => ({ hiddenLabels, hiddenTracks: new Set<number>(), minScore: view?.minScore ?? 0.5 }),
    [hiddenLabels, view?.minScore],
  );

  if (problem) {
    return (
      <main className="phone-main vw-phone">
        <p className="phone-problem" role="alert">
          {problem}
        </p>
      </main>
    );
  }
  if (!view) return <main className="phone-main vw-phone" aria-busy="true" />;

  const answer = view.answer ?? undefined;
  const photo = isPhoto(view);
  const region = answer?.regions.find((r) => r.index === selected);
  const src = picture.state === "shown" ? picture.url : undefined;
  const { boxes, masks } = answer
    ? drawn(view, answer, filters, selected, { regions: true, masks: true, order: false }, (index) => {
        setSelected(index);
        setSheet(true);
      }, true)
    : { boxes: [], masks: [] };
  // "line 2 camera, 10:41:07 · kept as evidence until 18 Dec" reads "10:41:07 · kept as …" here.
  const [first = "", ...rest] = view.meta.replace(/^\d+ × \d+ · /, "").split(" · ");
  const when = first.replace(/^.*, /, "");
  const header = (short: boolean) => (
    <header className="vw-phone-top">
      <button type="button" className="phone-back" onClick={onBack}>
        <ChevronIcon dir="left" />
        {view.agentName}
      </button>
      <h1 className="vw-phone-title">{view.title}</h1>
      <span className="vw-phone-sub">{short ? when : [when, ...rest].join(" · ")}</span>
    </header>
  );

  return (
    <main className={`phone-main vw-phone${sheet ? " vw-phone-sheeted" : ""}`}>
      {header(sheet)}
      <div className="vw-phone-picture">
        <Stage
          src={src}
          failed={picture.state === "failed"}
          failedText={words.imageFailed}
          width={view.image.width}
          height={view.image.height}
          displayWidth={STAGE_WIDTH}
          boxes={sheet && region ? boxes.filter((b) => b.key === `r${region.index}`) : boxes}
          masks={sheet && region ? masks.filter((m) => m.key === `m${region.index}`) : masks}
          svg={
            answer?.measurements?.line && !sheet ? <MeasureLine line={answer.measurements.line} /> : undefined
          }
          label={view.title}
        />
      </div>
      {answer && (
        <div className="vw-phone-chips">
          {answer.labels.map((entry) => (
            <button
              key={entry.label}
              type="button"
              className={`vw-chip${hiddenLabels.has(entry.label) ? " vw-chip-off" : ""}`}
              aria-pressed={!hiddenLabels.has(entry.label)}
              onClick={() =>
                setHiddenLabels((set) => {
                  const next = new Set(set);
                  if (next.has(entry.label)) next.delete(entry.label);
                  else next.add(entry.label);
                  return next;
                })
              }
            >
              <Dot swatch={swatchOf(entry.label, photo)} />
              {entry.label}
              <span className="vw-chip-n">{entry.count}</span>
            </button>
          ))}
        </div>
      )}
      <div className="vw-phone-rows">
        {answer?.empty && (
          <p className="vw-text">
            <Parts parts={answer.empty.text} />
          </p>
        )}
        {answer?.regions
          .filter((r) => passes(r, filters))
          .map((r) => (
            <button
              key={r.index}
              type="button"
              className={`vw-row${r.index === selected ? " vw-row-selected" : ""}`}
              aria-pressed={r.index === selected}
              onClick={() => {
                setSelected(r.index);
                setSheet(true);
              }}
            >
              <Dot swatch={swatchOf(r.label, photo)} />
              <span className="vw-row-label">{r.trackName ? `${r.trackName} ${r.label}` : r.label}</span>
              <span className="vw-row-detail">{r.detail.replace(/^(mask|box) · /, "")}</span>
              <span className="vw-row-score vw-mono">{r.scoreText}</span>
            </button>
          ))}
        <div className="vw-notice">
          <InfoIcon />
          <span>{words.readOnly}</span>
        </div>
      </div>
      {sheet && region && answer && (
        <>
          <button type="button" className="vw-phone-scrim" aria-label={words.closeSheet} onClick={() => setSheet(false)} />
          <section className="vw-phone-sheet" role="dialog" aria-label={region.heading}>
            <span className="vw-phone-grip" aria-hidden="true" />
            <span className="vw-phone-sheet-head">
              <Dot swatch={swatchOf(region.label, photo)} />
              <h2>{region.heading}</h2>
              <span className="vw-spacer" />
              <span className="vw-mono">{region.scoreText}</span>
            </span>
            {answer.measurements && answer.measurements.region === region.index && (
              <div className="vw-tiles">
                {answer.measurements.tiles.map((tile) => (
                  <Tile key={tile.label} label={tile.label} value={tile.value} uncertainty={tile.uncertainty} />
                ))}
              </div>
            )}
            <Facts facts={region.facts} />
            <span className="vw-lbl">{words.howTheBoxGotHere}</span>
            <Chain steps={region.chain} />
            <p className="vw-small2">
              {answer.run.text} · <Parts parts={answer.run.call} /> ·{" "}
              <button type="button" className="vw-link" onClick={onOpenThread}>
                {words.openInThread}
              </button>
            </p>
          </section>
        </>
      )}
    </main>
  );
}
