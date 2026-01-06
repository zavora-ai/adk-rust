import { memo } from 'react';
import { Handle, Position } from '@xyflow/react';

interface LlmNodeData {
  label: string;
  model?: string;
  tools?: string[];
  isActive?: boolean;
  thought?: string;
}

interface Props {
  data: LlmNodeData;
  selected?: boolean;
}

const toolIcons: Record<string, string> = {
  google_search: '🔍', browser: '🌐', mcp: '🔌', function: '⚡', file: '📁', code: '💻', default: '🔧',
};

const getToolIcon = (tool: string) => toolIcons[Object.keys(toolIcons).find(k => tool.toLowerCase().includes(k)) || 'default'];

export const LlmAgentNode = memo(({ data, selected }: Props) => {
  const isActive = data.isActive || false;
  const hasThought = !!data.thought;
  
  return (
    <div 
      className="rounded-lg min-w-[180px] transition-all duration-200"
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
          <span>🤖</span>
          <span>{data.label}</span>
          <span className="ml-auto flex items-center gap-1">
            {hasThought && <span className="text-blue-400 animate-pulse" title="Thinking...">💭</span>}
            {isActive && <span className="text-green-400 animate-pulse">●</span>}
          </span>
        </div>
        
        <div className="mt-2 pt-2 border-t border-white/20">
          <div className="text-xs text-blue-300 flex items-center gap-1.5">
            <span>🧠</span>
            <span>{data.model || 'gemini-2.0-flash'}</span>
          </div>
        </div>
        
        {data.tools && data.tools.length > 0 && (
          <div className="mt-2 pt-2 border-t border-white/10 space-y-1">
            {data.tools.map(t => (
              <div key={t} className="flex items-center gap-1.5 text-xs text-gray-300">
                <span>{getToolIcon(t)}</span>
                <span>{t}</span>
              </div>
            ))}
          </div>
        )}
      </div>
      
      <Handle type="source" position={Position.Bottom} id="bottom" className="!bg-gray-400" />
      <Handle type="source" position={Position.Right} id="right" className="!bg-gray-400" />
    </div>
  );
});

LlmAgentNode.displayName = 'LlmAgentNode';
