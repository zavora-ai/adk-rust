import { useState } from 'react';
import ReactMarkdown from 'react-markdown';

interface ThinkingSectionProps {
  thinking: string;
}

/**
 * Collapsible section for displaying model thinking/reasoning traces.
 * Renders collapsed by default with a toggle to expand and show full thinking text.
 *
 * @see thinking-traces Requirements 7.1, 7.2, 7.3, 7.4
 */
export function ThinkingSection({ thinking }: ThinkingSectionProps) {
  const [expanded, setExpanded] = useState(false);

  return (
    <div
      className="my-1 rounded border-l-4"
      style={{
        backgroundColor: 'var(--bg-secondary)',
        borderLeftColor: 'var(--text-muted)',
      }}
    >
      <button
        onClick={() => setExpanded(!expanded)}
        className="w-full flex items-center gap-1 px-2 py-1 text-xs cursor-pointer"
        style={{ color: 'var(--text-muted)' }}
        aria-expanded={expanded}
        aria-label={expanded ? 'Collapse thinking' : 'Expand thinking'}
      >
        <span>💭</span>
        <span className="italic">Thinking</span>
        <span>{expanded ? '▼' : '▶'}</span>
      </button>
      {expanded && (
        <div
          className="px-3 pb-2 text-xs prose prose-sm max-w-none"
          style={{ color: 'var(--text-secondary)' }}
        >
          <ReactMarkdown>{thinking}</ReactMarkdown>
        </div>
      )}
    </div>
  );
}
