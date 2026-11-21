/** The camera surface's icons, as the boards draw them. */
const common = { fill: "none", stroke: "currentColor", "aria-hidden": true, focusable: false } as const;

export const CameraIcon = ({ size = 15 }: { size?: number }) => (
  <svg width={size} height={size} viewBox="0 0 16 16" strokeWidth="1.6" {...common}>
    <rect x="1.5" y="4" width="9.5" height="8" rx="1.5" />
    <path d="M11 7l3.5-2v6L11 9" />
  </svg>
);
export const CameraOffIcon = ({ size = 22, color = "currentColor" }: { size?: number; color?: string }) => (
  <svg width={size} height={size} viewBox="0 0 16 16" strokeWidth="1.5" {...common} stroke={color}>
    <rect x="1.5" y="4" width="9.5" height="8" rx="1.5" />
    <path d="M11 7l3.5-2v6L11 9M1.5 1.5l13 13" />
  </svg>
);
export const StopIcon = () => (
  <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true" focusable="false">
    <rect x="1" y="1" width="8" height="8" rx="1.2" fill="currentColor" />
  </svg>
);
export const ZoneIcon = () => (
  <svg width="15" height="15" viewBox="0 0 16 16" strokeWidth="1.6" {...common} strokeDasharray="2 2">
    <rect x="2" y="3" width="12" height="10" rx="1" />
  </svg>
);
export const FlagIcon = () => (
  <svg width="15" height="15" viewBox="0 0 16 16" strokeWidth="1.6" {...common}>
    <path d="M3.5 14V2.5M3.5 3h8l-1.6 2.8L11.5 8.5h-8" />
  </svg>
);
export const WarnIcon = () => (
  <svg className="cam-notice-icon" width="18" height="18" viewBox="0 0 18 18" strokeWidth="1.7" {...common}>
    <path d="M9 2l7.5 13h-15z" />
    <path d="M9 7v3.5M9 12.7v.3" />
  </svg>
);
export const InfoIcon = () => (
  <svg className="cam-notice-icon" width="18" height="18" viewBox="0 0 18 18" strokeWidth="1.6" {...common}>
    <circle cx="9" cy="9" r="7.2" />
    <path d="M9 8v4.5M9 5.6v.2" />
  </svg>
);
export const CheckIcon = () => (
  <svg width="11" height="11" viewBox="0 0 12 12" strokeWidth="1.8" {...common}>
    <path d="M2 6.5l2.5 2.5L10 3" />
  </svg>
);
export const LockIcon = () => (
  <svg width="13" height="13" viewBox="0 0 16 16" strokeWidth="1.7" {...common}>
    <rect x="3" y="7" width="10" height="7" rx="1.5" />
    <path d="M5.5 7V5a2.5 2.5 0 015 0v2" />
  </svg>
);
export const ChevronIcon = ({ dir = "left" }: { dir?: "left" | "right" }) => (
  <svg width="13" height="13" viewBox="0 0 14 14" strokeWidth="1.8" {...common}>
    <path d={dir === "left" ? "M9 2L4 7l5 5" : "M5 2l5 5-5 5"} />
  </svg>
);
export const Spinner = () => (
  <svg className="cam-spinner" width="34" height="34" viewBox="0 0 16 16" fill="none" aria-hidden="true">
    <circle cx="8" cy="8" r="6" stroke="#FFFFFF" strokeOpacity="0.25" strokeWidth="2" />
    <path d="M14 8a6 6 0 00-6-6" stroke="#FFFFFF" strokeWidth="2" strokeLinecap="round" />
  </svg>
);
