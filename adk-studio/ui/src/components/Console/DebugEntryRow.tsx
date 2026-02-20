/**
 * DebugEntryRow renders a single debug entry in the Debug Console tab.
 *
 * Collapsed view: timestamp | level badge | category icon | agent | summary | copy button
 * Expanded view: adds a <pre> block with JSON.stringify(detail, null, 2)
 *
 * @see Requirements 2.1, 2.2, 2.3, 2.4, 3.1, 3.2, 3.3, 3.4, 3.5, 8.3
 */

import { useCallback } from 'react';
import type { DebugEntry } from '../../types/debug';
import { CATEGORY_ICONS, LEVEL_COLORS } from '../../types/debug';

export interface DebugEntryRowProps {
  entry: DebugEntry;
  isExpanded: boolean;
  onToggle: () => void;
}

/**
 * Format a timestamp (ms since epoch) as HH:MM:SS.mmm.
 * @see Requirement 2.2
 */
export function formatDebugTimestamp(ts: number): string {
  const d = new Date(ts);
  const hh = String(d.getHours()).padStart(2, '0');
  const mm = String(d.getMinutes()).padStart(2, '0');
  const ss = String(d.getSeconds()).padStart(2, '0');
  const ms = String(d.getMilliseconds()).padStart(3, '0');
  return `${hh}:${mm}:${ss}.${ms}`;
}

export function DebugEntryRow({ entry, isExpanded, onToggle }: DebugEntryRowProps) {
  const handleCopy = useCallback(
    (e: React.MouseEvent) => {
      e.stopPropagation();
      const json = JSON.stringify(entry.detail, null, 2);
      navigator.clipboard.writeText(json).catch((err) => {
        console.warn('Failed to copy debug entry to clipboard:', err);
      });
    },
    [entry.detail],
  );

  const levelColor = LEVEL_COLORS[entry.level];
  const categoryIcon = CATEGORY_ICONS[entry.category];

  return (
    <div
      style={{ borderBottom: '1px solid var(--border-default)' }}
    >
      {/* Collapsed row — always visible */}
      <div
        onClick={onToggle}
        role="button"
        tabIndex={0}
        onKeyDown={(e) => { if (e.key === 'Enter' || e.key === ' ') onToggle(); }}
        style={{
          display: 'flex',
          alignItems: 'center',
          gap: '8px',
          padding: '4px 8px',
          cursor: 'pointer',
          fontSize: '12px',
          lineHeight: '20px',
          backgroundColor: isExpanded ? 'var(--surface-hover)' : 'transparent',
        }}
      >
        {/* Timestamp */}
        <span
          style={{
            fontFamily: 'monospace',
            color: 'var(--text-muted)',
            flexShrink: 0,
            whiteSpace: 'nowrap',
          }}
        >
          {formatDebugTimestamp(entry.timestamp)}
        </span>

        {/* Level badge */}
        <span
          style={{
            padding: '0 5px',
            borderRadius: '3px',
            fontSize: '10px',
            fontWeight: 600,
            textTransform: 'uppercase',
            color: '#fff',
            backgroundColor: levelColor,
            flexShrink: 0,
          }}
        >
          {entry.level}
        </span>

        {/* Category icon */}
        <span style={{ flexShrink: 0 }} title={entry.category}>
          {categoryIcon}
        </span>

        {/* Agent name */}
        <span
          style={{
            color: 'var(--accent-primary)',
            fontWeight: 500,
            flexShrink: 0,
            maxWidth: '120px',
            overflow: 'hidden',
            textOverflow: 'ellipsis',
            whiteSpace: 'nowrap',
          }}
          title={entry.agent}
        >
          {entry.agent}
        </span>

        {/* Summary */}
        <span
          style={{
            color: 'var(--text-primary)',
            flex: 1,
            overflow: 'hidden',
            textOverflow: 'ellipsis',
            whiteSpace: 'nowrap',
          }}
          title={entry.summary}
        >
          {entry.summary}
        </span>

        {/* Copy button */}
        <button
          onClick={handleCopy}
          title="Copy detail as JSON"
          style={{
            background: 'none',
            border: 'none',
            cursor: 'pointer',
            padding: '2px 4px',
            fontSize: '12px',
            color: 'var(--text-muted)',
            flexShrink: 0,
          }}
        >
          📋
        </button>
      </div>

      {/* Expanded detail view */}
      {isExpanded && (
        <pre
          style={{
            margin: 0,
            padding: '8px 12px 8px 28px',
            fontFamily: 'monospace',
            fontSize: '11px',
            lineHeight: '1.5',
            overflowX: 'auto',
            backgroundColor: 'var(--surface-sunken, var(--surface-panel))',
            color: 'var(--text-secondary)',
            whiteSpace: 'pre-wrap',
            wordBreak: 'break-word',
          }}
        >
          {JSON.stringify(entry.detail, null, 2)}
        </pre>
      )}
    </div>
  );
}
