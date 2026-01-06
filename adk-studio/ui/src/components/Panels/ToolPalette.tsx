import { DragEvent } from 'react';

const TOOL_TYPES = [
  { type: 'function', label: 'Function Tool', icon: 'ƒ', configurable: true },
  { type: 'mcp', label: 'MCP Tool', icon: '🔌', configurable: true },
  { type: 'browser', label: 'Browser Tool', icon: '🌐', configurable: true },
  { type: 'exit_loop', label: 'Exit Loop', icon: '⏹', configurable: true },
  { type: 'google_search', label: 'Google Search', icon: '🔍', configurable: true },
  { type: 'load_artifact', label: 'Load Artifact', icon: '📦', configurable: true },
];

interface Props {
  selectedNodeId: string | null;
  agentTools: string[];
  onAdd: (type: string) => void;
  onRemove: (type: string) => void;
}

export function ToolPalette({ selectedNodeId, agentTools, onAdd, onRemove }: Props) {
  const onDragStart = (e: DragEvent, type: string) => {
    e.dataTransfer.setData('text/plain', `tool:${type}`);
    e.dataTransfer.effectAllowed = 'copy';
  };

  return (
    <div>
      <h3 className="font-semibold mb-2">Tools</h3>
      <div className="space-y-1">
        {TOOL_TYPES.map(({ type, label, icon }) => {
          const isMultiTool = type === 'function' || type === 'mcp';
          const isAdded = isMultiTool
            ? agentTools.some(t => t.startsWith(type))
            : agentTools.includes(type);
          const toolCount = isMultiTool ? agentTools.filter(t => t.startsWith(type)).length : 0;

          return (
            <div
              key={type}
              draggable
              onDragStart={(e) => onDragStart(e, type)}
              className={`p-1.5 rounded text-xs cursor-grab flex items-center gap-2 ${isAdded ? 'bg-green-800 hover:bg-green-700' : 'bg-gray-700 hover:bg-gray-600'
                } ${!selectedNodeId ? 'opacity-50' : ''}`}
              onClick={() => {
                if (!selectedNodeId) return;
                if (isMultiTool || !isAdded) onAdd(type);
                else onRemove(type);
              }}
            >
              <span>{icon}</span>
              <span className="text-xs">{label}</span>
              {isMultiTool && toolCount > 0 && <span className="ml-auto text-xs bg-blue-600 px-1 rounded">{toolCount}</span>}
              {!isMultiTool && isAdded && <span className="ml-auto text-xs">✓</span>}
            </div>
          );
        })}
      </div>
    </div>
  );
}

export { TOOL_TYPES };
