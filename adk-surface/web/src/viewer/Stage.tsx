import { useEffect, useRef } from "react";
import type { PointerEvent as ReactPointerEvent, ReactNode } from "react";
import type { MaskView } from "../generated/api";
import type { Swatch } from "./colors";

/** A rectangle in the image's pixels: `[x, y, width, height]`. */
export type View = [number, number, number, number];
/** A box in the image's pixels: `[x0, y0, x1, y1]`. */
export type Box = [number, number, number, number];

/** A box drawn over the picture, with its tag (board choices S2, V7). */
export interface StageBox {
  key: string;
  box: Box;
  swatch: Swatch;
  tag?: string;
  tagAt?: "above" | "below" | "in";
  order?: number;
  dash?: boolean;
  selected?: boolean;
  dim?: boolean;
  handles?: boolean;
  onSelect?: () => void;
  label?: string;
}

export interface StageMask {
  key: string;
  mask: MaskView;
  swatch: Swatch;
  dashed?: boolean;
}

export interface StageProps {
  src?: string;
  width: number;
  height: number;
  /** The part of the image shown; the whole image when absent. */
  view?: View;
  /** The picture's width on screen, in CSS pixels. */
  displayWidth: number;
  boxes: StageBox[];
  masks?: StageMask[];
  /** Overlays drawn in the image's pixels: reading order, trails, the measured length. */
  svg?: ReactNode;
  /** Anything else, in screen pixels over the picture. */
  extra?: (scale: number, view: View) => ReactNode;
  failed?: boolean;
  failedText?: string;
  label: string;
  onPointerDown?: (point: [number, number], event: ReactPointerEvent<HTMLDivElement>) => void;
  onPointerMove?: (point: [number, number], event: ReactPointerEvent<HTMLDivElement>) => void;
  onPointerUp?: (point: [number, number], event: ReactPointerEvent<HTMLDivElement>) => void;
  className?: string;
}

/** A mask's set pixels, decoded from COCO run-length encoding: column-major, unset run first. */
export function maskPixels(mask: MaskView): Uint8Array {
  const out = new Uint8Array(mask.width * mask.height);
  let at = 0;
  mask.counts.forEach((count, run) => {
    if (run % 2 === 1) {
      for (let i = at; i < at + count && i < out.length; i += 1) {
        const x = Math.floor(i / mask.height);
        const y = i % mask.height;
        out[y * mask.width + x] = 1;
      }
    }
    at += count;
  });
  return out;
}

function hexRgb(hex: string): [number, number, number] {
  const value = Number.parseInt(hex.replace("#", ""), 16);
  return [(value >> 16) & 255, (value >> 8) & 255, value & 255];
}

/** Masks painted at the image's resolution: a translucent fill and a solid edge. */
function MaskCanvas({ masks, width, height }: { masks: StageMask[]; width: number; height: number }) {
  const canvas = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    const element = canvas.current;
    const context = element?.getContext?.("2d");
    if (!element || !context) return;
    context.clearRect(0, 0, width, height);
    if (masks.length === 0) return;
    const image = context.createImageData(width, height);
    for (const { mask, swatch } of masks) {
      if (mask.width !== width || mask.height !== height) continue;
      const set = maskPixels(mask);
      const [r, g, b] = hexRgb(swatch[0]);
      const edge = Math.max(2, Math.round(width / 480));
      for (let y = 0; y < height; y += 1) {
        for (let x = 0; x < width; x += 1) {
          if (!set[y * width + x]) continue;
          let border = false;
          for (let d = 1; d <= edge && !border; d += 1) {
            border =
              x - d < 0 || y - d < 0 || x + d >= width || y + d >= height ||
              !set[y * width + x - d] || !set[y * width + x + d] ||
              !set[(y - d) * width + x] || !set[(y + d) * width + x];
          }
          const at = (y * width + x) * 4;
          image.data[at] = r;
          image.data[at + 1] = g;
          image.data[at + 2] = b;
          image.data[at + 3] = border ? 235 : 110;
        }
      }
    }
    context.putImageData(image, 0, 0);
  }, [masks, width, height]);
  return <canvas ref={canvas} className="vw-masks" width={width} height={height} aria-hidden="true" />;
}

/**
 * The picture with what was found over it (board choices S1, S2): boxes and their tags in
 * screen pixels, so tags stay legible at any zoom; masks and lines in the image's pixels.
 */
export function Stage({
  src,
  width,
  height,
  view,
  displayWidth,
  boxes,
  masks = [],
  svg,
  extra,
  failed,
  failedText,
  label,
  onPointerDown,
  onPointerMove,
  onPointerUp,
  className,
}: StageProps) {
  const shown: View = view ?? [0, 0, width, height];
  const scale = shown[2] > 0 ? displayWidth / shown[2] : 1;
  const displayHeight = Math.round(shown[3] * scale);
  const placed = {
    left: `${-shown[0] * scale}px`,
    top: `${-shown[1] * scale}px`,
    width: `${width * scale}px`,
    height: `${height * scale}px`,
  };
  const pointer = (handler?: StageProps["onPointerDown"]) =>
    handler
      ? (event: ReactPointerEvent<HTMLDivElement>) => {
          const rect = event.currentTarget.getBoundingClientRect();
          handler([shown[0] + (event.clientX - rect.left) / scale, shown[1] + (event.clientY - rect.top) / scale], event);
        }
      : undefined;
  return (
    <div
      className={`vw-stage${className ? ` ${className}` : ""}`}
      style={{ width: `${displayWidth}px`, height: `${displayHeight}px` }}
      role="img"
      aria-label={label}
      onPointerDown={pointer(onPointerDown)}
      onPointerMove={pointer(onPointerMove)}
      onPointerUp={pointer(onPointerUp)}
    >
      {src && !failed && <img className="vw-picture" src={src} alt="" draggable={false} style={placed} />}
      {failed && <p className="vw-stage-failed">{failedText}</p>}
      {masks.length > 0 && width > 0 && (
        <div className="vw-layer" style={placed}>
          <MaskCanvas masks={masks} width={width} height={height} />
        </div>
      )}
      {svg && (
        <svg
          className="vw-svg"
          width={displayWidth}
          height={displayHeight}
          viewBox={shown.join(" ")}
          preserveAspectRatio="none"
          aria-hidden="true"
        >
          {svg}
        </svg>
      )}
      {boxes.map((entry) => {
        const [x0, y0, x1, y1] = entry.box;
        const [fill, ink] = entry.swatch;
        const style = {
          left: `${(x0 - shown[0]) * scale}px`,
          top: `${(y0 - shown[1]) * scale}px`,
          width: `${(x1 - x0) * scale}px`,
          height: `${(y1 - y0) * scale}px`,
          borderColor: fill,
          ...(entry.selected ? { boxShadow: `0 0 0 3px ${fill}55, 0 0 0 1px #00000055` } : {}),
        };
        const classes = ["vw-box", entry.dash && "vw-box-dash", entry.selected && "vw-box-selected", entry.dim && "vw-box-dim"]
          .filter(Boolean)
          .join(" ");
        const content = (
          <>
            {entry.tag && (
              <span className={`vw-tag vw-tag-${entry.tagAt ?? "above"}`} style={{ background: fill, color: ink }}>
                {entry.tag}
              </span>
            )}
            {entry.order !== undefined && <span className="vw-order">{entry.order}</span>}
            {entry.handles &&
              [
                [0, 0],
                [1, 0],
                [0, 1],
                [1, 1],
                [0.5, 0],
                [0.5, 1],
                [0, 0.5],
                [1, 0.5],
              ].map(([a, b]) => (
                <span
                  key={`${a}-${b}`}
                  className="vw-handle"
                  data-handle={`${a},${b}`}
                  style={{ left: `calc(${a * 100}% - 6px)`, top: `calc(${b * 100}% - 6px)`, borderColor: fill }}
                />
              ))}
          </>
        );
        return entry.onSelect ? (
          <button
            key={entry.key}
            type="button"
            className={classes}
            style={style}
            aria-label={entry.label}
            aria-pressed={entry.selected ?? false}
            onClick={entry.onSelect}
          >
            {content}
          </button>
        ) : (
          <div key={entry.key} className={classes} style={style}>
            {content}
          </div>
        );
      })}
      {extra?.(scale, shown)}
    </div>
  );
}
