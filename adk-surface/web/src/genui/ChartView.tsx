import type { DrawnChart } from "../generated/api";
import { visible } from "../safe";

/**
 * A chart a component of an extension catalog drew (spec 3.3 §6.1 `genui`, extension catalogs),
 * as the home draws it: an SVG in the home's chart colours (`--chart-1` … `--chart-8`), from
 * numbers and words the server already checked. Its accessible name is the sentence the server
 * wrote for it; each point names its value on hover. No links, no markup, no animation.
 */
export function ChartView({ chart }: { chart: DrawnChart }) {
  const summary = visible(chart.summary);
  return (
    <figure className="drawn-chart" data-kind={chart.kind}>
      {chart.kind === "sparkline" ? (
        <Sparkline chart={chart} summary={summary} />
      ) : chart.kind === "pie" ? (
        <Pie chart={chart} summary={summary} />
      ) : (
        <Axes chart={chart} summary={summary} />
      )}
      {chart.kind !== "sparkline" && legend(chart)}
    </figure>
  );
}

const WIDTH = 640;
const HEIGHT = 240;
const PAD = { left: 52, right: 16, top: 12, bottom: 34 };
const MAX_X_LABELS = 12;

const colour = (index: number) => `var(--chart-${(index % 8) + 1})`;

/** A number as an axis reads it: `950`, `1.2k`, `3.4M`. */
export function compact(value: number): string {
  const abs = Math.abs(value);
  const trim = (n: number) => String(Number(n.toFixed(1)));
  if (abs >= 1e9) return `${trim(value / 1e9)}B`;
  if (abs >= 1e6) return `${trim(value / 1e6)}M`;
  if (abs >= 1e4) return `${trim(value / 1e3)}k`;
  return String(Number(value.toFixed(2)));
}

/** Four or five round ticks over `[low, high]`. */
export function niceTicks(low: number, high: number): number[] {
  if (!Number.isFinite(low) || !Number.isFinite(high)) return [0];
  if (low === high) high = low + 1;
  const raw = (high - low) / 4;
  const magnitude = 10 ** Math.floor(Math.log10(raw));
  const norm = raw / magnitude;
  const step = (norm < 1.5 ? 1 : norm < 3 ? 2 : norm < 7 ? 5 : 10) * magnitude;
  // The ticks span every value: the last is at or above `high`.
  const first = Math.floor(low / step);
  const last = Math.ceil(high / step - 1e-9);
  const ticks: number[] = [];
  for (let n = first; n <= last; n += 1) {
    ticks.push(Number((n * step).toFixed(10)));
  }
  return ticks;
}

function legend(chart: DrawnChart) {
  const entries =
    chart.kind === "pie" && chart.x.type === "category"
      ? chart.x.labels.map((label, index) => ({ label, value: chart.series[0]?.y[index] }))
      : chart.series.length > 1
        ? chart.series.map((series) => ({ label: series.name, value: undefined }))
        : [];
  if (entries.length === 0) return null;
  return (
    <ul className="drawn-chart-legend">
      {entries.map((entry, index) => (
        <li key={index}>
          <span className="drawn-chart-swatch" style={{ background: colour(index) }} aria-hidden="true" />
          {visible(entry.label)}
          {entry.value !== undefined && <span className="drawn-chart-value"> {compact(entry.value)}</span>}
        </li>
      ))}
    </ul>
  );
}

function Axes({ chart, summary }: { chart: DrawnChart; summary: string }) {
  const plotW = WIDTH - PAD.left - PAD.right;
  const plotH = HEIGHT - PAD.top - PAD.bottom;
  const values = chart.series.flatMap((series) => series.y);
  const yTicks = niceTicks(Math.min(0, ...values), Math.max(0, ...values));
  const yLow = yTicks[0];
  const yHigh = yTicks[yTicks.length - 1];
  const y = (value: number) => PAD.top + plotH - ((value - yLow) / (yHigh - yLow || 1)) * plotH;

  const category = chart.x.type === "category" ? chart.x.labels : undefined;
  const xs = chart.series.flatMap((series) => series.x ?? []);
  const xLow = xs.length ? Math.min(...xs) : 0;
  const xHigh = xs.length ? Math.max(...xs) : 1;
  const band = category ? plotW / Math.max(category.length, 1) : 0;
  const x = (value: number, index: number) =>
    category
      ? PAD.left + band * index + band / 2
      : PAD.left + ((value - xLow) / (xHigh - xLow || 1)) * plotW;

  const xLabels: { at: number; text: string }[] = category
    ? category
        .map((label, index) => ({ at: x(0, index), text: label }))
        .filter((_, index, all) => all.length <= MAX_X_LABELS || index % Math.ceil(all.length / MAX_X_LABELS) === 0)
    : chart.ticks && chart.ticks.length > 0
      ? chart.ticks.map((tick) => ({ at: x(tick.at, 0), text: tick.text }))
      : niceTicks(xLow, xHigh)
          .filter((tick) => tick >= xLow && tick <= xHigh)
          .map((tick) => ({ at: x(tick, 0), text: compact(tick) }));

  const groups = chart.series.length;
  const barWidth = (band * 0.72) / Math.max(groups, 1);
  return (
    <svg className="drawn-chart-plot" viewBox={`0 0 ${WIDTH} ${HEIGHT}`} role="img" aria-label={summary}>
      {yTicks.map((tick) => (
        <g key={`y${tick}`}>
          <line className="drawn-chart-grid" x1={PAD.left} x2={WIDTH - PAD.right} y1={y(tick)} y2={y(tick)} />
          <text className="drawn-chart-tick" x={PAD.left - 8} y={y(tick)} textAnchor="end" dominantBaseline="middle">
            {compact(tick)}
          </text>
        </g>
      ))}
      {xLabels.map((label, index) => (
        <text key={`x${index}`} className="drawn-chart-tick" x={label.at} y={HEIGHT - PAD.bottom + 18} textAnchor="middle">
          {visible(label.text)}
        </text>
      ))}
      {chart.series.map((series, s) => {
        const points = series.y.map((value, index) => ({
          px: x(series.x?.[index] ?? 0, index),
          py: y(value),
          value,
          label: category ? category[index] : series.x?.[index] !== undefined ? compact(series.x[index]) : "",
        }));
        const name = visible(series.name);
        const tip = (label: string, value: number) => `${name ? `${name} · ` : ""}${visible(label)}: ${compact(value)}`;
        if (chart.kind === "bar") {
          return (
            <g key={s} fill={colour(s)}>
              {points.map((point, index) => {
                const left = PAD.left + band * index + band * 0.14 + barWidth * s;
                const top = Math.min(point.py, y(0));
                return (
                  <rect key={index} x={left} y={top} width={Math.max(barWidth - 1, 1)} height={Math.abs(y(0) - point.py)}>
                    <title>{tip(point.label, point.value)}</title>
                  </rect>
                );
              })}
            </g>
          );
        }
        const path = points.map((point) => `${point.px},${point.py}`).join(" ");
        return (
          <g key={s} stroke={colour(s)} fill={colour(s)}>
            {chart.kind === "area" && points.length > 1 && (
              <polygon
                className="drawn-chart-area"
                points={`${points[0].px},${y(Math.max(yLow, 0))} ${path} ${points[points.length - 1].px},${y(Math.max(yLow, 0))}`}
              />
            )}
            {chart.kind !== "scatter" && <polyline className="drawn-chart-line" points={path} fill="none" />}
            {points.map((point, index) => (
              <circle key={index} cx={point.px} cy={point.py} r={chart.kind === "scatter" ? 3.5 : 2.5}>
                <title>{tip(point.label, point.value)}</title>
              </circle>
            ))}
          </g>
        );
      })}
      {chart.xLabel && (
        <text className="drawn-chart-axis" x={PAD.left + plotW / 2} y={HEIGHT - 2} textAnchor="middle">
          {visible(chart.xLabel)}
        </text>
      )}
      {chart.yLabel && (
        <text className="drawn-chart-axis" x={12} y={PAD.top + plotH / 2} textAnchor="middle" transform={`rotate(-90 12 ${PAD.top + plotH / 2})`}>
          {visible(chart.yLabel)}
        </text>
      )}
    </svg>
  );
}

function Pie({ chart, summary }: { chart: DrawnChart; summary: string }) {
  const values = chart.series[0]?.y ?? [];
  const labels = chart.x.type === "category" ? chart.x.labels : [];
  const total = values.reduce((sum, value) => sum + Math.max(value, 0), 0) || 1;
  const radius = 90;
  const centre = { x: 120, y: 110 };
  let angle = -Math.PI / 2;
  return (
    <svg className="drawn-chart-plot drawn-chart-pie" viewBox="0 0 240 220" role="img" aria-label={summary}>
      {values.map((value, index) => {
        const sweep = (Math.max(value, 0) / total) * Math.PI * 2;
        const start = angle;
        angle += sweep;
        const tip = `${visible(labels[index] ?? "")}: ${compact(value)}`;
        if (sweep >= Math.PI * 2 - 1e-9) {
          return (
            <circle key={index} cx={centre.x} cy={centre.y} r={radius} fill={colour(index)}>
              <title>{tip}</title>
            </circle>
          );
        }
        const point = (at: number) => `${centre.x + radius * Math.cos(at)} ${centre.y + radius * Math.sin(at)}`;
        const large = sweep > Math.PI ? 1 : 0;
        return (
          <path key={index} d={`M ${centre.x} ${centre.y} L ${point(start)} A ${radius} ${radius} 0 ${large} 1 ${point(angle)} Z`} fill={colour(index)}>
            <title>{tip}</title>
          </path>
        );
      })}
    </svg>
  );
}

function Sparkline({ chart, summary }: { chart: DrawnChart; summary: string }) {
  const values = chart.series[0]?.y ?? [];
  const width = 120;
  const height = 28;
  const low = Math.min(...values);
  const high = Math.max(...values);
  const points = values
    .map((value, index) => {
      const px = values.length > 1 ? (index / (values.length - 1)) * (width - 4) + 2 : width / 2;
      const py = high > low ? height - 3 - ((value - low) / (high - low)) * (height - 6) : height / 2;
      return `${px},${py}`;
    })
    .join(" ");
  return (
    <svg className="drawn-chart-spark" viewBox={`0 0 ${width} ${height}`} role="img" aria-label={summary}>
      <polyline className="drawn-chart-line" points={points} fill="none" stroke={colour(0)} />
    </svg>
  );
}
