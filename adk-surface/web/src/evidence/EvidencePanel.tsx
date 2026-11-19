import { useState } from "react";
import type { CardMedia, DecisionCard, EvidenceImage } from "../generated/api";
import { visible } from "../safe";
import { useEvidenceImage } from "./images";
import { evidenceLabels as labels } from "./labels";
import "./evidence.css";

/** How the egress gate's classes are drawn, the same in both themes (board choice S1). */
export const CLASS_COLOR: Record<string, string> = {
  face: "var(--cover-face)",
  personalText: "var(--cover-text)",
  schemaField: "var(--cover-schema)",
  marked: "var(--cover-marked)",
  other: "var(--cover-other)",
};

/**
 * One evidence image with its regions (spec 3.7 §8.2, board choices A2, A3, S2): the exact
 * bytes the server checked against the digest, each covered region outlined in its class's
 * colour and tagged at a fixed size whatever the picture's scale. A deleted image is a hatched
 * placeholder that says when and why.
 */
export function EvidencePicture({
  image,
  regions,
  short = false,
}: {
  image: EvidenceImage;
  regions: boolean;
  /** Tags as a small picture shows them: "face", "text", "schema". */
  short?: boolean;
}) {
  const loaded = useEvidenceImage(image.purged ? undefined : image.href);
  if (image.purged) {
    return (
      <div className="evidence-purged" role="img" aria-label={visible(image.purged.title)}>
        <span className="evidence-purged-mark" aria-hidden="true">
          i
        </span>
        <strong>{visible(image.purged.title)}</strong>
        <span>{visible(image.purged.text)}</span>
        <code>{image.digestShort}</code>
      </div>
    );
  }
  return (
    <div
      className="evidence-picture"
      style={{ aspectRatio: `${image.width} / ${image.height}` }}
      data-state={loaded.state}
    >
      {loaded.state === "shown" && <img src={loaded.url} alt={visible(image.caption ?? labels.image)} draggable={false} />}
      {loaded.state === "loading" && <span className="evidence-wait">{labels.loading}</span>}
      {loaded.state === "failed" && <span className="evidence-wait">{labels.cantShow}</span>}
      {regions &&
        image.regions.map((region, index) => (
          <span
            key={index}
            className={`evidence-region evidence-region-${region.class}`}
            style={{
              left: `${(region.x0 / image.width) * 100}%`,
              top: `${(region.y0 / image.height) * 100}%`,
              width: `${((region.x1 - region.x0) / image.width) * 100}%`,
              height: `${((region.y1 - region.y0) / image.height) * 100}%`,
              borderColor: CLASS_COLOR[region.class],
            }}
          >
            <span className="evidence-tag" style={{ background: CLASS_COLOR[region.class] }}>
              {visible(short ? region.shortTag : region.tag)}
            </span>
          </span>
        ))}
    </div>
  );
}

/** The digest in short with a copy button (board choice S4); the full digest is the title. */
export function CopyDigest({ digest, short }: { digest: string; short: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <span className="evidence-digest">
      <code title={digest}>{short}</code>
      <button
        type="button"
        className="evidence-copy"
        aria-label={copied ? labels.copied : labels.copyDigest}
        title={copied ? labels.copied : labels.copyDigest}
        onClick={() => {
          void navigator.clipboard?.writeText(digest).then(
            () => setCopied(true),
            () => setCopied(false),
          );
        }}
      >
        <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false">
          <rect x="8" y="8" width="12" height="12" rx="2" />
          <path d="M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h2" />
        </svg>
      </button>
    </span>
  );
}

/** The class colours of "Covered before it leaves", each line with its count. */
function CoveredList({ media }: { media: CardMedia }) {
  return (
    <ul className="evidence-covered">
      {media.covered.map((line) => (
        <li key={line.class}>
          <i style={{ background: CLASS_COLOR[line.class] }} aria-hidden="true" />
          <span>
            <strong>{visible(line.text)}</strong>
            {line.note && <span className="evidence-quiet"> · {visible(line.note)}</span>}
          </span>
        </li>
      ))}
      {media.coveredAllText && (
        <li className="evidence-all">
          <strong>{visible(media.coveredAllText)}</strong>
        </li>
      )}
    </ul>
  );
}

/**
 * The images of a waiting card (spec 3.7 §8.2, board choice A1): the exact image that would
 * leave on the left with its regions and the tools that change it, the facts on the right —
 * where it goes, what it is, what was covered, the estimated cost, its size and digest, what
 * it was cut from. With several images, the selected one is large over a filmstrip of each
 * crop with its field, why it escalated, what was covered and its digest (A14).
 */
export function EvidencePanel({
  media,
  onChange,
}: {
  media: CardMedia;
  /** Opens the editor on the image shown, in a mode; absent when the image cannot change. */
  onChange?: (image: EvidenceImage, mode: "crop" | "cover") => void;
}) {
  const [regions, setRegions] = useState(true);
  const [selected, setSelected] = useState(0);
  const several = media.images.length > 1;
  const image = media.images[Math.min(selected, media.images.length - 1)];
  const purged = media.images.some((each) => each.purged);
  const crop = media.whatNote;
  return (
    <section className="evidence" aria-label={visible(media.title)}>
      <div className="evidence-frame">
        <header className="evidence-bar">
          <span className="evidence-title">
            {visible(media.title)}
            {several && ` · ${labels.imageOf(selected + 1, media.images.length)}`}
          </span>
          {!purged && (
            <span className="evidence-switch" role="group" aria-label={labels.regions}>
              <button type="button" aria-pressed={regions} onClick={() => setRegions(true)}>
                {labels.regions}
              </button>
              <button type="button" aria-pressed={!regions} onClick={() => setRegions(false)}>
                {labels.imageOnly}
              </button>
            </span>
          )}
        </header>
        <div className={`evidence-stage${several ? " evidence-stage-several" : ""}`}>
          <EvidencePicture image={image} regions={regions} />
        </div>
        {several && (
          <ol className="evidence-strip">
            {media.images.map((each, index) => (
              <li key={each.digest}>
                <button
                  type="button"
                  className="evidence-strip-item"
                  aria-pressed={index === selected}
                  onClick={() => setSelected(index)}
                >
                  <EvidencePicture image={each} regions={regions} short />
                  <strong>
                    {index + 1}. {visible(each.field ?? each.sizeText)}
                  </strong>
                  <span className="evidence-quiet">
                    {[each.why, each.coveredText].filter(Boolean).map((part) => visible(part ?? "")).join(" · ")}
                  </span>
                  <code>{each.digestShort}</code>
                </button>
              </li>
            ))}
          </ol>
        )}
        {(media.sourceHref || !purged) && (
          <footer className="evidence-tools">
            {!purged && (
              <>
                <button
                  type="button"
                  className="button button-secondary evidence-tool"
                  disabled={!onChange}
                  onClick={() => onChange?.(image, "crop")}
                >
                  <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false">
                    <path d="M6 2v14a2 2 0 0 0 2 2h14M18 22V8a2 2 0 0 0-2-2H2" />
                  </svg>
                  {labels.tightenTheCrop}
                </button>
                <button
                  type="button"
                  className="button button-secondary evidence-tool"
                  disabled={!onChange}
                  onClick={() => onChange?.(image, "cover")}
                >
                  <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false">
                    <rect x="3" y="7" width="18" height="10" rx="2" />
                    <rect x="7" y="11" width="10" height="2" />
                  </svg>
                  {labels.addARedaction}
                </button>
              </>
            )}
            {media.coveredAll && <span className="evidence-quiet">{labels.coveringCantBeUndone} ·</span>}
            {media.sourceHref && (
              <a className="evidence-source" href={media.sourceHref}>
                {labels.openTheSourceImage}
              </a>
            )}
          </footer>
        )}
      </div>
      <dl className="evidence-facts">
        {media.goesTo && (
          <div>
            <dt>{labels.goesTo}</dt>
            <dd>
              {media.goesTo.provider && <>{visible(media.goesTo.provider)} · </>}
              {media.goesTo.model && <code>{visible(media.goesTo.model)}</code>}
              <br />
              <code>{visible(media.goesTo.host)}</code>
            </dd>
          </div>
        )}
        {media.what && (
          <div>
            <dt>{several ? labels.whatTheyAre : labels.whatItIs}</dt>
            <dd>
              {visible(media.what)}
              {crop && <span className="evidence-quiet"> · {visible(crop)}</span>}
            </dd>
          </div>
        )}
        {media.covered.length > 0 && (
          <div>
            <dt>{several ? labels.coveredBeforeTheyLeave : labels.coveredBeforeItLeaves}</dt>
            <dd>
              <CoveredList media={media} />
            </dd>
          </div>
        )}
        {media.costText && !purged && (
          <div>
            <dt>{labels.estimatedCost}</dt>
            <dd>
              {visible(media.costText)}
              {media.costDetail && <span className="evidence-quiet"> · {visible(media.costDetail)}</span>}
            </dd>
          </div>
        )}
        <div>
          <dt>{several ? labels.thisImage : labels.image}</dt>
          <dd>
            {image.purged ? (
              <>{labels.deleted} · </>
            ) : (
              <>
                {several && <>{labels.imageOf(selected + 1, media.images.length)} · </>}
                {visible(image.sizeText)}
                <br />
              </>
            )}
            <CopyDigest digest={image.digest} short={image.digestShort} />
            {media.replaces && (
              <span className="evidence-quiet evidence-replaces">
                {labels.replaces} <s>{visible(media.replaces.slice(0, 13))}…</s>
              </span>
            )}
          </dd>
        </div>
        {media.cutFrom && (
          <div>
            <dt>{labels.cutFrom}</dt>
            <dd>
              {visible(media.cutFrom)}
              {media.cutNote && !purged && <span className="evidence-quiet"> · {visible(media.cutNote)}</span>}
            </dd>
          </div>
        )}
      </dl>
    </section>
  );
}

/**
 * What left, on a decided card opened (board choice A11): a thumbnail of the image that left,
 * its digest, where it went and when, what was covered, the cost and how long it is kept.
 */
export function DecidedEvidence({ card, media }: { card: DecisionCard; media: CardMedia }) {
  const image = media.images[0];
  const at = card.decided?.atText;
  const covered = media.covered.map((line) => line.text).join(" · ");
  return (
    <div className="evidence-decided">
      <div className="evidence-decided-thumb">
        <EvidencePicture image={image} regions={false} short />
      </div>
      <dl className="evidence-decided-facts">
        <div>
          <dt>{labels.whatLeft}</dt>
          <dd>
            <CopyDigest digest={image.digest} short={image.digestShort} />
            <br />
            <span className="evidence-quiet">{visible(image.sizeText)}</span>
          </dd>
        </div>
        {media.goesTo && (
          <div>
            <dt>{labels.to}</dt>
            <dd>
              <code>{visible(media.goesTo.host)}</code>
              {media.goesTo.provider && <> · {visible(media.goesTo.provider)}</>}
              {media.goesTo.model && (
                <>
                  {" "}
                  <code>{visible(media.goesTo.model)}</code>
                </>
              )}
              {at && <> · at {at}</>}
            </dd>
          </div>
        )}
        {covered && (
          <div>
            <dt>{labels.covered}</dt>
            <dd>{visible(covered)}</dd>
          </div>
        )}
        {media.costText && (
          <div>
            <dt>{labels.cost}</dt>
            <dd>
              {visible(media.costText)}
              {media.costDetail && <span className="evidence-quiet"> · {visible(media.costDetail)}</span>}
            </dd>
          </div>
        )}
        {media.keptText && (
          <div>
            <dt>{labels.kept}</dt>
            <dd>
              {visible(media.keptText)} · {labels.originalNeverLeft}
            </dd>
          </div>
        )}
      </dl>
    </div>
  );
}

/** A decision's first image, small, for the Decisions list (board choice A10). */
export function EvidenceThumb({ media }: { media: CardMedia }) {
  const image = media.images[0];
  if (!image || image.purged) return null;
  return (
    <span className="evidence-thumb" aria-hidden="true">
      <EvidencePicture image={image} regions={false} short />
    </span>
  );
}
