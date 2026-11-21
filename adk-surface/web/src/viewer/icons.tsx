/** The viewer's icons, as the boards draw them. */
const common = { fill: "none", stroke: "currentColor", "aria-hidden": true, focusable: false } as const;

export const CopyIcon = () => (
  <svg width="13" height="13" viewBox="0 0 16 16" strokeWidth="1.6" {...common}>
    <rect x="5" y="5" width="9" height="9" rx="1.5" />
    <path d="M11 5V3.5A1.5 1.5 0 009.5 2h-6A1.5 1.5 0 002 3.5v6A1.5 1.5 0 003.5 11H5" />
  </svg>
);
export const DownloadIcon = () => (
  <svg width="14" height="14" viewBox="0 0 16 16" strokeWidth="1.7" {...common}>
    <path d="M8 2v8M4.5 6.5L8 10l3.5-3.5M2.5 13.5h11" />
  </svg>
);
export const CompareIcon = () => (
  <svg width="15" height="15" viewBox="0 0 16 16" strokeWidth="1.6" {...common}>
    <rect x="1.5" y="2.5" width="5.5" height="11" rx="1" />
    <rect x="9" y="2.5" width="5.5" height="11" rx="1" />
  </svg>
);
export const PenIcon = () => (
  <svg width="14" height="14" viewBox="0 0 16 16" strokeWidth="1.6" {...common}>
    <path d="M11 2.5l2.5 2.5L6 12.5 3 13l.5-3z" />
  </svg>
);
export const InfoIcon = () => (
  <svg className="vw-notice-icon" width="18" height="18" viewBox="0 0 18 18" strokeWidth="1.6" {...common}>
    <circle cx="9" cy="9" r="7.2" />
    <path d="M9 8v4.5M9 5.6v.2" />
  </svg>
);
export const WarnIcon = () => (
  <svg className="vw-notice-icon" width="18" height="18" viewBox="0 0 18 18" strokeWidth="1.7" {...common}>
    <path d="M9 2l7.5 13h-15z" />
    <path d="M9 7v3.5M9 12.7v.3" />
  </svg>
);
export const PlayIcon = () => (
  <svg width="13" height="13" viewBox="0 0 16 16" aria-hidden="true" focusable="false">
    <path d="M4 2.5v11l9-5.5z" fill="currentColor" />
  </svg>
);
export const CheckIcon = () => (
  <svg width="11" height="11" viewBox="0 0 12 12" strokeWidth="1.8" {...common}>
    <path d="M2 6.5l2.5 2.5L10 3" />
  </svg>
);
export const ChevronIcon = ({ dir = "down" }: { dir?: "down" | "left" }) => (
  <svg width={dir === "left" ? 13 : 12} height={dir === "left" ? 13 : 12} viewBox="0 0 14 14" strokeWidth="1.8" {...common}>
    <path d={dir === "left" ? "M9 2L4 7l5 5" : "M3 5l4 4 4-4"} />
  </svg>
);
