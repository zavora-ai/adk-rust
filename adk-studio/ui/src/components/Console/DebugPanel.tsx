/**
 * DebugPanel renders the main debug console view with filtering, search,
 * auto-scroll, copy-all, and clear functionality.
 *
 * @see Requirements 4.2, 5.2, 5.3, 6.2, 6.3, 7.1, 7.4, 8.1, 8.2
 */

import { useState, useRef, useEffect, useCallback, useMemo } from 'react';
import type { DebugEntry, DebugLevel, DebugCategoryFilter } from '../../types/debug';
import { DEBUG_LEVEL_PRIORITY, matchesCategoryFilter, CATEGORY_ICONS } from '../../types/debug';
import { DebugFilters } from './DebugFilters';
import { DebugEntryRow, formatDebugTimestamp } from './DebugEntryRow';

export interface DebugPanelProps {
  debugEntries: DebugEntry[];
  clearDebugEntries: () => void;
  autoScroll: boolean;
  onAutoScrollChange: (enabled: boolean) => void;
}

/**
 * Filter debug entries through the combined pipeline: level → category → search.
 *
 * Exported for property-based testing (Property 1: Combined Filter Pipeline).
 *
 * @see Requirements 4.2, 5.2, 5.3, 6.2, 6.3
 */
export function filterDebugEntries(
  entries: DebugEntry[],
  levelFilter: DebugLevel,
  categoryFilter: DebugCategoryFilter,
  searchQuery: string,
): DebugEntry[] {
  const maxPriority = DEBUG_LEVEL_PRIORITY[levelFilter];
  const query = searchQuery.trim().toLowerCase();

  return entries.filter((entry) => {
    // 1. Level filter: include entries at or above the selected level
    if (DEBUG_LEVEL_PRIORITY[entry.level] > maxPriority) return false;

    // 2. Category filter
    if (!matchesCategoryFilter(entry.category, categoryFilter)) return false;

    // 3. Search filter (case-insensitive on summary, agent, or stringified detail)
    if (query) {
      const summaryMatch = entry.summary.toLowerCase().includes(query);
      const agentMatch = entry.agent.toLowerCase().includes(query);
      const detailMatch = JSON.stringify(entry.detail).toLowerCase().includes(query);
      if (!summaryMatch && !agentMatch && !detailMatch) return false;
    }

    return true;
  });
}

/**
 * Format a list of debug entries as human-readable text for clipboard copy.
 */
function formatEntriesAsText(entries: DebugEntry[]): string {
  return entries
    .map((e) => {
      const ts = formatDebugTimestamp(e.timestamp);
      const icon = CATEGORY_ICONS[e.category];
      return `[${ts}] ${e.level.toUpperCase()} ${icon} [${e.agent}] ${e.summary}`;
    })
    .join('\n');
}

export function DebugPanel({
  debugEntries,
  clearDebugEntries,
  autoScroll,
  onAutoScrollChange,
}: DebugPanelProps) {
  const [levelFilter, setLevelFilter] = useState<DebugLevel>('debug');
  const [categoryFilter, setCategoryFilter] = useState<DebugCategoryFilter>('all');
  const [searchQuery, setSearchQuery] = useState('');
  const [expandedIds, setExpandedIds] = useState<Set<string>>(new Set());

  const scrollContainerRef = useRef<HTMLDivElement>(null);
  const bottomRef = useRef<HTMLDivElement>(null);

  // Filtered entries via the combined pipeline
  const filteredEntries = useMemo(
    () => filterDebugEntries(debugEntries, levelFilter, categoryFilter, searchQuery),
    [debugEntries, levelFilter, categoryFilter, searchQuery],
  );

  // Auto-scroll to bottom when enabled and new entries arrive (Requirement 7.1, 7.4)
  useEffect(() => {
    if (autoScroll) {
      bottomRef.current?.scrollIntoView({ behavior: 'smooth' });
    }
  }, [debugEntries.length, autoScroll]);

  // Toggle expand/collapse for a single entry
  const handleToggle = useCallback((id: string) => {
    setExpandedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
  }, []);

  // Copy all visible (filtered) entries as text (Requirement 8.2)
  const handleCopyAll = useCallback(() => {
    const text = formatEntriesAsText(filteredEntries);
    navigator.clipboard.writeText(text).catch((err) => {
      console.warn('Failed to copy debug entries to clipboard:', err);
    });
  }, [filteredEntries]);

  // Toggle auto-scroll
  const handleAutoScrollToggle = useCallback(() => {
    onAutoScrollChange(!autoScroll);
  }, [autoScroll, onAutoScrollChange]);

  const hasEntries = debugEntries.length > 0;
  const hasMatches = filteredEntries.length > 0;

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%', overflow: 'hidden' }}>
      <DebugFilters
        levelFilter={levelFilter}
        onLevelChange={setLevelFilter}
        categoryFilter={categoryFilter}
        onCategoryChange={setCategoryFilter}
        searchQuery={searchQuery}
        onSearchChange={setSearchQuery}
        autoScroll={autoScroll}
        onAutoScrollToggle={handleAutoScrollToggle}
        onCopyAll={handleCopyAll}
        onClear={clearDebugEntries}
      />

      <div
        ref={scrollContainerRef}
        style={{
          flex: 1,
          overflowY: 'auto',
          fontSize: '12px',
        }}
      >
        {/* Empty state */}
        {!hasEntries && (
          <div
            style={{
              display: 'flex',
              alignItems: 'center',
              justifyContent: 'center',
              height: '100%',
              color: 'var(--text-muted)',
              fontSize: '13px',
              padding: '24px',
              textAlign: 'center',
            }}
          >
            No debug entries yet. Run your agent with debug mode enabled to see traces.
          </div>
        )}

        {hasEntries && !hasMatches && (
          <div
            style={{
              display: 'flex',
              alignItems: 'center',
              justifyContent: 'center',
              height: '100%',
              color: 'var(--text-muted)',
              fontSize: '13px',
              padding: '24px',
              textAlign: 'center',
            }}
          >
            No entries match the current filters.
          </div>
        )}

        {filteredEntries.map((entry) => (
          <DebugEntryRow
            key={entry.id}
            entry={entry}
            isExpanded={expandedIds.has(entry.id)}
            onToggle={() => handleToggle(entry.id)}
          />
        ))}

        <div ref={bottomRef} />
      </div>
    </div>
  );
}
