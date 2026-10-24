/**
 * Light and dark from one set of tokens (spec 3.3 §9): the device's setting by default, or
 * the person's choice, remembered in this browser only.
 */
export type Theme = "system" | "light" | "dark";

const KEY = "adk.home.theme";

export function storedTheme(): Theme {
  try {
    const value = window.localStorage.getItem(KEY);
    return value === "light" || value === "dark" ? value : "system";
  } catch {
    return "system";
  }
}

export function applyTheme(theme: Theme) {
  const root = document.documentElement;
  if (theme === "system") delete root.dataset.theme;
  else root.dataset.theme = theme;
  try {
    if (theme === "system") window.localStorage.removeItem(KEY);
    else window.localStorage.setItem(KEY, theme);
  } catch {
    // Storage can be off (a private window); the choice then lasts for this page only.
  }
}

/** The next theme a click on the switch gives. */
export function nextTheme(theme: Theme): Theme {
  return theme === "system" ? "light" : theme === "light" ? "dark" : "system";
}
