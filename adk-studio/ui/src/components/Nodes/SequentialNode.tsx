import { memo } from 'react';
import { Handle, Position } from '@xyflow/react';

const toolIcons: Record<string, string> = {
  google_search: '🔍', browser: '🌐', mcp: '🔌', function: '⚡', file: '📁', code: '💻',
};
const getToolIcon = (t: string) => toolIcons[Object.keys(toolIcons).find(k => t.includes(k)) || ''] || '🔧';

interface Props {
  data: {
    label: string;
    subAgents?: string[];
    subAgentTools?: Record<string, string[]>;
    activeSubAgent?: string;
    isActive?: boolean;
  };
  selected?: boolean;
}

export const SequentialNode = memo(({ data, selected }: Props) => {
  const isActive = data.isActive || false;
  
  return (
    <div 
      className="rounded-lg min-w-[160px] transition-all duration-200"
      style={{ 
        background: '#1e3a5f',
        border: `2px solid ${isActive ? '#4ade80' : '#60a5fa'}`,
        boxShadow: isActive ? '0 0 20px rgba(74, 222, 128, 0.5)' : selected ? '0 0 0 2px #3b82f6' : 'none',
      }}
    >
      <Handle type="target" position={Position.Top} id="top" className="!bg-gray-400" />
      <Handle type="target" position={Position.Left} id="left" className="!bg-gray-400" />
      
      <div className="px-3 py-2">
        <div className="flex items-center gap-2 font-medium text-white text-sm">
          <span>⛓</span>
          <span>{data.label}</span>
          {isActive && <span className="ml-auto text-green-400 animate-pulse">●</span>}
        </div>
        <div className="mt-2 border-t border-white/20 pt-2 space-y-1">
          {(data.subAgents || []).map((sub, idx) => {
            const tools = data.subAgentTools?.[sub] || [];
            return (
              <div 
                key={sub}
                className={`px-2 py-1 rounded text-xs transition-all ${
                  data.activeSubAgent === sub 
                    ? 'bg-green-900 ring-1 ring-green-400 text-green-200' 
                    : 'bg-gray-800 text-gray-300'
                }`}
              >
                <div>{data.activeSubAgent === sub ? '⚡' : `${idx + 1}.`} {sub}</div>
                {tools.length > 0 && (
                  <div className="mt-0.5 text-gray-400">{tools.map(t => getToolIcon(t)).join(' ')}</div>
                )}
              </div>
            );
          })}
        </div>
      </div>
      
      <Handle type="source" position={Position.Bottom} id="bottom" className="!bg-gray-400" />
      <Handle type="source" position={Position.Right} id="right" className="!bg-gray-400" />
    </div>
  );
});

SequentialNode.displayName = 'SequentialNode';
