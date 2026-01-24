/**
 * ConsoleFilters component for client-side event filtering.
 * 
 * @see Requirements 13.3: Client-side filtered views
 * 
 * Provides filter buttons for:
 * - all: Show all events
 * - model: Show model/agent events (agent_start, agent_end, model)
 * - tool: Show tool events (tool_call, tool_result)
 * - session: Show session events (user, done, error)
 */

export type EventFilter = 'all' | 'model' | 'tool' | 'session';

interface ConsoleFiltersProps {
  /** Currently active filter */
  currentFilter: EventFilter;
  /** Callback when filter changes */
  onFilterChange: (filter: EventFilter) => void;
  /** Whether auto-scroll is enabled */
  autoScroll?: boolean;
  /** Callback when auto-scroll changes */
  onAutoScrollChange?: (enabled: boolean) => void;
}

interface FilterButton {
  id: EventFilter;
  label: string;
  icon: string;
  title: string;
}

const FILTER_BUTTONS: FilterButton[] = [
  { id: 'all', label: 'All', icon: '📋', title: 'Show all events' },
  { id: 'model', label: 'Model', icon: '💬', title: 'Show model and agent events' },
  { id: 'tool', label: 'Tool', icon: '🔧', title: 'Show tool calls and results' },
  { id: 'session', label: 'Session', icon: '👤', title: 'Show session events (user input, done, errors)' },
];

export function ConsoleFilters({ 
  currentFilter, 
  onFilterChange,
  autoScroll = true,
  onAutoScrollChange,
}: ConsoleFiltersProps) {
  return (
    <div 
      className="flex items-center justify-between px-2 py-1 border-b"
      style={{ borderColor: 'var(--border-default)' }}
    >
      <div className="flex gap-1">
        {FILTER_BUTTONS.map((btn) => (
          <button
            key={btn.id}
            onClick={() => onFilterChange(btn.id)}
            className="px-2 py-0.5 rounded text-xs flex items-center gap-1 transition-colors"
            style={{
              backgroundColor: currentFilter === btn.id ? 'var(--accent-primary)' : 'transparent',
              color: currentFilter === btn.id ? 'white' : 'var(--text-secondary)',
            }}
            title={btn.title}
          >
            <span>{btn.icon}</span>
            <span>{btn.label}</span>
          </button>
        ))}
      </div>
      
      {/* Auto-scroll toggle */}
      {onAutoScrollChange && (
        <button
          onClick={() => onAutoScrollChange(!autoScroll)}
          className="px-2 py-0.5 rounded text-xs flex items-center gap-1"
          style={{
            backgroundColor: autoScroll ? 'var(--accent-primary)' : 'transparent',
            color: autoScroll ? 'white' : 'var(--text-secondary)',
          }}
          title={autoScroll ? 'Auto-scroll enabled' : 'Auto-scroll disabled'}
        >
          <span>{autoScroll ? '📜' : '⏸️'}</span>
          <span>Auto-scroll</span>
        </button>
      )}
    </div>
  );
}
