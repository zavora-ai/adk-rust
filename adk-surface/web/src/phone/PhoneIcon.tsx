const paths = {
  back: "M15 5 8 12l7 7",
  lock: "M7 11V8a5 5 0 0 1 10 0v3M6 11h12v9H6z",
} as const;

/** The phone's own glyphs, drawn like the home's. */
export function PhoneIcon({ name }: { name: keyof typeof paths }) {
  return (
    <svg className="icon" viewBox="0 0 24 24" width="18" height="18" aria-hidden="true" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <path d={paths[name]} />
    </svg>
  );
}
