/**
 * Global/App-level settings that apply to all projects.
 * These are stored in localStorage and serve as defaults for new projects.
 */
export interface GlobalSettings {
  // Default provider/model for new projects
  defaultProvider: string;
  defaultModel: string;
  
  // Code generation defaults
  adkVersion: string;
  rustEdition: '2021' | '2024';
  
  // UI defaults
  theme: 'light' | 'dark' | 'system';
  showMinimap: boolean;
  showTimeline: boolean;
  showDataFlowOverlay: boolean;
  
  // Build defaults
  autobuildEnabled: boolean;
  autobuildTriggers: {
    onAgentAdd: boolean;
    onAgentDelete: boolean;
    onAgentUpdate: boolean;
    onToolAdd: boolean;
    onToolUpdate: boolean;
    onEdgeAdd: boolean;
    onEdgeDelete: boolean;
  };
  
  // Layout defaults
  layoutMode: 'free' | 'fixed';
  layoutDirection: 'TB' | 'LR' | 'BT' | 'RL';
}

export const DEFAULT_GLOBAL_SETTINGS: GlobalSettings = {
  defaultProvider: 'gemini',
  defaultModel: 'gemini-2.5-flash',
  adkVersion: '0.3.0',
  rustEdition: '2024',
  theme: 'dark',
  showMinimap: true,
  showTimeline: true,
  showDataFlowOverlay: false,
  autobuildEnabled: true,
  autobuildTriggers: {
    onAgentAdd: true,
    onAgentDelete: true,
    onAgentUpdate: true,
    onToolAdd: true,
    onToolUpdate: true,
    onEdgeAdd: true,
    onEdgeDelete: true,
  },
  layoutMode: 'free',
  layoutDirection: 'LR',  // Default to Left-to-Right
};

const GLOBAL_SETTINGS_KEY = 'adk-studio-global-settings';

export function loadGlobalSettings(): GlobalSettings {
  try {
    const stored = localStorage.getItem(GLOBAL_SETTINGS_KEY);
    if (stored) {
      return { ...DEFAULT_GLOBAL_SETTINGS, ...JSON.parse(stored) };
    }
  } catch {
    // Ignore parse errors
  }
  return DEFAULT_GLOBAL_SETTINGS;
}

export function saveGlobalSettings(settings: GlobalSettings): void {
  try {
    localStorage.setItem(GLOBAL_SETTINGS_KEY, JSON.stringify(settings));
  } catch {
    // Ignore storage errors
  }
}
