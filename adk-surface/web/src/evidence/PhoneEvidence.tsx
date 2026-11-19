import { useState } from "react";
import type { CardMedia, DecisionCard } from "../generated/api";
import { visible } from "../safe";
import { CLASS_COLOR, EvidencePicture } from "./EvidencePanel";
import { evidenceLabels as labels } from "./labels";

/**
 * A decision's images on the phone (spec 3.7 §8.2, board choices A14, A19): the exact image
 * that would leave with its regions tagged short, its size and digest under it, what was
 * covered, where it goes and the estimated cost. Several images swipe one at a time, with a
 * strip of each crop's field. A deleted image is a hatched placeholder. The image tools sit
 * further down, beside what the image was cut from; they open the editor in the home.
 */
export function PhoneEvidence({ card, media }: { card: DecisionCard; media: CardMedia }) {
  const [regions, setRegions] = useState(true);
  const [selected, setSelected] = useState(0);
  const several = media.images.length > 1;
  const image = media.images[Math.min(selected, media.images.length - 1)];
  const decided = card.status === "decided";
  const purged = media.images.some((each) => each.purged);
  return (
    <div className="phone-evidence">
      {image.purged ? (
        <EvidencePicture image={image} regions={false} />
      ) : (
        <figure className="phone-evidence-frame">
          <figcaption>
            <span>
              {several ? labels.swipeForTheOthers(selected + 1, media.images.length) : visible(media.title)}
            </span>
            {!decided && (
              <button type="button" className="phone-evidence-toggle" aria-pressed={regions} onClick={() => setRegions(!regions)}>
                <span aria-hidden="true">{regions ? "✓" : ""}</span> {labels.regions}
              </button>
            )}
          </figcaption>
          <div
            className="phone-evidence-stage"
            onTouchEnd={(event) => {
              // A swipe moves between several images.
              const touch = event.changedTouches[0];
              const start = Number((event.currentTarget as HTMLElement).dataset.x ?? touch.clientX);
              if (several && Math.abs(touch.clientX - start) > 40) {
                setSelected((index) => (index + (touch.clientX < start ? 1 : -1) + media.images.length) % media.images.length);
              }
            }}
            onTouchStart={(event) => {
              (event.currentTarget as HTMLElement).dataset.x = String(event.touches[0].clientX);
            }}
          >
            <EvidencePicture image={image} regions={regions && !decided} short />
            {several && (
              <span className="phone-evidence-dots" aria-hidden="true">
                {media.images.map((each, index) => (
                  <i key={each.digest} className={index === selected ? "current" : undefined} />
                ))}
              </span>
            )}
          </div>
          <p className="phone-evidence-foot">
            {several ? (
              <>
                <span>
                  {visible(image.field ?? image.sizeText)}
                  {image.why && ` · ${visible(image.why)}`}
                </span>
                <code>{image.digestShort}</code>
              </>
            ) : (
              <>
                <span>{visible(image.sizeText)}</span>
                <code>{image.digestShort}</code>
              </>
            )}
          </p>
        </figure>
      )}
      {several && (
        <ol className="phone-evidence-strip">
          {media.images.map((each, index) => (
            <li key={each.digest}>
              <button type="button" aria-pressed={index === selected} onClick={() => setSelected(index)}>
                <EvidencePicture image={each} regions={false} short />
                <strong>
                  {index + 1}. {visible(each.field ?? each.sizeText)}
                </strong>
              </button>
            </li>
          ))}
        </ol>
      )}
      {!several && !decided && media.covered.length > 0 && (
        <section className="phone-evidence-covered">
          <h2>{labels.coveredBeforeItLeaves}</h2>
          <ul>
            {media.covered.map((line) => (
              <li key={line.class}>
                <i style={{ background: CLASS_COLOR[line.class] }} aria-hidden="true" />
                {visible(line.text)}
              </li>
            ))}
          </ul>
          {media.coveredAllText && <p className="phone-evidence-all">{visible(media.coveredAllText)}</p>}
        </section>
      )}
      <dl className="phone-evidence-rows">
        {media.goesTo && (
          <div>
            <dt>{decided ? labels.sentTo : purged ? labels.wasGoingTo : labels.goesTo}</dt>
            <dd>
              {!decided && !purged && media.goesTo.provider && <>{visible(media.goesTo.provider)} · </>}
              <code>{visible(media.goesTo.host)}</code>
              {decided && card.decided?.atText && <> · {card.decided.atText}</>}
            </dd>
          </div>
        )}
        {decided && media.covered.length > 0 && (
          <div>
            <dt>{labels.covered}</dt>
            <dd>{media.covered.map((line) => visible(line.text)).join(" · ")}</dd>
          </div>
        )}
        {media.costText && !purged && (
          <div>
            <dt>{decided ? labels.cost : labels.estimatedCost}</dt>
            <dd>{visible(media.costText)}</dd>
          </div>
        )}
        {several && media.covered.length > 0 && (
          <div>
            <dt>{labels.covered}</dt>
            <dd>
              {media.covered
                .map((line) => [line.text, line.note].filter(Boolean).map((part) => visible(part ?? "")).join(" · "))
                .join(", ")}
            </dd>
          </div>
        )}
        {decided && media.keptText && (
          <div>
            <dt>{labels.kept}</dt>
            <dd>{visible(media.keptText)}</dd>
          </div>
        )}
      </dl>
      {decided && <p className="phone-footnote">{labels.originalNeverLeftPhone}</p>}
    </div>
  );
}

/**
 * What follows the call on the phone for a decision with images: the tools that change the
 * image (they open it in the home), what it was cut from, the source link and the full digest.
 */
export function PhoneEvidenceMore({ card, media, home }: { card: DecisionCard; media: CardMedia; home: string }) {
  const image = media.images[0];
  if (card.status === "decided") return null;
  return (
    <div className="phone-evidence-more">
      {media.canChange && (
        <div className="phone-evidence-tools">
          <a className="phone-button phone-button-secondary" href={home}>
            {labels.tightenTheCrop}
          </a>
          <a className="phone-button phone-button-secondary" href={home}>
            {labels.addARedaction}
          </a>
        </div>
      )}
      {(media.cutFrom || media.sourceHref) && (
        <p className="phone-note">
          {media.cutFrom && <>Cut from {visible(media.cutFrom)}</>}
          {media.cutFrom && media.sourceHref && " · "}
          {media.sourceHref && (
            <a className="phone-link-accent" href={media.sourceHref}>
              {labels.openTheSourceImage}
            </a>
          )}
        </p>
      )}
      {!image.purged && (
        <section className="phone-evidence-digest">
          <h2>{labels.image}</h2>
          <code>{image.digest}</code>
        </section>
      )}
    </div>
  );
}
