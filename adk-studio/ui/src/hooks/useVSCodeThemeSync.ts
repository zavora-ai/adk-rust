/**
 * VS Code Theme Sync for ADK Studio
 *
 * Handles two mechanisms from the VS Code extension host:
 * 1. URL query parameter `?theme=` on initial load (synchronous, no flash)
 * 2. `postMessage` events with `type: 'themeChanged'` for live updates
 *
 * Maps VS Code theme kinds to our existing light/dark themes.
 * Applies VS Code CSS variables as overrides on document root.
 */

import { useEffect } from 'react';
import { useTheme } from './useTheme';
import type { ThemeMode } from '../types/theme';

/** VS Code theme kinds sent by the extension */
type VSCodeThemeKind = 'dark' | 'light' | 'high-contrast-dark' | 'high-contrast-light';

/** Map VS Code theme kind to our existing light/dark mode */
function mapVSCodeTheme(kind: string): ThemeMode {
  switch (kind) {
    case 'light':
    case 'high-contrast-light':
      return 'light';
    case 'dark':
    case 'high-contrast-dark':
      return 'dark';
    default:
      return 'dark';
  }
}

/** Apply a map of CSS variable overrides to document root */
function applyVSCodeVariables(variables: Record<string, string>) {
  const root = document.documentElement;
  for (const [name, value] of Object.entries(variables)) {
    if (value) root.style.setProperty(name, value);
  }
}

/** Clear any previously applied VS Code CSS variable overrides */
function clearVSCodeVariables() {
  const root = document.documentElement;
  // Only clear inline styles that start with --vscode-
  const style = root.style;
  const toRemove: string[] = [];
  for (let i = 0; i < style.length; i++) {
    const prop = style[i];
    if (prop.startsWith('--vscode-')) {
      toRemove.push(prop);
    }
  }
  toRemove.forEach((prop) => root.style.removeProperty(prop));
}

/**
 * Synchronously read `?theme=` from URL and apply before first render.
 * Call this ONCE in main.tsx before ReactDOM.createRoot().
 * Returns true if running inside VS Code (theme param was present).
 */
export function initVSCodeTheme(): boolean {
  const params = new URLSearchParams(window.location.search);
  const themeParam = params.get('theme');
  if (!themeParam) return false;

  const mode = mapVSCodeTheme(themeParam);
  document.documentElement.setAttribute('data-theme', mode);

  // Also pre-seed Zustand's localStorage so the store initializes with the right value
  const persistedState = { state: { mode }, version: 0 };
  localStorage.setItem('adk-studio-theme', JSON.stringify(persistedState));

  return true;
}

/**
 * Hook to listen for live VS Code theme changes via postMessage.
 * Use in App.tsx — registers the message listener on mount.
 */
export function useVSCodeThemeSync() {
  const { setMode } = useTheme();

  useEffect(() => {
    function handleMessage(event: MessageEvent) {
      const data = event.data;
      if (!data || data.type !== 'themeChanged') return;

      const themeKind: VSCodeThemeKind = data.themeKind;
      if (themeKind) {
        setMode(mapVSCodeTheme(themeKind));
      }

      const variables: Record<string, string> | undefined = data.variables;
      if (variables) {
        clearVSCodeVariables();
        applyVSCodeVariables(variables);
      }
    }

    window.addEventListener('message', handleMessage);
    return () => window.removeEventListener('message', handleMessage);
  }, [setMode]);
}
