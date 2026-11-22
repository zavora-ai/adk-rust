/**
 * Overlay colours of the annotated viewer (spec 3.7 §8.2): fixed per label in both themes,
 * since the picture under them does not change with the theme (board choice S1). Each is a
 * fill and the ink a tag's words take on it, at 4.5:1 or more (WCAG AA).
 */
export type Swatch = readonly [fill: string, ink: string];

const KNOWN: Record<string, Swatch> = {
  // A document's blocks.
  title: ["#2563EB", "#FFFFFF"],
  text: ["#64748B", "#FFFFFF"],
  table: ["#10B981", "#02251A"],
  seal: ["#E11D48", "#FFFFFF"],
  handwriting: ["#F59E0B", "#1A1300"],
  checkbox: ["#9333EA", "#FFFFFF"],
  formula: ["#0369A1", "#FFFFFF"],
  figure: ["#94A3B8", "#0F172A"],
  // The line 2 camera's parts.
  part: ["#22C55E", "#03210F"],
  "seal tear": ["#DC2626", "#FFFFFF"],
  tear: ["#DC2626", "#FFFFFF"],
  fiducial: ["#38BDF8", "#04202C"],
  // A warehouse aisle.
  forklift: ["#F59E0B", "#1A1300"],
  pallet: ["#22D3EE", "#032228"],
  person: ["#A78BFA", "#140B33"],
};

/** A photo's seal is yellow; a page's seal is a stamp, in red. */
const PHOTO: Record<string, Swatch> = { seal: ["#FACC15", "#231C00"] };

/** The colours the person's own correction and the comparison's marks take. */
export const CORRECTION: Swatch = ["#0369A1", "#FFFFFF"];
export const DIFFER: Swatch = ["#F59E0B", "#1A1300"];
export const ONLY_HERE: Swatch = ["#9333EA", "#FFFFFF"];
export const WAS: Swatch = ["#64748B", "#FFFFFF"];
export const ORDER_PATH = "#2563EB";

const SPARE: Swatch[] = [
  ["#E11D48", "#FFFFFF"],
  ["#14B8A6", "#022421"],
  ["#EAB308", "#231C00"],
  ["#4F46E5", "#FFFFFF"],
  ["#DB2777", "#FFFFFF"],
  ["#84CC16", "#1A2E05"],
];

/** The swatch of `label`: a known label's, else one picked by the label's letters, so a label keeps its colour. */
export function swatchOf(label: string, photo = false): Swatch {
  const base = label.replace(/ · blurred$/, "");
  if (photo && PHOTO[base]) return PHOTO[base];
  const known = KNOWN[base];
  if (known) return known;
  let hash = 0;
  for (const char of base) hash = (hash * 31 + char.charCodeAt(0)) >>> 0;
  return SPARE[hash % SPARE.length];
}
