// Debug Console Tab types and constants
// Requirements: 2.1, 2.3, 2.4, 4.2, 5.2, 10.2

export type DebugLevel = 'error' | 'warn' | 'info' | 'debug' | 'trace';
export type DebugCategory = 'request' | 'response' | 'error' | 'state_change' | 'tool_call' | 'tool_result' | 'lifecycle' | 'trigger' | 'action';

export interface DebugEntry {
  id: string;
  timestamp: number;
  level: DebugLevel;
  category: DebugCategory;
  agent: string;
  summary: string;
  detail: unknown;
}

export type DebugCategoryFilter = 'all' | 'request' | 'response' | 'tool' | 'state' | 'error' | 'trigger' | 'action';

export const DEBUG_LEVEL_PRIORITY: Record<DebugLevel, number> = {
  error: 0,
  warn: 1,
  info: 2,
  debug: 3,
  trace: 4,
};

export const MAX_DEBUG_ENTRIES = 500;

export const CATEGORY_ICONS: Record<DebugCategory, string> = {
  request: '📤',
  response: '📥',
  error: '❌',
  state_change: '🔄',
  tool_call: '🔧',
  tool_result: '✅',
  lifecycle: '⚙️',
  trigger: '🎯',
  action: '⚡',
};

export const LEVEL_COLORS: Record<DebugLevel, string> = {
  error: 'var(--accent-error)',
  warn: 'var(--accent-warning)',
  info: 'var(--accent-primary)',
  debug: 'var(--text-secondary)',
  trace: 'var(--text-muted)',
};

/** Maps each DebugCategoryFilter to the DebugCategory values it matches. */
export const CATEGORY_FILTER_MAP: Record<DebugCategoryFilter, DebugCategory[] | '*'> = {
  all: '*',
  request: ['request'],
  response: ['response'],
  tool: ['tool_call', 'tool_result'],
  state: ['state_change'],
  error: ['error'],
  trigger: ['trigger'],
  action: ['action'],
};

/** Returns true if the entry's category matches the given category filter. */
export function matchesCategoryFilter(category: DebugCategory, filter: DebugCategoryFilter): boolean {
  const mapping = CATEGORY_FILTER_MAP[filter];
  return mapping === '*' || mapping.includes(category);
}
