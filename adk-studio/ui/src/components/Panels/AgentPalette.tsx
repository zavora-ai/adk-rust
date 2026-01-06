import { DragEvent } from 'react';

const AGENT_TYPES = [
  { type: 'llm', label: 'LLM Agent' },
  { type: 'sequential', label: 'Sequential Agent' },
  { type: 'loop', label: 'Loop Agent' },
  { type: 'parallel', label: 'Parallel Agent' },
  { type: 'router', label: 'Router Agent' },
];

interface Props {
  onDragStart: (e: DragEvent, type: string) => void;
  onCreate: (type: string) => void;
}

export function AgentPalette({ onDragStart, onCreate }: Props) {
  return (
    <div>
      <h3 className="font-semibold mb-2">Agents</h3>
      <div className="space-y-1">
        {AGENT_TYPES.map(({ type, label }) => (
          <div
            key={type}
            draggable
            onDragStart={(e) => onDragStart(e, type)}
            onClick={() => onCreate(type)}
            className="p-1.5 bg-studio-accent rounded text-xs cursor-grab hover:bg-studio-highlight"
          >
            ⊕ {label}
          </div>
        ))}
      </div>
    </div>
  );
}
