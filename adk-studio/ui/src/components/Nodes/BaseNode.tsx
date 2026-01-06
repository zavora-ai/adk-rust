import { Handle, Position } from '@xyflow/react';
import { ReactNode } from 'react';

interface BaseNodeProps {
  label: string;
  icon: string;
  isActive: boolean;
  isSelected: boolean;
  color: { bg: string; border: string };
  children?: ReactNode;
}

export function BaseNode({ 
  label, 
  icon, 
  isActive, 
  isSelected, 
  color,
  children,
}: BaseNodeProps) {
  return (
    <div 
      className="relative rounded-lg min-w-[160px] transition-all duration-200"
      style={{ 
        background: color.bg,
        border: `2px solid ${isActive ? '#4ade80' : color.border}`,
        boxShadow: isActive 
          ? '0 0 20px rgba(74, 222, 128, 0.5)' 
          : isSelected 
            ? '0 0 0 2px #3b82f6' 
            : 'none',
      }}
    >
      <Handle type="target" position={Position.Top} id="top" className="!bg-gray-400" />
      <Handle type="target" position={Position.Left} id="left" className="!bg-gray-400" />
      
      <div className="px-3 py-2">
        <div className="flex items-center gap-2 font-medium text-white text-sm">
          <span>{icon}</span>
          <span>{label}</span>
          {isActive && <span className="ml-auto text-green-400 animate-pulse">●</span>}
        </div>
        {children && <div className="mt-2 border-t border-white/20 pt-2">{children}</div>}
      </div>
      
      <Handle type="source" position={Position.Bottom} id="bottom" className="!bg-gray-400" />
      <Handle type="source" position={Position.Right} id="right" className="!bg-gray-400" />
    </div>
  );
}
