import { memo } from 'react';
import { Handle, Position } from '@xyflow/react';

interface Props {
  data: {
    label: string;
    routes?: Array<{ condition: string; target: string }>;
    activeRoute?: string;
    isActive?: boolean;
  };
  selected?: boolean;
}

export const RouterNode = memo(({ data, selected }: Props) => {
  const isActive = data.isActive || false;
  
  return (
    <div 
      className="rounded-lg min-w-[160px] transition-all duration-200"
      style={{ 
        background: '#5f3d1e',
        border: `2px solid ${isActive ? '#4ade80' : '#f59e0b'}`,
        boxShadow: isActive ? '0 0 20px rgba(74, 222, 128, 0.5)' : selected ? '0 0 0 2px #3b82f6' : 'none',
      }}
    >
      <Handle type="target" position={Position.Top} id="top" className="!bg-gray-400" />
      <Handle type="target" position={Position.Left} id="left" className="!bg-gray-400" />
      
      <div className="px-3 py-2">
        <div className="flex items-center gap-2 font-medium text-white text-sm">
          <span>🔀</span>
          <span>{data.label}</span>
          {isActive && <span className="ml-auto text-green-400 animate-pulse">●</span>}
        </div>
        <div className="mt-2 border-t border-white/20 pt-2 space-y-1">
          {(data.routes || []).map(route => (
            <div 
              key={route.condition}
              className={`px-2 py-1 rounded text-xs flex items-center gap-1 ${
                data.activeRoute === route.condition 
                  ? 'bg-green-900 ring-1 ring-green-400 text-green-200' 
                  : 'bg-gray-800 text-gray-300'
              }`}
            >
              <span className="text-yellow-400">{route.condition}</span>
              <span className="text-gray-500">→</span>
              <span>{route.target}</span>
            </div>
          ))}
        </div>
      </div>
      
      <Handle type="source" position={Position.Bottom} id="bottom" className="!bg-gray-400" />
      <Handle type="source" position={Position.Right} id="right" className="!bg-gray-400" />
    </div>
  );
});

RouterNode.displayName = 'RouterNode';
