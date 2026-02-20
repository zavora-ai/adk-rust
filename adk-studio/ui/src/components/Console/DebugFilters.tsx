/**
 * DebugFilters renders the filter bar for the Debug Console tab.
 *
 * - Log level dropdown (error, warn, info, debug, trace) defaulting to "debug"
 * - Category filter buttons (all, request, response, tool, state, error)
 * - Search input field
 * - Auto-scroll toggle button
 * - Copy all button
 * - Clear button
 *
 * @see Requirements 4.1, 4.3, 5.1, 6.1, 7.3, 8.1, 8.2
 */

import type { DebugLevel, DebugCategoryFilter } from '../../types/debug';

export interface DebugFiltersProps {
  levelFilter: DebugLevel;
  onLevelChange: (level: DebugLevel) => void;
  categoryFilter: DebugCategoryFilter;
  onCategoryChange: (category: DebugCategoryFilter) => void;
  searchQuery: string;
  onSearchChange: (query: string) => void;
  autoScroll: boolean;
  onAutoScrollToggle: () => void;
  onCopyAll: () => void;
  onClear: () => void;
}

const LOG_LEVELS: DebugLevel[] = ['error', 'warn', 'info', 'debug', 'trace'];

interface CategoryButton {
  id: DebugCategoryFilter;
  label: string;
}

const CATEGORY_BUTTONS: CategoryButton[] = [
  { id: 'all', label: 'All' },
  { id: 'request', label: 'Request' },
  { id: 'response', label: 'Response' },
  { id: 'tool', label: 'Tool' },
  { id: 'trigger', label: 'Trigger' },
  { id: 'action', label: 'Action' },
  { id: 'state', label: 'State' },
  { id: 'error', label: 'Error' },
];

export function DebugFilters({
  levelFilter,
  onLevelChange,
  categoryFilter,
  onCategoryChange,
  searchQuery,
  onSearchChange,
  autoScroll,
  onAutoScrollToggle,
  onCopyAll,
  onClear,
}: DebugFiltersProps) {
  return (
    <div
      style={{
        display: 'flex',
        alignItems: 'center',
        gap: '8px',
        padding: '4px 8px',
        borderBottom: '1px solid var(--border-default)',
        flexWrap: 'wrap',
      }}
    >
      {/* Log level dropdown — Requirement 4.1, 4.3 */}
      <select
        value={levelFilter}
        onChange={(e) => onLevelChange(e.target.value as DebugLevel)}
        title="Filter by log level"
        style={{
          padding: '2px 4px',
          borderRadius: '3px',
          border: '1px solid var(--border-default)',
          backgroundColor: 'var(--surface-panel)',
          color: 'var(--text-primary)',
          fontSize: '11px',
          cursor: 'pointer',
          outline: 'none',
        }}
      >
        {LOG_LEVELS.map((level) => (
          <option key={level} value={level}>
            {level.toUpperCase()}
          </option>
        ))}
      </select>

      {/* Category filter buttons — Requirement 5.1 */}
      <div style={{ display: 'flex', gap: '2px' }}>
        {CATEGORY_BUTTONS.map((btn) => (
          <button
            key={btn.id}
            onClick={() => onCategoryChange(btn.id)}
            title={`Filter: ${btn.label}`}
            style={{
              padding: '2px 6px',
              borderRadius: '3px',
              border: 'none',
              fontSize: '11px',
              cursor: 'pointer',
              backgroundColor:
                categoryFilter === btn.id ? 'var(--accent-primary)' : 'transparent',
              color: categoryFilter === btn.id ? '#fff' : 'var(--text-secondary)',
            }}
          >
            {btn.label}
          </button>
        ))}
      </div>

      {/* Search input — Requirement 6.1 */}
      <input
        type="text"
        value={searchQuery}
        onChange={(e) => onSearchChange(e.target.value)}
        placeholder="Search debug entries..."
        style={{
          flex: 1,
          minWidth: '100px',
          padding: '2px 6px',
          borderRadius: '3px',
          border: '1px solid var(--border-default)',
          backgroundColor: 'var(--surface-panel)',
          color: 'var(--text-primary)',
          fontSize: '11px',
          outline: 'none',
        }}
      />

      {/* Auto-scroll toggle — Requirement 7.3 */}
      <button
        onClick={onAutoScrollToggle}
        title={autoScroll ? 'Auto-scroll enabled' : 'Auto-scroll disabled'}
        style={{
          padding: '2px 6px',
          borderRadius: '3px',
          border: 'none',
          fontSize: '11px',
          cursor: 'pointer',
          backgroundColor: autoScroll ? 'var(--accent-primary)' : 'transparent',
          color: autoScroll ? '#fff' : 'var(--text-secondary)',
        }}
      >
        {autoScroll ? '⬇️' : '⏸'}
      </button>

      {/* Copy all — Requirement 8.2 */}
      <button
        onClick={onCopyAll}
        title="Copy all visible entries"
        style={{
          padding: '2px 6px',
          borderRadius: '3px',
          border: 'none',
          fontSize: '11px',
          cursor: 'pointer',
          backgroundColor: 'transparent',
          color: 'var(--text-secondary)',
        }}
      >
        📋
      </button>

      {/* Clear — Requirement 8.1 */}
      <button
        onClick={onClear}
        title="Clear all debug entries"
        style={{
          padding: '2px 6px',
          borderRadius: '3px',
          border: 'none',
          fontSize: '11px',
          cursor: 'pointer',
          backgroundColor: 'transparent',
          color: 'var(--text-secondary)',
        }}
      >
        🗑️
      </button>
    </div>
  );
}
